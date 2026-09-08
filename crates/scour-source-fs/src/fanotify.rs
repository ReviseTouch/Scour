//! Watching a whole filesystem with one mark.
//!
//! inotify costs a watch a directory. On the machine this was built for that is
//! 296,711 directories under the home alone against a limit of 268,593, and
//! 152,529 for the NTFS volume — which is why that volume was not being watched
//! at all. `FAN_MARK_FILESYSTEM` costs **one mark a superblock**, measured at
//! 0.005 ms and no measurable kernel memory, against 26.4 ms and 3.1 MB for
//! 30,000 inode marks that still miss every directory created afterwards.
//!
//! Three things about this mechanism decide the shape of everything below, and
//! each was measured rather than assumed:
//!
//! * **Events merge.** A file created, written, closed and deleted between two
//!   reads arrives as a single event with `mask = 0x30a` — CREATE, MODIFY,
//!   CLOSE_WRITE and DELETE together, in no order. Nothing in the event says
//!   whether the file is there now. So the mask is never read as a description;
//!   the path is looked at instead, which is the contract [`crate::watch`]
//!   already states.
//! * **The cost is the wake, not the event.** Per event the reader spends
//!   11.4 µs at 20 events a second and 1.1 µs at 230,000 — the difference is a
//!   fixed cost amortised over the batch. Draining on a timer instead of on
//!   arrival took a 2,120-event-a-second load from 0.84% of a core to 0.078%,
//!   and idle from 143 wakes a second to 5. Hence [`WINDOW`].
//! * **A file handle is not a path.** The event carries the *parent directory*
//!   as an opaque handle plus the entry name. Turning that into a path takes
//!   either `open_by_handle_at` — which needs `CAP_DAC_READ_SEARCH`, measured
//!   EPERM as an ordinary user — or a map this process builds itself. It builds
//!   it: the handle's bytes carry the inode number, and the walk already stats
//!   every directory.
//!
//! What this module does *not* do is decide anything. Every path it resolves
//! goes through the same [`crate::watch::look`] as an inotify event, so the
//! rules, the metadata read and the "a new directory is a subtree" step are
//! shared and cannot drift apart.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ignore::WalkBuilder;
use scour_core::{Change, ChangeSink, Result, ScanOptions, SourceId, WatchHandle};

use crate::path;
use crate::rules::Rules;
use crate::scan::FsSource;

/// Where the privileged helper leaves the already-marked descriptor.
///
/// The marks need `CAP_SYS_ADMIN` and this process does not have it and must
/// not: measured, an unprivileged process that is handed the descriptor can
/// read every event on both disks while `fanotify_mark` and `fanotify_init`
/// both return EPERM to it. So privilege is spent once, before this process
/// starts, and what crosses over is a descriptor that cannot be used to widen
/// the watch.
const FD_ENV: &str = "SCOUR_FANOTIFY_FD";

/// How long to let events pile up before draining them.
///
/// Two hundred milliseconds, and the number is the whole reason this is
/// affordable. Measured against a steady load, wakes a second and the share of
/// one core they cost:
///
/// | window | ~212 events/s | ~2120 events/s |
/// |---|---|---|
/// | 0 ms | 143 wakes, 0.274% | 1417 wakes, 0.838% |
/// | 50 ms | 17 wakes, 0.073% | 20 wakes, 0.157% |
/// | **200 ms** | **5 wakes, 0.030%** | **5 wakes, 0.078%** |
/// | 500 ms | 2 wakes, 0.019% | 2 wakes, 0.062% |
/// | 1000 ms | 1 wake, 0.014% | 1 wake, **0.075%** |
///
/// Past 500 ms it stops paying and at 1000 ms it reverses: the batch reaches
/// 2,600 events and stops fitting in cache. Two hundred is where the idle cost
/// crosses under a thirtieth of a percent, which is the requirement, and the
/// price is that the index is at most this far behind — well under the time it
/// takes to type a query.
const WINDOW: Duration = Duration::from_millis(200);

/// Read buffer.
///
/// 256 KiB against 64 KiB carries 3.98× as many events a call and is 1.04×
/// faster, so the syscall is not the cost and this is sized for the batch
/// rather than for throughput. At 71 bytes an event it holds about 3,600.
const BUF: usize = 256 * 1024;

/// The most events one window will collect before it stops reading and says it
/// lost track.
///
/// **The kernel queue is deliberately unlimited and this vector was not.**
/// `scour-watch` opens the group with `FAN_UNLIMITED_QUEUE` — its own comment
/// costs that at "about 95 bytes an event, so a million unread events is 90 MB
/// of kernel memory", and calls the 4.6-million-a-second drain rate "what makes
/// that a bound rather than a risk". That reasoning covers the kernel's side of
/// the queue and stops there: draining it is what moves those events *into this
/// process*, and the read loop below kept going until the queue was empty or
/// two seconds had passed. Two seconds at the measured drain rate is about nine
/// million [`Seen`] values, each with an owned name — several hundred megabytes
/// of anonymous memory, reached by nothing more unusual than deleting a large
/// tree.
///
/// So the deadline is a *time* bound and this is the *memory* one. Past it the
/// window is abandoned exactly as a kernel overflow is: `lost` is set, and the
/// subtree is walked again. That is the module's existing contract — an event
/// is a hint to look again, never a description of what happened — so nothing
/// downstream needs to learn a new case.
///
/// A quarter of a million is far above any ordinary burst (a kernel build, a
/// `git clone`, an unpacked archive) and is about 20 MB of `Seen` while it is
/// held. The ceiling is approached rather than hit exactly: [`parse`] empties a
/// whole buffer before the check, so a window may end up to one buffer — about
/// 3,600 events — past it.
const MAX_SEEN: usize = 262_144;

/// The capacity one window may leave behind for the next.
///
/// `clear` keeps capacity, so without this a single burst sets the reader's
/// allocation for the life of the process: the peak becomes the floor, and the
/// service ends an afternoon holding memory that one `rm -rf` asked for. Ten
/// thousand events is about 800 KB and covers an ordinary window without
/// reallocating; anything past it is given back when the window ends.
const KEEP_SEEN: usize = 10_000;

// Not in `libc` at the time of writing, and their numeric values are kernel
// ABI, so they are written out rather than derived.
const FAN_REPORT_DIR_FID: u32 = 0x0000_0400;
const FAN_REPORT_NAME: u32 = 0x0000_0800;
/// What the helper has to have opened the group with, or nothing below works.
///
/// Both bits, not either: `DIR_FID` puts the parent directory's handle in the
/// event and `NAME` puts the entry name next to it, and this module needs the
/// pair to build a path. Without them the events still arrive and still parse —
/// they just carry the object's own handle and no name, so every path would be
/// wrong rather than absent, which is the failure worth spending a check on.
const REQUIRED_FLAGS: u32 = FAN_REPORT_DIR_FID | FAN_REPORT_NAME;
const FAN_Q_OVERFLOW: u64 = 0x0000_4000;
const FAN_EVENT_INFO_TYPE_DFID_NAME: u8 = 2;
const FAN_ONDIR: u64 = 0x4000_0000;
const FAN_CREATE: u64 = 0x0000_0100;
const FAN_MOVED_TO: u64 = 0x0000_0080;
const FAN_CLOSE_WRITE: u64 = 0x0000_0008;

/// What identifies a directory across a reboot, a remount and a rename.
///
/// The device number rather than btrfs's subvolume id, because that is what
/// `stat` hands the walk. The event carries the subvolume id instead, and the
/// two are matched through a table with one row a mounted subvolume — seven on
/// this machine — rather than one a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DirKey {
    dev: u64,
    ino: u64,
}

/// One name in [`NameArena`].
///
/// The field order keeps this at eight bytes. A component that
/// `symlink_metadata` accepted on Linux is at most `NAME_MAX` and a root's
/// whole path at most `PATH_MAX`, both far below `u16::MAX`; a chunk is one
/// MiB, and 65,536 chunks would already mean 64 GiB of directory names.
#[derive(Debug, Clone, Copy)]
struct NameSlot {
    start: u32,
    chunk: u16,
    len: u16,
}

impl NameSlot {
    /// Filler for the climb's stack buffer; names no bytes.
    const EMPTY: NameSlot = NameSlot {
        start: 0,
        chunk: 0,
        len: 0,
    };
}

const _: () = assert!(std::mem::size_of::<NameSlot>() == 8);

/// One directory: what it is called, and which directory it is called that in.
///
/// **Twelve bytes a directory instead of its whole path.** The representation
/// this replaced packed every directory's *full* path into an arena, which was
/// itself a measured win — 2026-08-15, retained heap −28% against one `String`
/// a directory — and then stopped scaling with the thing it was storing. Both
/// live sources here hold 603,950 directories between them, and `find -xdev
/// -type d` puts the average full path at 118.1 B and 98.7 B against average
/// *basenames* of 15.6 B and 12.7 B: an ancestor's name is written down once
/// per descendant, ten and a half times over on average. That is 68.2 MB of
/// path against 8.9 MB of name, about 70% of the daemon's anonymous memory and
/// all of the 96 MB this process had in swap. Both maps together weighed
/// **87.82 MB** of retained heap and now weigh **39.58 MB**.
///
/// So a name is stored once and a path is spelled out from the chain when an
/// event needs it. The second thing that falls out of it is the rename: a
/// directory keeps `(dev, ino)` when it moves but every descendant's *spelled*
/// path changes, so the arena had to rewrite all of them — two passes over the
/// whole map, **under the reader's lock**, measured at 17.4 ms to move 781
/// directories and 45.6 ms to move a root of 423,384. Here the descendants
/// point at this node, so moving it is one assignment: 2.3 µs and 480 ns.
///
/// What it costs is the lookup, and the probe is the place that argues about
/// it: 114.3 ns → 371.6 ns for the path an event needs, and 3.2× to 4.3×
/// across four sittings — the climb touches about twenty cache lines where the
/// arena touched two, so it is the arm that suffers under load. See
/// `docs/MEASUREMENTS.md`, 2026-09-06.
#[derive(Debug, Clone, Copy)]
struct Node {
    parent: u32,
    name: NameSlot,
}

const _: () = assert!(std::mem::size_of::<Node>() == 12);

/// [`Node::parent`] for a directory that is nobody's child here: a walk root,
/// or a directory whose parent this map never learned. It holds its whole path
/// as its name, which is what makes the two cases the same case.
const NO_PARENT: u32 = u32::MAX;

/// How many components [`DirMap::components`] keeps on the stack.
///
/// Six times the measured average of 10.6, which is 512 bytes to zero on a
/// lookup rather than the 16 KB an array that could not overflow would need.
/// Deeper than this is not refused, it spills — see [`DirMap::components`].
const INLINE_DEPTH: usize = 64;

/// How far a climb will follow parent links before calling the chain broken.
///
/// This is **not** a depth limit: `PATH_MAX` is 4096 bytes and a component
/// costs at least two of them, so no real path reaches this. It is a *cycle*
/// guard. A chain that never reaches a root would spin the reader thread
/// forever, and the only honest answer to a broken chain is the one an unknown
/// handle already gets — no path at all, rather than a partial one.
const MAX_DEPTH: usize = 4096;

/// Names in coarse allocations rather than one allocator object a directory.
///
/// A single growing `Vec` would briefly need both the old and new allocation
/// whenever it grows. Fixed-size chunks keep peak memory bounded, waste less
/// than one chunk at the end, and never move bytes that an existing slot names.
#[derive(Debug, Default)]
struct NameArena {
    chunks: Vec<Vec<u8>>,
}

impl NameArena {
    const CHUNK: usize = 1024 * 1024;

    fn push(&mut self, name: &str) -> NameSlot {
        let len = name.len();
        let needs_chunk = self
            .chunks
            .last()
            .is_none_or(|chunk| chunk.capacity() - chunk.len() < len);
        if needs_chunk {
            self.chunks.push(Vec::with_capacity(Self::CHUNK.max(len)));
        }
        let chunk = self.chunks.len() - 1;
        let bytes = &mut self.chunks[chunk];
        let start = bytes.len();
        bytes.extend_from_slice(name.as_bytes());
        NameSlot {
            start: start.try_into().expect("name arena chunk exceeds 4 GiB"),
            chunk: chunk.try_into().expect("name arena exceeds 65,536 chunks"),
            len: len.try_into().expect("a stat-able Linux name fits in u16"),
        }
    }

    /// The bytes back as text.
    ///
    /// Checked rather than `from_utf8_unchecked`, which is what the full-path
    /// arena needed: everything written here came from a `&str`, so the check
    /// can only pass, and it is 15 bytes a component against a lookup whose
    /// cost is the cache misses of the climb. The probe measures it; buying an
    /// `unsafe` block back would have to be argued from that number.
    fn get(&self, slot: NameSlot) -> &str {
        let start = slot.start as usize;
        let end = start + slot.len as usize;
        self.chunks
            .get(slot.chunk as usize)
            .and_then(|chunk| chunk.get(start..end))
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .unwrap_or_default()
    }

    /// What the names actually cost, for the probes and for the tests that
    /// hold a rename to writing one name rather than one path a descendant.
    #[cfg(test)]
    fn used_bytes(&self) -> usize {
        self.chunks.iter().map(Vec::len).sum()
    }
}

/// A path's parent and its last component, `/`-separated.
///
/// `/a/b` is `("/a", "b")`, `/a` is `("/", "a")`, and `/` — or anything else
/// ending in a separator, or holding none — has no last component and is kept
/// whole. Splitting the string rather than the `Path` is deliberate: this is
/// the same string [`crate::path::from_path`] produced, and a byte that is not
/// valid UTF-8 is encoded into a private-use character there, never into
/// something that could be read as a separator.
fn split_name(path: &str) -> Option<(&str, &str)> {
    let at = path.rfind('/')?;
    if at + 1 == path.len() {
        return None;
    }
    Some((if at == 0 { "/" } else { &path[..at] }, &path[at + 1..]))
}

/// What identifies the directory at this path, if it is one.
///
/// One `statx`, and it is what buys the parent link. The walk pays it on its
/// own threads — 0.55 µs warm, against a directory the walker has just read, so
/// the parent is in the dentry cache by construction — and [`DirMap::learn`]
/// pays it once per created directory. The alternative was a second index from
/// path to node, which is the memory this whole representation is removing.
fn dir_key(at: &std::path::Path) -> Option<DirKey> {
    use std::os::unix::fs::MetadataExt;
    if at.as_os_str().is_empty() {
        return None;
    }
    let md = std::fs::symlink_metadata(at).ok()?;
    md.is_dir().then(|| DirKey {
        dev: md.dev(),
        ino: md.ino(),
    })
}

/// The inode number a file handle carries, by filesystem.
///
/// The layouts are not documented as stable and are read here anyway, because
/// the alternative — `open_by_handle_at` — needs a capability this process
/// deliberately does not hold. Each was confirmed against `stat` on 5,000 real
/// directories, and the pair `(type, length)` is what distinguishes them:
/// tmpfs and ntfs3 both report type 1 with the fields the other way round, so
/// the type alone is not enough and reading it as though it were puts a
/// generation number where an inode belongs.
fn handle_ino(fh_type: i32, bytes: &[u8]) -> Option<u64> {
    let le64 = |at: usize| -> Option<u64> {
        bytes
            .get(at..at + 8)?
            .try_into()
            .ok()
            .map(u64::from_le_bytes)
    };
    let le32 = |at: usize| -> Option<u64> {
        bytes
            .get(at..at + 4)?
            .try_into()
            .ok()
            .map(|b| u32::from_le_bytes(b) as u64)
    };
    match (fh_type, bytes.len()) {
        // btrfs: ino u64 @0, subvolume id u64 @8, generation u32 @16.
        (0x4d..=0x4f, 20..) => le64(0),
        // tmpfs: generation u32 @0, ino u64 @4. Twelve bytes, and the reason
        // the length is part of the match.
        (1, 12) => le64(4),
        // FILEID_INO32_GEN, which is what ntfs3 and most others use: ino u32
        // @0, generation u32 @4.
        (1, 8) => le32(0),
        _ => None,
    }
}

/// Directory identity to path, for the directories this source covers.
///
/// Held in memory rather than in the index. The index used to carry an inode a
/// row and it was removed for a measured reason — 19 MB of a 200 MB index, on
/// 2.09 million rows, to answer a question the path already answered. What is
/// needed here is narrower: one key and one path per directory.
///
/// The path is not stored. [`Node`] says why: names are held once each and a
/// path is spelled out by climbing to a root. The ignored
/// `directory_map_memory_probe` test measures the complete allocator footprint
/// and the lookup cost of that climb, at live-machine scale, against the
/// full-path arena this replaced.
///
/// **What the graph assumes is that the map holds whole trees.** The walk
/// records every directory it descends through and prunes excluded ones
/// entirely, so a directory in this map has its parent in it too — up to the
/// root, which is where the chain stops. A directory inserted without its
/// ancestors still resolves to exactly the right path (it keeps its whole path
/// and parents nothing), it simply does not move when an ancestor is renamed —
/// and neither did it before, because it had no ancestor to be renamed.
#[derive(Debug, Default)]
struct DirMap {
    by_key: HashMap<DirKey, u32>,
    nodes: Vec<Node>,
    names: NameArena,
    /// Directories that are only somebody's parent.
    ///
    /// A walk root's parent is outside the tree — `/home` for a source that
    /// watches `/home/hasan` — and the parallel walk can hand a directory over
    /// before the batch its parent is sitting in. Both need a node to hang a
    /// child off, and neither may be answerable by [`DirMap::path_of`]:
    /// resolving an event in `/home` for this source would name a path no scan
    /// of it ever produces, and the reader would deliver a change outside its
    /// own roots. So they live here instead of in `by_key`, and the moment the
    /// walk reaches one for real it moves across, keeping its node and its
    /// children with it.
    outside: HashMap<DirKey, u32>,
    /// Device candidates an event's inode is looked up against.
    ///
    /// The event gives an inode but not a device — btrfs reports the
    /// superblock's fsid on every event whatever subvolume it came from — so the
    /// inode is offered to each device this source actually covers.
    ///
    /// There are only a handful, but deriving them from `by_key` is not cheap:
    /// on this machine it copied and sorted 103,000 or 152,000 keys every time a
    /// directory was created. Keep the unique list as entries arrive instead.
    devices: Vec<u64>,
}

/// How many directories a walker thread gathers before handing them over.
///
/// The same shape as the scan's, and for the same reason: a send per directory
/// on a bounded channel drained by one thread is a queue that is always full,
/// which cost that walk 9.5 context switches a file until it was batched.
const DIR_BATCH: usize = 512;

/// How many batches may be in the air. This is the whole of the extra memory
/// the parallel walk costs over the stack walk it replaced — threads times
/// batch, not a quarter of a million paths held twice.
const DIR_IN_FLIGHT: usize = 64;

/// One directory as a walker thread hands it over: what it is, what its parent
/// is, and where it is. The parent travels with it because the walker is the
/// one place that can name it cheaply — it has just read that directory.
type Walked = (DirKey, Option<DirKey>, String);

/// One walker thread's outgoing buffer of directories.
struct DirBatch {
    tx: crossbeam_channel::Sender<Vec<Walked>>,
    buf: Vec<Walked>,
}

impl DirBatch {
    fn new(tx: crossbeam_channel::Sender<Vec<Walked>>) -> DirBatch {
        DirBatch {
            tx,
            buf: Vec::with_capacity(DIR_BATCH),
        }
    }

    /// Returns false once the far end is gone, which is a walk to abandon.
    fn push(&mut self, key: DirKey, parent: Option<DirKey>, path: String) -> bool {
        self.buf.push((key, parent, path));
        self.buf.len() < DIR_BATCH || self.flush()
    }

    fn flush(&mut self) -> bool {
        if self.buf.is_empty() {
            return true;
        }
        let full = std::mem::replace(&mut self.buf, Vec::with_capacity(DIR_BATCH));
        self.tx.send(full).is_ok()
    }
}

/// The tail of a thread's last batch.
///
/// `ignore` gives a visitor no way to say it has finished, but it does drop the
/// box when the thread ends — so this is where the remainder goes. Without it
/// the map loses up to [`DIR_BATCH`] directories a thread, and a directory
/// missing from the map is one whose every event resolves to nothing.
impl Drop for DirBatch {
    fn drop(&mut self) {
        self.flush();
    }
}

impl DirMap {
    /// Walk the roots and record what each directory is.
    ///
    /// The order matters and was measured: with the mark placed **after** the
    /// walk, ten files created during it were lost; with the mark placed first,
    /// none were, because everything that happens during the walk is sitting in
    /// the queue when it finishes. The mark here is always already in place —
    /// the helper set it before this process existed — so the walk is safe by
    /// construction.
    ///
    /// **This is a second pass over the tree the scan also walks, and it is not
    /// the one a reviewer looked for.** The recorded double walk belongs to
    /// inotify, which holds one watch per directory and must enumerate the tree
    /// to install them — 342,000 of them, 15.1 s. None of that runs here:
    /// [`crate::watch::start`] returns from [`try_start`] before `notify` is
    /// ever built. What this backend needs the tree for is different and
    /// unavoidable: an event names its parent directory by file handle, not by
    /// path, so without this map every event resolves to nothing. Resolving a
    /// handle on demand instead is `open_by_handle_at`, which wants
    /// `CAP_DAC_READ_SEARCH` — the one capability this process is careful not
    /// to have.
    ///
    /// So the pass stays. What it does not have to stay is **single-threaded**:
    /// it reads exactly the directories the scan reads, and the scan reads them
    /// on several threads. Measured on this machine, warm, alternating in one
    /// process: `/mnt/depo`'s 152,530 directories went from 1.03 s to 0.19 s,
    /// and `/home/hasan`'s 103,524 from 0.80 s to 0.16 s. Cold, `/mnt/depo`
    /// went from 15.7 s to 4.7 s. See `docs/MEASUREMENTS.md`.
    ///
    /// The walk's own thread count is decided here rather than taken from
    /// [`crate::fs::Medium`], and the difference is the consumer. The scan is
    /// held to two threads because the index behind it cannot take more —
    /// "a faster consumer would make this number four again", says that
    /// comment. This walk's consumer is a hash-map insert at 213 ns, so the
    /// disk is the only thing left to saturate.
    fn build(roots: &[std::path::PathBuf], rules: &Rules, threads: usize) -> DirMap {
        let mut map = DirMap::default();
        let Some((first, rest)) = roots.split_first() else {
            return map;
        };

        let mut builder = WalkBuilder::new(first);
        for r in rest {
            builder.add(r);
        }
        builder
            // `ignore` is used here as a concurrent directory walk and nothing
            // else, exactly as in the scan: a rule about what belongs in an
            // index is not a rule about what a watcher can resolve.
            .standard_filters(false)
            // The stack walk that came before this filtered no name, so nor
            // does this: a change under `~/.config` is a change.
            .hidden(false)
            // `symlink_metadata` and `DirEntry::file_type` both refused to
            // descend a link, and the map must agree with the walk about that
            // or an event resolves to a path no scan ever produces.
            .follow_links(false)
            .same_file_system(false)
            .threads(threads);

        // Drained while the walk runs rather than collected and merged
        // afterwards. The whole point of the packed arena is that a quarter of
        // a million separate `String`s is 13 MB nobody needs; holding them all
        // once more in a joining vector would put that peak straight back, on a
        // machine where the invariant is `RssAnon + VmSwap`.
        let (tx, rx) = crossbeam_channel::bounded::<Vec<Walked>>(DIR_IN_FLIGHT);
        std::thread::scope(|scope| {
            let walker_tx = tx.clone();
            scope.spawn(move || {
                builder.build_parallel().run(|| {
                    let mut batch = DirBatch::new(walker_tx.clone());
                    // On the first entry rather than here, because `ignore`
                    // builds the visitor on the thread that spawns the workers
                    // and this would otherwise make that one polite instead.
                    let mut polite = false;
                    Box::new(move |result| {
                        if !polite {
                            polite = true;
                            // The same nice value and idle I/O class the scan's
                            // walkers take. Eight threads reading a disk at
                            // start-up is worth having only if it yields to the
                            // session coming up beside it, and neither costs
                            // anything on an idle machine.
                            crate::scan::stand_aside();
                        }
                        let Ok(de) = result else {
                            // A directory that cannot be read resolves no
                            // events, which is what the stack walk's silent
                            // `continue` also meant.
                            return ignore::WalkState::Continue;
                        };
                        if !de.file_type().is_some_and(|t| t.is_dir()) {
                            return ignore::WalkState::Continue;
                        }
                        let text = path::from_path(de.path());
                        if rules.excludes_path(&text) {
                            // Pruned, not merely skipped: the stack walk never
                            // pushed an excluded directory's children either,
                            // and a watcher that resolves what the scan
                            // discards is how a `cargo test` under an unscanned
                            // build tree took a query from 8 ms to 13 seconds.
                            return ignore::WalkState::Skip;
                        }
                        let Ok(md) = de.metadata() else {
                            return ignore::WalkState::Continue;
                        };
                        use std::os::unix::fs::MetadataExt;
                        let key = DirKey {
                            dev: md.dev(),
                            ino: md.ino(),
                        };
                        // Named here rather than at the far end, because this
                        // thread has just read the parent and the far end is a
                        // single thread that would pay every one of these
                        // `statx` calls in series. See [`dir_key`].
                        let parent = de.path().parent().and_then(dir_key);
                        if batch.push(key, parent, text) {
                            ignore::WalkState::Continue
                        } else {
                            ignore::WalkState::Quit
                        }
                    })
                });
            });
            // The senders are dropped when the visitors are, which is what ends
            // the loop below — so this clone has to go with them or it never
            // ends. Each visitor's own tail is flushed by [`DirBatch`]'s `Drop`.
            drop(tx);
            for batch in rx {
                for (key, parent, text) in batch {
                    map.insert_key(key, parent, &text);
                }
            }
        });
        map
    }

    fn insert(&mut self, md: &std::fs::Metadata, parent: Option<DirKey>, path: &str) {
        use std::os::unix::fs::MetadataExt;
        self.insert_key(
            DirKey {
                dev: md.dev(),
                ino: md.ino(),
            },
            parent,
            path,
        );
    }

    /// Record what a directory is called and where it hangs.
    ///
    /// `parent` is the directory this one is *in*, as its caller already knows
    /// it — the walker `statx`ed it on its own thread, [`learn`] on the one
    /// event that needed it. `None`, or a parent this map has never heard of,
    /// is not an error and not a guess: the directory keeps its whole path and
    /// resolves exactly as it always did.
    ///
    /// [`learn`]: DirMap::learn
    fn insert_key(&mut self, key: DirKey, parent: Option<DirKey>, path: &str) {
        if !self.devices.contains(&key.dev) {
            self.devices.push(key.dev);
        }
        // A directory that is its own parent would be a chain that never
        // reaches a root — every event below it unresolvable, and only the
        // depth cap between the reader and a spin.
        let (parent_id, name) = self.place_under(parent.filter(|p| *p != key), path);

        if let Some(id) = self.by_key.get(&key).copied() {
            // The ordinary case is that nothing has changed: `learn` runs on
            // every created directory and most of them are already here.
            if self.nodes[id as usize].parent == parent_id && self.path_matches(id, path) {
                return;
            }
            // A directory keeps `(dev, ino)` across a rename, and so does every
            // descendant — which is why the whole subtree moves with this one
            // assignment rather than being rewritten path by path.
            let slot = self.names.push(name);
            self.nodes[id as usize] = Node {
                parent: parent_id,
                name: slot,
            };
            return;
        }

        // Met as somebody's parent before the walk reached it. Take that node
        // over — its children already point at it — rather than leaving two
        // nodes for one directory and a subtree hanging off the wrong one.
        let id = match self.outside.remove(&key) {
            Some(id) => {
                let slot = self.names.push(name);
                self.nodes[id as usize] = Node {
                    parent: parent_id,
                    name: slot,
                };
                id
            }
            None => self.add_node(parent_id, name),
        };
        self.by_key.insert(key, id);
    }

    /// The node a path hangs off, and the name it is known by there.
    ///
    /// The `&str` comes back out of `path` rather than out of the arena, so the
    /// caller can hand it straight to [`NameArena::push`] while still holding
    /// the map mutably.
    fn place_under<'p>(&mut self, parent: Option<DirKey>, path: &'p str) -> (u32, &'p str) {
        let (Some(parent), Some((parent_path, name))) = (parent, split_name(path)) else {
            return (NO_PARENT, path);
        };
        if let Some(&id) = self.by_key.get(&parent) {
            return (id, name);
        }
        if let Some(&id) = self.outside.get(&parent) {
            return (id, name);
        }
        let id = self.add_node(NO_PARENT, parent_path);
        self.outside.insert(parent, id);
        (id, name)
    }

    fn add_node(&mut self, parent: u32, name: &str) -> u32 {
        let slot = self.names.push(name);
        let id: u32 = self
            .nodes
            .len()
            .try_into()
            .expect("a directory map holds fewer than 4 billion directories");
        self.nodes.push(Node { parent, name: slot });
        id
    }

    /// Record a directory that appeared after the map was built.
    ///
    /// Without this a `mkdir` is seen once — the create in its parent, which
    /// does resolve — and then everything inside it arrives against a handle
    /// nothing knows, so a `git clone` would be a stream of unresolvable
    /// events. The walk that [`crate::watch::look`] already queues for a fresh
    /// directory covers the entries; this covers the *events*.
    fn learn(&mut self, path: &str) {
        if let Ok(md) = std::fs::symlink_metadata(path::to_path(path))
            && md.is_dir()
        {
            // The parent is in this map already — the event that produced this
            // path was resolved through it — but its *key* is not, and one
            // `statx` is what turns the path back into the link. Only paid when
            // a directory is created, which is why it is not on the event path.
            let parent = split_name(path).and_then(|(at, _)| dir_key(&path::to_path(at)));
            self.insert(&md, parent, path);
        }
    }

    /// The path of the directory this inode names, written into `out`.
    ///
    /// `out` is the caller's buffer rather than a returned `&str` because there
    /// is no longer a path anywhere to borrow — it is spelled out from the
    /// chain each time. The one production caller already copied the answer
    /// into a `String` of its own, so this moves that allocation rather than
    /// adding one, and lets the reader keep a single buffer for a whole window.
    ///
    /// False leaves `out` empty. A path that is missing a component names a
    /// different file, so a chain that does not reach a root is refused whole,
    /// exactly like a handle nothing recognises.
    fn path_of(&self, ino: u64, out: &mut String) -> bool {
        out.clear();
        let Some(id) = self.node_of(ino) else {
            return false;
        };
        self.write_path(id, out)
    }

    fn node_of(&self, ino: u64) -> Option<u32> {
        self.devices
            .iter()
            .find_map(|&dev| self.by_key.get(&DirKey { dev, ino }).copied())
    }

    /// Every component of this node's path, root first.
    ///
    /// The climb collects *names* rather than node numbers, because the caller
    /// would otherwise have to go back into `nodes` for each of them — two
    /// random accesses a component instead of one, on the one path an event
    /// takes. The measured average is 10.6 components; [`INLINE_DEPTH`] holds
    /// six times that on the stack and anything past it spills to a `Vec` that
    /// is never allocated otherwise. The spill is what keeps this **lossless**:
    /// a fixed cap would silently stop resolving events under a directory
    /// nested deeper than it, and `PATH_MAX` permits far deeper than any array
    /// worth zeroing on every lookup.
    ///
    /// False for a chain that does not end — a parent link pointing at nothing,
    /// or a cycle. Neither is reachable through a rename the kernel permits: it
    /// refuses to move a directory inside itself with `EINVAL`. See
    /// [`MAX_DEPTH`] for why the guard is here anyway.
    fn components(&self, id: u32, mut each: impl FnMut(&str)) -> bool {
        let mut stack = [NameSlot::EMPTY; INLINE_DEPTH];
        let mut spill: Vec<NameSlot> = Vec::new();
        let mut at = id;
        let mut depth = 0usize;
        loop {
            let Some(node) = self.nodes.get(at as usize) else {
                return false;
            };
            if depth < INLINE_DEPTH {
                stack[depth] = node.name;
            } else {
                spill.push(node.name);
            }
            depth += 1;
            if node.parent == NO_PARENT {
                break;
            }
            if depth == MAX_DEPTH {
                return false;
            }
            at = node.parent;
        }
        for &slot in spill.iter().rev() {
            each(self.names.get(slot));
        }
        for &slot in stack[..depth.min(INLINE_DEPTH)].iter().rev() {
            each(self.names.get(slot));
        }
        true
    }

    fn write_path(&self, id: u32, out: &mut String) -> bool {
        let complete = self.components(id, |name| {
            // A root keeps its whole path, and the only path that ends in a
            // separator is `/` itself — where a second one would spell `//x`.
            if !out.is_empty() && !out.ends_with('/') {
                out.push('/');
            }
            out.push_str(name);
        });
        if !complete {
            out.clear();
        }
        complete
    }

    /// Whether this node already spells exactly this path.
    ///
    /// Compared component by component against the caller's string rather than
    /// through a rebuilt one: this runs on every `learn`, which is every
    /// created directory, and most of those are already here and unchanged.
    fn path_matches(&self, id: u32, path: &str) -> bool {
        let mut at = 0usize;
        let mut same = true;
        let complete = self.components(id, |name| {
            if !same {
                return;
            }
            if at != 0 && !path[..at].ends_with('/') {
                if path.as_bytes().get(at) != Some(&b'/') {
                    same = false;
                    return;
                }
                at += 1;
            }
            if !path[at..].starts_with(name) {
                same = false;
                return;
            }
            at += name.len();
        });
        complete && same && at == path.len()
    }
}

/// Every filesystem with a block device behind it, as `mountinfo` sees it.
///
/// The key is the mount id — the first field, unique for the life of a mount —
/// rather than the device, because a disk that is unmounted and mounted again
/// is a *new* mount of the same device and the two have to be told apart: the
/// mark does not survive it. Measured, and it is the surprising half of the
/// pair: the mark does survive the unmount of the path it was placed through,
/// but a remount of the filesystem itself does not bring it back. A fresh
/// group saw the same write the old one had gone silent for.
fn mounts(text: &str) -> HashSet<(u32, String)> {
    let mut out = HashSet::new();
    for line in text.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let mut r = right.split_whitespace();
        let (Some(_fs), Some(source)) = (r.next(), r.next()) else {
            continue;
        };
        if !source.starts_with("/dev/") {
            continue;
        }
        let mut l = left.split_whitespace();
        let (Some(id), Some(at)) = (l.next(), l.nth(3)) else {
            continue;
        };
        if let Ok(id) = id.parse::<u32>() {
            out.insert((id, at.replace("\\040", " ")));
        }
    }
    out
}

/// Open `/proc/self/mountinfo` for watching rather than for reading.
///
/// `poll` on it returns `POLLERR | POLLPRI` when the mount table changes and
/// nothing otherwise — measured at 300 ms from the mount to the wake, with a
/// negative control that stayed quiet. It is the only way this process learns a
/// disk was plugged in, because a filesystem mark covers one superblock and a
/// new disk is a new superblock: the events simply never come, measured, and
/// nothing about that is visible from inside the fanotify descriptor.
fn mountinfo() -> Option<std::fs::File> {
    std::fs::File::open("/proc/self/mountinfo").ok()
}

/// One event, as far as this module cares: which directory, what name, and
/// whether the entry might be new.
struct Seen {
    parent_ino: u64,
    name: String,
    fresh: bool,
    is_dir: bool,
    /// The kernel says the content is final: a descriptor opened for writing
    /// was closed. For a mapping that is `munmap` rather than `close`, which
    /// is what takes a path off the revisit list without asking `/proc`
    /// anything. See [`crate::revisit::forget`].
    settled: bool,
}

/// Collect one window's events, and say whether anything was lost.
///
/// Split out of [`drain`] so the ceiling has something to be tested against:
/// the loop it came from could only be reached with a real fanotify group and a
/// real burst, which is why nothing had ever checked what it costs. `read` is
/// the one syscall it needs, and a test supplies its own.
///
/// Three things end a window, and only one of them is "the kernel had no more
/// to give": the other two are [`MAX_SEEN`] and the two-second deadline, and
/// both report themselves as lost rather than as a complete window. Reporting a
/// truncated window as complete is the one outcome that would be wrong — the
/// events that were not read still happened, and a sweep on that evidence
/// deletes rows that exist.
fn fill(buf: &mut [u8], seen: &mut Vec<Seen>, mut read: impl FnMut(&mut [u8]) -> isize) -> bool {
    seen.clear();
    // Before the window rather than after it: whatever the last burst asked for
    // is given back here, so the peak does not become the floor. See
    // [`KEEP_SEEN`].
    if seen.capacity() > KEEP_SEEN {
        seen.shrink_to(KEEP_SEEN);
    }
    let mut lost = false;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let n = read(buf);
        if n <= 0 {
            break;
        }
        if !parse(&buf[..n as usize], seen) {
            lost = true;
        }
        // **The memory bound**, checked before the time one because it is the
        // one a busy filesystem reaches first. See [`MAX_SEEN`].
        if seen.len() >= MAX_SEEN {
            lost = true;
            break;
        }
        // A burst larger than the buffer is read out in this loop rather
        // than left for the next window, but not forever: the drain rate
        // is 4.6 million events a second, so two seconds is far past any
        // real batch and exists only so a pathological producer cannot
        // hold the thread.
        if Instant::now() > deadline {
            lost = true;
            break;
        }
    }
    lost
}

/// Pull every event out of one buffer.
///
/// Returns `None` for the whole batch if the kernel reported that it dropped
/// events. The overflow record carries no information at all — `event_len`
/// equals `metadata_len`, the descriptor is `FAN_NOFD` and there is no info
/// record, all measured — so there is nothing to be selective about and the
/// only honest response is to say the subtree was lost.
fn parse(buf: &[u8], out: &mut Vec<Seen>) -> bool {
    let mut at = 0usize;
    let mut ok = true;
    while at + 24 <= buf.len() {
        let event_len = u32::from_le_bytes(buf[at..at + 4].try_into().unwrap()) as usize;
        if event_len < 24 || at + event_len > buf.len() {
            break;
        }
        let metadata_len = u16::from_le_bytes(buf[at + 6..at + 8].try_into().unwrap()) as usize;
        let mask = u64::from_le_bytes(buf[at + 8..at + 16].try_into().unwrap());
        if mask & FAN_Q_OVERFLOW != 0 {
            ok = false;
            at += event_len;
            continue;
        }
        let mut p = at + metadata_len.max(24);
        let end = at + event_len;
        while p + 4 <= end {
            let info_type = buf[p];
            let info_len = u16::from_le_bytes(buf[p + 2..p + 4].try_into().unwrap()) as usize;
            if info_len < 4 || p + info_len > end {
                break;
            }
            if info_type == FAN_EVENT_INFO_TYPE_DFID_NAME {
                // header 4 + fsid 8, then a `struct file_handle`: bytes u32,
                // type i32, then the handle, then the NUL-terminated name.
                let fh = p + 12;
                if fh + 8 <= p + info_len {
                    let hb = u32::from_le_bytes(buf[fh..fh + 4].try_into().unwrap()) as usize;
                    let ht = i32::from_le_bytes(buf[fh + 4..fh + 8].try_into().unwrap());
                    let body = fh + 8;
                    if body + hb <= p + info_len
                        && let Some(ino) = handle_ino(ht, &buf[body..body + hb])
                    {
                        let tail = &buf[body + hb..p + info_len];
                        let n = tail.iter().position(|&c| c == 0).unwrap_or(tail.len());
                        if n > 0
                            && let Ok(name) = std::str::from_utf8(&tail[..n])
                        {
                            out.push(Seen {
                                parent_ino: ino,
                                name: name.to_owned(),
                                fresh: mask & (FAN_CREATE | FAN_MOVED_TO) != 0,
                                is_dir: mask & FAN_ONDIR != 0,
                                settled: mask & FAN_CLOSE_WRITE != 0,
                            });
                        }
                    }
                }
                break;
            }
            p += info_len;
        }
        at += event_len;
    }
    ok
}

/// Take the descriptor the helper left, if it left one.
///
/// Absent is the ordinary case — nothing is installed, or the helper is not
/// being used — and it is not an error: [`try_start`] returns `None` and the
/// caller falls back to inotify.
fn inherited() -> Option<OwnedFd> {
    let raw: RawFd = std::env::var(FD_ENV).ok()?.trim().parse().ok()?;
    if raw < 0 {
        return None;
    }
    // **Check what it is before reading it.** A stale environment variable
    // pointing at some other open file would otherwise be read as a stream of
    // events, and a group opened without the two report flags would parse into
    // paths that are wrong rather than missing. `fdinfo` answers both: the
    // first line of a fanotify descriptor is `fanotify flags:%x event-flags:%x`
    // and nothing else produces it.
    let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{raw}")).ok()?;
    let flags = info.lines().find_map(|l| {
        let rest = l.strip_prefix("fanotify flags:")?;
        let hex = rest.split_whitespace().next()?;
        u32::from_str_radix(hex.trim_start_matches("0x"), 16).ok()
    })?;
    if flags & REQUIRED_FLAGS != REQUIRED_FLAGS {
        return None;
    }
    // SAFETY: the descriptor is open — `fdinfo` for it was just read — and the
    // environment variable is the helper's contract for handing over ownership,
    // so nothing else in this process holds it.
    Some(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// One source's share of the one reader.
///
/// **There is a single fanotify group and it cannot be split.** Two readers on
/// two descriptors for the same group share one queue, so each would take about
/// half the events and silently drop the other half. The first attempt handed
/// the descriptor to whichever source asked first and left the second on
/// inotify, which put `/mnt/depo` — 152,529 watches, the volume this whole
/// mechanism exists for — back to being unwatched. So one thread reads, and the
/// sources subscribe to it.
///
/// Routing is not a lookup table: each subscriber knows its own directories, so
/// the event's parent handle is offered to each in turn and the one that
/// recognises it owns the path. A source cannot claim another's tree because it
/// never walked it.
struct Sub {
    id: SourceId,
    real_modes: bool,
    rules: Arc<Rules>,
    sink: Arc<dyn ChangeSink>,
    roots: Vec<std::path::PathBuf>,
    map: DirMap,
    /// What [`walk_threads`] answered when the map was first built, kept so
    /// that rebuilding it in [`WatchHandle::retune`] does not have to ask a
    /// `FsSource` that is no longer in reach.
    threads: usize,
    /// Cleared when the handle is dropped. The entry stays in the list — an
    /// index has to keep meaning what it meant — and is simply skipped.
    live: Arc<AtomicBool>,
    uncovered: Arc<Mutex<Vec<String>>>,
}

/// The subscribers, and whether the reader has been started.
static SUBS: Mutex<Vec<Sub>> = Mutex::new(Vec::new());
static READER: std::sync::OnceLock<()> = std::sync::OnceLock::new();

impl std::fmt::Debug for Sub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sub")
            .field("id", &self.id)
            .field("roots", &self.roots)
            .field("dirs", &self.map.by_key.len())
            .finish()
    }
}

#[derive(Debug)]
struct FanWatch {
    /// Which subscription in [`SUBS`] is this one's, for [`WatchHandle::retune`].
    ///
    /// The list is shared by every source and the reader walks all of it, so a
    /// handle that wants to change its own rules has to be able to say which
    /// entry it is. Nothing else here needed to know.
    id: SourceId,
    live: Arc<AtomicBool>,
    /// Filesystems that appeared after the marks were set.
    ///
    /// They are not covered and cannot be from here: placing a mark needs
    /// `CAP_SYS_ADMIN` and this process deliberately has none, so the honest
    /// thing is to name them and let the layer above say so. That is what this
    /// side of [`WatchHandle`] is for, and `scourd` already prints it.
    uncovered: Arc<Mutex<Vec<String>>>,
}

impl WatchHandle for FanWatch {
    fn unwatched(&self) -> Vec<String> {
        self.uncovered.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// Nothing to do, and that is the point of this backend.
    ///
    /// inotify needs to be told about a subtree that appeared after the watch
    /// did, because it holds a watch per directory and a new directory has
    /// none. A filesystem mark covers the superblock, so a directory created a
    /// moment ago is already watched — measured: a file written inside a
    /// subvolume created after the mark produced its event like any other.
    fn cover(&self, _path: &str) {}

    /// Take the new rules, and rebuild the directory map behind them.
    ///
    /// **The map is not an optimisation here, it is how an event gets a name.**
    /// A fanotify event names its parent by file handle, and this backend
    /// answers that from a map it built by walking the roots *under the rules
    /// in force at the time*. So a rule that is switched off re-opens a subtree
    /// the map has never heard of, and every event in it would arrive
    /// unnameable and be dropped — live updates silently off for exactly the
    /// tree somebody just asked to see.
    ///
    /// Built before the lock is taken, because building it walks the disk —
    /// 0.38 s cold on the NTFS volume here, 0.7 s for both roots warm — and the
    /// reader takes that same lock for every event it delivers.
    fn retune(&self, opts: &ScanOptions) {
        let rules = Arc::new(Rules::from_options(opts));
        let Some((roots, threads)) = SUBS.lock().ok().and_then(|subs| {
            subs.iter()
                .find(|s| s.id == self.id)
                .map(|s| (s.roots.clone(), s.threads))
        }) else {
            return;
        };
        let map = DirMap::build(&roots, &rules, threads);
        if let Ok(mut subs) = SUBS.lock()
            && let Some(s) = subs.iter_mut().find(|s| s.id == self.id)
        {
            s.rules = rules;
            s.map = map;
        }
    }

    /// Leave the reader running.
    ///
    /// It serves every source, so one of them stopping is not a reason to take
    /// it down; the subscription goes quiet and the thread stays for the rest.
    /// The thread ends with the process, which is the only moment at which no
    /// source is left to serve.
    fn stop(self: Box<Self>) {
        self.live.store(false, Ordering::Relaxed);
    }
}

impl Drop for FanWatch {
    fn drop(&mut self) {
        self.live.store(false, Ordering::Relaxed);
    }
}

/// How many threads to walk the tree for the directory map on.
///
/// The scan's answer, asked the same way, because the question is about the
/// device and not about what the walk is for: a spinning disk turns every extra
/// reader into a seek whoever is asking. An explicit `scan.threads` is honoured
/// for the same reason it is honoured by the scan — somebody who set it meant
/// this disk, not that walk.
fn walk_threads(source: &FsSource, opts: &ScanOptions) -> usize {
    if opts.threads != 0 {
        return opts.threads;
    }
    source.medium().walk_threads(
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4),
    )
}

/// Subscribe to the one reader, or say this mechanism is not available here.
///
/// `None` means "not this one" rather than "no watching": the caller falls back
/// to inotify, which is why nothing in here panics or reports a failure for the
/// ordinary case of a machine where the helper was never installed.
pub fn try_start(
    source: &FsSource,
    opts: &ScanOptions,
    sink: Arc<dyn ChangeSink>,
) -> Option<Result<Box<dyn WatchHandle>>> {
    // The descriptor is taken once. After that the reader owns it and later
    // sources join the reader instead of trying to take it again — two owners
    // of one descriptor is a double close, and two readers of one group is half
    // the events each.
    let first = READER.get().is_none();
    let fd = if first { Some(inherited()?) } else { None };

    let roots: Vec<std::path::PathBuf> = source.roots().to_vec();
    let rules = Arc::new(Rules::from_options(opts));
    let threads = walk_threads(source, opts);
    let map = DirMap::build(&roots, &rules, threads);
    let live = Arc::new(AtomicBool::new(true));
    let uncovered: Arc<Mutex<Vec<String>>> = Arc::default();

    eprintln!(
        "scourd: watching with fanotify — {} director{} under {}",
        map.by_key.len(),
        if map.by_key.len() == 1 { "y" } else { "ies" },
        roots
            .first()
            .map(|r| path::from_path(r))
            .unwrap_or_default()
    );

    SUBS.lock().ok()?.push(Sub {
        id: source.source_id(),
        real_modes: source.real_modes(),
        rules,
        sink,
        roots,
        map,
        threads,
        live: Arc::clone(&live),
        uncovered: Arc::clone(&uncovered),
    });

    if let Some(fd) = fd {
        READER.get_or_init(|| {
            let _ = std::thread::Builder::new()
                .name("scour-fanotify".into())
                .spawn(move || drain(fd));
        });
    }

    Some(Ok(Box::new(FanWatch {
        id: source.source_id(),
        live,
        uncovered,
    })))
}

/// The reader. One thread, however many sources.
///
/// Wakes at most five times a second by construction: `poll` returns as soon as
/// the first event lands, and then the window runs before anything is read, so
/// what arrives during it is taken in one call. Everything the window collected
/// is reduced to distinct paths before a single `stat` is made — the same file
/// written a hundred times in the window is one look, which is where the second
/// saving is, and it does not show up in an event count.
fn drain(fd: OwnedFd) {
    let mut buf = vec![0u8; BUF];
    let mut seen: Vec<Seen> = Vec::new();
    // One buffer for every path the reader resolves, for the life of the
    // thread. The map no longer holds a path to borrow, so this is where the
    // spelled-out directory lands — and it is the allocation the caller used to
    // make per event with `to_owned`, moved rather than added.
    let mut dir = String::new();
    let raw = fd.as_raw_fd();

    // The mount table, watched beside the events rather than on a thread of its
    // own: one `poll` over two descriptors costs nothing extra and keeps the
    // whole watcher a single place that can be stopped.
    let mut mi = mountinfo();
    let mut known = mi
        .as_mut()
        .and_then(|f| {
            let mut s = String::new();
            f.read_to_string(&mut s).ok()?;
            Some(mounts(&s))
        })
        .unwrap_or_default();

    loop {
        let mut pfds = [
            libc::pollfd {
                fd: raw,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: mi.as_ref().map_or(-1, |f| f.as_raw_fd()),
                // `POLLERR` is not requested — it always arrives — but `POLLPRI`
                // is what the mount table signals a change with.
                events: libc::POLLPRI,
                revents: 0,
            },
        ];
        let pr = unsafe { libc::poll(pfds.as_mut_ptr(), 2, 500) };
        if pr <= 0 {
            continue;
        }

        if pfds[1].revents != 0 {
            // Re-read from the start, or `poll` keeps reporting the same change
            // and the loop spins.
            if let Some(f) = mi.as_mut() {
                let mut s = String::new();
                if f.seek(SeekFrom::Start(0)).is_ok() && f.read_to_string(&mut s).is_ok() {
                    let now = mounts(&s);
                    let fresh: Vec<String> =
                        now.difference(&known).map(|(_, at)| at.clone()).collect();
                    known = now;
                    if !fresh.is_empty() {
                        note_new_mounts(&fresh);
                    }
                }
            }
        }

        if pfds[0].revents & libc::POLLIN == 0 {
            continue;
        }
        std::thread::sleep(WINDOW);

        let lost = fill(&mut buf, &mut seen, |b| unsafe {
            libc::read(raw, b.as_mut_ptr().cast(), b.len())
        });

        let Ok(mut subs) = SUBS.lock() else { return };

        if lost {
            // Nothing in the overflow record says what was missed, so the
            // subtree is the only unit available — for everyone, because the
            // queue that overflowed was shared.
            for s in subs.iter().filter(|s| s.live.load(Ordering::Relaxed)) {
                for r in &s.roots {
                    s.sink.emit(Change::Rescan {
                        path: path::from_path(r),
                    });
                }
            }
            continue;
        }

        // Distinct paths a subscriber, keeping "this might be new" if any event
        // said so.
        let mut batch: HashMap<(usize, String), (bool, bool, bool)> = HashMap::new();
        for ev in seen.drain(..) {
            // The subscriber that walked this directory owns the path. An event
            // nobody recognises is **dropped, not escalated**: it is almost
            // always another source's tree or an excluded one, and answering it
            // with a rescan of every root turns ordinary traffic into a storm.
            // The case that would have justified escalating is covered
            // elsewhere — a btrfs snapshot arrives as a create *in a directory
            // that is known*, and a fresh directory already queues a walk.
            let Some(i) = subs.iter().enumerate().find_map(|(i, s)| {
                if !s.live.load(Ordering::Relaxed) {
                    return None;
                }
                s.map.path_of(ev.parent_ino, &mut dir).then_some(i)
            }) else {
                continue;
            };
            let full = if dir.ends_with('/') {
                format!("{dir}{}", ev.name)
            } else {
                format!("{dir}/{}", ev.name)
            };
            if subs[i].rules.excludes_path(&full) {
                continue;
            }
            let e = batch.entry((i, full)).or_insert((false, false, false));
            e.0 |= ev.fresh;
            e.1 |= ev.is_dir;
            e.2 |= ev.settled;
        }

        for ((i, full), (fresh, is_dir, settled)) in batch {
            let s = &mut subs[i];
            if fresh && is_dir {
                s.map.learn(&full);
            }
            let md = crate::watch::look(s.id, s.real_modes, &full, fresh, s.sink.as_ref());
            // A write through a mapping produces no event at all, so a path
            // that has just spoken is a path worth looking at again later —
            // unless the kernel has said the content is final.
            if settled {
                crate::revisit::forget(&full);
            } else {
                crate::revisit::note(&full, s.id, s.real_modes, &s.sink, md.as_ref());
            }
        }
    }
}

/// A filesystem that appeared after the marks were set.
///
/// Its contents can still be indexed — a walk reads what is there — but nothing
/// here can watch it: a new filesystem is a new superblock and a mark needs a
/// privilege this process does not have. So it goes to every subscriber that
/// wants it, as a walk and as an entry on the list `unwatched` carries.
fn note_new_mounts(fresh: &[String]) {
    let Ok(subs) = SUBS.lock() else { return };
    for s in subs.iter().filter(|s| s.live.load(Ordering::Relaxed)) {
        for at in fresh {
            if s.rules.excludes_path(at) {
                continue;
            }
            if let Ok(mut u) = s.uncovered.lock()
                && !u.contains(at)
            {
                u.push(at.clone());
            }
            s.sink.emit(Change::Rescan { path: at.clone() });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// [`DirMap::path_of`] as a test would rather read it.
    ///
    /// The reader keeps one buffer for the life of its thread; a test that
    /// checks a single directory should be able to say so in one line.
    fn resolved(map: &DirMap, ino: u64) -> Option<String> {
        let mut out = String::new();
        map.path_of(ino, &mut out).then_some(out)
    }

    /// The full-path arena this replaced, kept as the explicit control.
    ///
    /// The same reasoning as `build_serial` below: an A/B against a git
    /// revision nobody will rebuild is not an A/B, and the two representations
    /// have to be measured **in one process** or the reading is of two
    /// different states of an allocator. This is the 2026-08-15 packed arena
    /// verbatim — `HashMap<DirKey, PathSlot>` over one-MiB chunks of whole
    /// paths, with the two-pass rebase a directory rename cost — and nothing
    /// but `directory_map_memory_probe` and the rename-equivalence test below
    /// uses it.
    mod full_path {
        use super::DirKey;
        use std::collections::HashMap;

        #[derive(Debug, Clone, Copy)]
        pub(super) struct PathSlot {
            pub start: u32,
            pub chunk: u16,
            pub len: u16,
        }

        #[derive(Debug, Default)]
        pub(super) struct PathArena {
            pub chunks: Vec<Vec<u8>>,
            pub stale_bytes: usize,
        }

        impl PathArena {
            pub const CHUNK: usize = 1024 * 1024;

            pub fn push(&mut self, path: &str) -> PathSlot {
                self.push_parts(path, "")
            }

            pub fn push_parts(&mut self, prefix: &str, suffix: &str) -> PathSlot {
                let len = prefix.len() + suffix.len();
                let needs_chunk = self
                    .chunks
                    .last()
                    .is_none_or(|chunk| chunk.capacity() - chunk.len() < len);
                if needs_chunk {
                    self.chunks.push(Vec::with_capacity(Self::CHUNK.max(len)));
                }
                let chunk = self.chunks.len() - 1;
                let bytes = &mut self.chunks[chunk];
                let start = bytes.len();
                bytes.extend_from_slice(prefix.as_bytes());
                bytes.extend_from_slice(suffix.as_bytes());
                PathSlot {
                    start: start.try_into().expect("path arena chunk exceeds 4 GiB"),
                    chunk: chunk.try_into().expect("path arena exceeds 65,536 chunks"),
                    len: len.try_into().expect("a stat-able Linux path fits in u16"),
                }
            }

            pub fn push_rebased(
                &mut self,
                old: PathSlot,
                suffix_start: usize,
                prefix: &str,
            ) -> PathSlot {
                let source_chunk = old.chunk as usize;
                let source_start = old.start as usize + suffix_start;
                let source_end = old.start as usize + old.len as usize;
                let len = prefix.len() + source_end - source_start;
                let needs_chunk = self
                    .chunks
                    .last()
                    .is_none_or(|chunk| chunk.capacity() - chunk.len() < len);
                if needs_chunk {
                    self.chunks.push(Vec::with_capacity(Self::CHUNK.max(len)));
                }
                let target_chunk = self.chunks.len() - 1;
                let start = self.chunks[target_chunk].len();
                if source_chunk == target_chunk {
                    let bytes = &mut self.chunks[target_chunk];
                    bytes.extend_from_slice(prefix.as_bytes());
                    bytes.extend_from_within(source_start..source_end);
                } else {
                    let (sources, target) = self.chunks.split_at_mut(target_chunk);
                    let source = &sources[source_chunk][source_start..source_end];
                    let target = &mut target[0];
                    target.extend_from_slice(prefix.as_bytes());
                    target.extend_from_slice(source);
                }
                PathSlot {
                    start: start.try_into().expect("path arena chunk exceeds 4 GiB"),
                    chunk: target_chunk
                        .try_into()
                        .expect("path arena exceeds 65,536 chunks"),
                    len: len.try_into().expect("a stat-able Linux path fits in u16"),
                }
            }

            pub fn get(&self, slot: PathSlot) -> &str {
                let start = slot.start as usize;
                let end = start + slot.len as usize;
                // SAFETY: the arena's append methods copy valid UTF-8 from
                // `&str` or another valid arena range; slots name only the
                // exact appended bytes.
                unsafe {
                    std::str::from_utf8_unchecked(&self.chunks[slot.chunk as usize][start..end])
                }
            }

            pub fn used_bytes(&self) -> usize {
                self.chunks.iter().map(Vec::len).sum()
            }

            pub fn should_compact(&self) -> bool {
                Self::should_compact_at(self.stale_bytes, self.used_bytes())
            }

            pub fn should_compact_at(stale_bytes: usize, used_bytes: usize) -> bool {
                stale_bytes >= Self::CHUNK && stale_bytes >= used_bytes / 4
            }
        }

        pub(super) fn descendant_suffix_start(path: &str, prefix: &str) -> Option<usize> {
            if path == prefix {
                return Some(path.len());
            }
            if prefix == "/" && path.starts_with('/') {
                return Some(0);
            }
            path.strip_prefix(prefix)
                .filter(|suffix| suffix.starts_with('/'))
                .map(|_| prefix.len())
        }

        #[derive(Debug, Default)]
        pub(super) struct DirMap {
            pub by_key: HashMap<DirKey, PathSlot>,
            pub paths: PathArena,
            pub devices: Vec<u64>,
        }

        impl DirMap {
            pub fn insert_key(&mut self, key: DirKey, path: &str) {
                let dev = key.dev;
                if !self.devices.contains(&dev) {
                    self.devices.push(dev);
                }
                let Some(old) = self.by_key.get(&key).copied() else {
                    let slot = self.paths.push(path);
                    self.by_key.insert(key, slot);
                    return;
                };
                if self.paths.get(old) == path {
                    return;
                }
                let old_prefix = self.paths.get(old).to_owned();
                let moved = self.rebase_paths(&old_prefix, path);
                debug_assert!(moved != 0);
            }

            fn rebase_paths(&mut self, old_prefix: &str, new_prefix: &str) -> usize {
                let mut moved = 0usize;
                let mut old_bytes = 0usize;
                let mut new_bytes = 0usize;
                for slot in self.by_key.values().copied() {
                    let path = self.paths.get(slot);
                    if let Some(suffix) = descendant_suffix_start(path, old_prefix) {
                        moved += 1;
                        old_bytes += path.len();
                        new_bytes += new_prefix.len() + path.len() - suffix;
                    }
                }
                if moved == 0 {
                    return 0;
                }
                let compact = PathArena::should_compact_at(
                    self.paths.stale_bytes + old_bytes,
                    self.paths.used_bytes() + new_bytes,
                );
                if compact {
                    self.compact_rebased_paths(old_prefix, new_prefix);
                    return moved;
                }
                let (by_key, paths) = (&mut self.by_key, &mut self.paths);
                for slot in by_key.values_mut() {
                    let old = *slot;
                    let suffix = {
                        let path = paths.get(old);
                        descendant_suffix_start(path, old_prefix)
                    };
                    if let Some(suffix) = suffix {
                        *slot = paths.push_rebased(old, suffix, new_prefix);
                        paths.stale_bytes += old.len as usize;
                    }
                }
                debug_assert!(!self.paths.should_compact());
                moved
            }

            fn compact_rebased_paths(&mut self, old_prefix: &str, new_prefix: &str) {
                let old = std::mem::take(&mut self.paths);
                let mut fresh = PathArena::default();
                for slot in self.by_key.values_mut() {
                    let path = old.get(*slot);
                    *slot = match descendant_suffix_start(path, old_prefix) {
                        Some(suffix) => fresh.push_parts(new_prefix, &path[suffix..]),
                        None => fresh.push(path),
                    };
                }
                self.paths = fresh;
            }

            pub fn path_of(&self, ino: u64) -> Option<&str> {
                self.devices
                    .iter()
                    .find_map(|&dev| self.by_key.get(&DirKey { dev, ino }).copied())
                    .map(|slot| self.paths.get(slot))
            }
        }
    }

    /// The two directory maps this machine really holds, as a function.
    ///
    /// Measured with `find -xdev -type d`, 2026-09-06: `/home/hasan` is 423,384
    /// directories averaging 118.1 B of path against 15.6 B of basename, and
    /// `/mnt/depo` is 180,566 averaging 98.7 B against 12.7 B — 603,950
    /// directories, 68.2 MB of paths and 8.9 MB of names, at about 10.6 path
    /// components each. The journal names the same two counts.
    ///
    /// **Nothing is stored.** A directory's parent, name and key are all
    /// functions of its index, so a probe can walk 604k directories without
    /// holding 68 MB of strings beside the thing it is trying to weigh.
    ///
    /// A name is `stem + depth` bytes long because that is what makes the two
    /// averages come out at once: an ancestor's name is shorter than a leaf's
    /// in every real tree — `src`, `.git`, `hasan` against
    /// `2026-08-rapor-taslak` — which is why the average path is 7.6 basenames
    /// wide rather than 10.6 of them. Both fits are within 4%, and
    /// [`Shape::describe`] prints what was actually generated beside the
    /// measured figures rather than asking anyone to take this on trust.
    #[derive(Clone, Copy)]
    struct Shape {
        root: &'static str,
        dev: u64,
        count: usize,
        fanout: usize,
        stem: usize,
        /// Every other name one byte longer, which is how a non-integer average
        /// name length is reached with integer names.
        jitter: bool,
    }

    /// `/home/hasan`: 118.0 B of path and 16.2 B of name against 118.1 and 15.6.
    const HOME: Shape = Shape {
        root: "/home/hasan",
        dev: 259 << 20 | 5,
        count: 423_384,
        fanout: 5,
        stem: 8,
        jitter: true,
    };

    /// `/mnt/depo`: 98.5 B of path and 13.4 B of name against 98.7 and 12.7.
    const DEPO: Shape = Shape {
        root: "/mnt/depo",
        dev: 259 << 20 | 9,
        count: 180_566,
        fanout: 4,
        stem: 5,
        jitter: false,
    };

    impl Shape {
        fn parent_of(&self, n: usize) -> Option<usize> {
            (n > 0).then(|| (n - 1) / self.fanout)
        }

        fn depth_of(&self, n: usize) -> usize {
            let mut at = n;
            let mut depth = 0;
            while let Some(up) = self.parent_of(at) {
                at = up;
                depth += 1;
            }
            depth
        }

        /// A key that is stable for an index, and distinct across the shapes.
        fn key(&self, n: usize) -> DirKey {
            DirKey {
                dev: self.dev,
                ino: n as u64 + 10,
            }
        }

        fn parent_key(&self, n: usize) -> Option<DirKey> {
            self.parent_of(n).map(|up| self.key(up))
        }

        /// The name this directory is known by inside its parent — for the
        /// root, its whole path, which is what the map holds for a root too.
        fn name(&self, n: usize, out: &mut String) {
            use std::fmt::Write;
            if n == 0 {
                out.push_str(self.root);
                return;
            }
            let want = self.stem + self.depth_of(n) + usize::from(self.jitter && n & 1 == 1);
            let from = out.len();
            // Five bytes and not ASCII: a name arena that only ever held
            // `[a-z]` would not notice a slicing bug on a multi-byte boundary.
            let _ = write!(out, "öge-{n:x}");
            while out.len() - from < want {
                out.push('a');
            }
        }

        fn path(&self, n: usize, out: &mut String) {
            out.clear();
            let mut chain = [0usize; MAX_DEPTH];
            let mut depth = 0;
            let mut at = n;
            loop {
                chain[depth] = at;
                depth += 1;
                match self.parent_of(at) {
                    Some(up) => at = up,
                    None => break,
                }
            }
            for &at in chain[..depth].iter().rev() {
                if at != 0 {
                    out.push('/');
                }
                self.name(at, out);
            }
        }

        /// Every path this shape names, in index order.
        fn paths(&self, upto: usize) -> Vec<String> {
            let mut out = Vec::with_capacity(upto);
            let mut one = String::new();
            for n in 0..upto {
                self.path(n, &mut one);
                out.push(one.clone());
            }
            out
        }
    }

    /// What a generated shape actually came out as, for printing beside what
    /// was measured on the disk. Read off the paths themselves rather than off
    /// the generator, so a bug in the generator cannot hide in its own summary.
    fn summarise(root: &str, paths: &[String]) -> String {
        let path_bytes: usize = paths.iter().map(String::len).sum();
        // The root holds its whole path in the arena; everything else a name.
        let name_bytes: usize = paths
            .iter()
            .enumerate()
            .map(|(n, p)| {
                if n == 0 {
                    p.len()
                } else {
                    p.rsplit('/').next().unwrap_or(p).len()
                }
            })
            .sum();
        let components: usize = paths.iter().map(|p| p.matches('/').count() + 1).sum();
        let n = paths.len().max(1) as f64;
        format!(
            "{root} {} dirs · path {:.1} B ({:.1} MB) · name {:.1} B ({:.1} MB) · {:.1} components",
            paths.len(),
            path_bytes as f64 / n,
            path_bytes as f64 / 1e6,
            name_bytes as f64 / n,
            name_bytes as f64 / 1e6,
            components as f64 / n,
        )
    }

    /// One `FAN_CREATE` event for `name` in the directory with inode `ino`,
    /// laid out the way [`parse`] reads it.
    ///
    /// Built by hand rather than captured, because what these tests need is a
    /// stream that never ends — which is exactly the shape no recorded capture
    /// has.
    fn event(ino: u32, name: &str) -> Vec<u8> {
        let info_len = 29 + name.len();
        let event_len = 24 + info_len;
        let mut e = vec![0u8; event_len];
        e[0..4].copy_from_slice(&(event_len as u32).to_le_bytes());
        e[6..8].copy_from_slice(&24u16.to_le_bytes());
        e[8..16].copy_from_slice(&FAN_CREATE.to_le_bytes());
        let p = 24;
        e[p] = FAN_EVENT_INFO_TYPE_DFID_NAME;
        e[p + 2..p + 4].copy_from_slice(&(info_len as u16).to_le_bytes());
        // `struct file_handle`: eight bytes of handle, type 1 — the
        // `FILEID_INO32_GEN` shape `handle_ino` reads as a u32 inode.
        e[p + 12..p + 16].copy_from_slice(&8u32.to_le_bytes());
        e[p + 16..p + 20].copy_from_slice(&1i32.to_le_bytes());
        e[p + 20..p + 24].copy_from_slice(&ino.to_le_bytes());
        e[p + 28..p + 28 + name.len()].copy_from_slice(name.as_bytes());
        e
    }

    /// Fill a read buffer with as many events as it holds.
    fn buffer_of(events: usize) -> Vec<u8> {
        let mut out = Vec::new();
        for i in 0..events {
            out.extend_from_slice(&event(1000 + i as u32, "dosya.txt"));
        }
        out
    }

    #[test]
    fn the_hand_built_event_is_the_one_parse_reads() {
        // Every ceiling below is measured in events, so a builder that produced
        // nothing at all would make all of them pass while testing nothing.
        let mut seen = Vec::new();
        assert!(parse(&event(4242, "rapor.pdf"), &mut seen));
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].parent_ino, 4242);
        assert_eq!(seen[0].name, "rapor.pdf");
        assert!(seen[0].fresh, "FAN_CREATE means the entry may be new");
    }

    #[test]
    fn a_window_stops_collecting_before_it_can_eat_the_heap() {
        // **The kernel queue is unlimited on purpose.** `scour-watch` opens the
        // group with `FAN_UNLIMITED_QUEUE` and reasons about what that costs in
        // *kernel* memory; nothing had ever reasoned about the userspace vector
        // that drains it. A producer that never stops — an `rm -rf` of a large
        // tree, an unpacked archive — used to be read into this vector until
        // the two-second deadline, which at the module's own measured drain
        // rate of 4.6 million events a second is about nine million owned
        // names.
        //
        // A reader that never runs out of events is the whole test.
        let mut buf = vec![0u8; BUF];
        let mut seen: Vec<Seen> = Vec::new();
        let stream = buffer_of(1_000);
        let mut calls = 0usize;
        let lost = fill(&mut buf, &mut seen, |b| {
            calls += 1;
            b[..stream.len()].copy_from_slice(&stream);
            stream.len() as isize
        });

        assert!(
            lost,
            "a window that stopped early must say so, or the events it never \
             read are treated as events that never happened"
        );
        // `parse` empties a whole buffer before the length is looked at, so the
        // ceiling is reached from below by at most one buffer.
        assert!(
            seen.len() < MAX_SEEN + 1_000,
            "the window collected {} events against a ceiling of {MAX_SEEN}",
            seen.len()
        );
        assert!(
            seen.len() >= MAX_SEEN,
            "it stopped early for some other reason than the ceiling"
        );
        // And it stopped because of the count, not because two seconds passed:
        // this stream is served from memory and could not have taken that long.
        assert!(
            calls < MAX_SEEN,
            "the deadline ended the window, not the cap"
        );
    }

    #[test]
    fn a_burst_does_not_become_the_reader_s_floor() {
        // `clear` keeps capacity. Without the shrink in `fill`, one burst set
        // the reader's allocation for the rest of the process's life: the peak
        // became the floor, and a service that had been busy once held the
        // memory for it while idle.
        let mut buf = vec![0u8; BUF];
        let mut seen: Vec<Seen> = Vec::new();
        let stream = buffer_of(1_000);
        let mut left = 400usize;
        fill(&mut buf, &mut seen, |b| {
            if left == 0 {
                return 0;
            }
            left -= 1;
            b[..stream.len()].copy_from_slice(&stream);
            stream.len() as isize
        });
        let after_burst = seen.capacity();
        assert!(
            after_burst > KEEP_SEEN,
            "the burst was too small to be worth testing"
        );

        // The next window is an ordinary quiet one.
        let quiet = fill(&mut buf, &mut seen, |_| 0);
        assert!(!quiet, "an empty window has lost nothing");
        assert!(
            seen.capacity() <= KEEP_SEEN,
            "the reader kept {} slots after the burst, against {KEEP_SEEN}",
            seen.capacity()
        );
    }

    #[test]
    fn an_ordinary_window_is_read_to_the_end_and_reported_complete() {
        // The negative control the ceiling needs: a window that fits must not
        // be reported as lost, or every ordinary burst becomes a full walk of
        // every root and the bound costs more than it saves.
        let mut buf = vec![0u8; BUF];
        let mut seen: Vec<Seen> = Vec::new();
        let stream = buffer_of(1_000);
        let mut left = 3usize;
        let lost = fill(&mut buf, &mut seen, |b| {
            if left == 0 {
                return 0;
            }
            left -= 1;
            b[..stream.len()].copy_from_slice(&stream);
            stream.len() as isize
        });
        assert!(!lost, "3,000 events is an ordinary window, not an overflow");
        assert_eq!(seen.len(), 3_000);
    }

    #[test]
    fn the_kernel_saying_it_dropped_events_is_still_reported() {
        // The overflow record was the only way `lost` could be set before, and
        // the new ceiling must not have displaced it.
        let mut over = event(1, "x");
        let mask = FAN_CREATE | FAN_Q_OVERFLOW;
        over[8..16].copy_from_slice(&mask.to_le_bytes());
        let mut buf = vec![0u8; BUF];
        let mut seen: Vec<Seen> = Vec::new();
        let mut left = 1usize;
        let lost = fill(&mut buf, &mut seen, |b| {
            if left == 0 {
                return 0;
            }
            left -= 1;
            b[..over.len()].copy_from_slice(&over);
            over.len() as isize
        });
        assert!(
            lost,
            "a kernel overflow record still means the subtree is lost"
        );
    }

    /// What the directory map's **own walk** costs against a real tree.
    ///
    /// This backend does not walk to establish the watch — the mark covers the
    /// superblock and the helper set it before this process existed — but it
    /// does walk to learn which directory an event's file handle names. That is
    /// a second pass over the same tree the scan walks, and this measures it
    /// beside the scan's own parallel walk of the same roots so the two can be
    /// compared rather than guessed at.
    ///
    /// ```text
    /// SCOUR_WALK_ROOTS=/home/hasan cargo test -p scour-source-fs --release \
    ///   directory_map_walk_cost_probe -- --ignored --nocapture --test-threads=1
    /// ```
    #[test]
    #[ignore = "diagnostic walk-cost probe against a real tree"]
    fn directory_map_walk_cost_probe() {
        let roots: Vec<std::path::PathBuf> = std::env::var("SCOUR_WALK_ROOTS")
            .unwrap_or_else(|_| "/home/hasan".into())
            .split(':')
            .filter(|s| !s.is_empty())
            .map(std::path::PathBuf::from)
            .collect();
        let rounds: usize = std::env::var("SCOUR_WALK_ROUNDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3);

        // The rules scourd actually runs with, or the walk prunes nothing and
        // the number is of a scan nobody performs.
        let (paths, dirs, files) = crate::rules::platform_defaults();
        let opts = ScanOptions {
            hidden: true,
            follow_symlinks: false,
            skip_metadata: false,
            threads: 0,
            exclude_paths: paths,
            exclude_dirs: dirs,
            exclude_files: files,
            ..Default::default()
        };
        let rules = Arc::new(Rules::from_options(&opts));

        /// Counts and holds nothing, so the reading is the walk's.
        #[derive(Default)]
        struct Count {
            entries: u64,
            bytes: u64,
        }
        impl scour_core::EntrySink for Count {
            fn push(&mut self, e: scour_core::Entry) -> scour_core::Flow {
                self.entries += 1;
                self.bytes += e.path.len() as u64;
                scour_core::Flow::Continue
            }
        }

        /// The stack walk this replaced, kept here as the explicit control.
        ///
        /// An A/B against an unset variable is not an A/B — the repository has
        /// been caught by that once — so the old shape stays in the probe that
        /// retired it rather than in a git revision nobody will rebuild.
        fn build_serial(roots: &[std::path::PathBuf], rules: &Rules) -> DirMap {
            let mut map = DirMap::default();
            let mut stack: Vec<std::path::PathBuf> = roots.to_vec();
            while let Some(dir) = stack.pop() {
                let text = path::from_path(&dir);
                if rules.excludes_path(&text) {
                    continue;
                }
                let Ok(md) = std::fs::symlink_metadata(&dir) else {
                    continue;
                };
                if !md.is_dir() {
                    continue;
                }
                map.insert(&md, dir.parent().and_then(dir_key), &text);
                let Ok(rd) = std::fs::read_dir(&dir) else {
                    continue;
                };
                for e in rd.flatten() {
                    if e.file_type().is_ok_and(|t| t.is_dir()) {
                        stack.push(e.path());
                    }
                }
            }
            map
        }

        let source = FsSource::new(SourceId(0), "probe", roots.clone());
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4);
        // The shipped setting by default, so an ordinary run of this probe
        // measures what the service does rather than a sweep of what it could.
        let threads: Vec<usize> = std::env::var("SCOUR_WALK_THREADS")
            .map(|v| v.split(':').filter_map(|s| s.parse().ok()).collect())
            .unwrap_or_else(|_| vec![walk_threads(&source, &opts)]);
        println!(
            "roots {roots:?} · medium {} · {cores} cores · the map walk ships on \
             {} thread(s), the scan on {}",
            source.medium().label(),
            walk_threads(&source, &opts),
            source.medium().threads(cores),
        );
        for round in 0..rounds {
            // Alternating within the round, because this machine drifts more
            // than 10% across a day and two numbers taken an hour apart are two
            // different machines.
            let began = Instant::now();
            let serial = build_serial(&roots, &rules);
            let serial_took = began.elapsed();
            let serial_dirs = serial.by_key.len();
            drop(serial);

            let mut parallel = String::new();
            for &t in &threads {
                let began = Instant::now();
                let map = DirMap::build(&roots, &rules, t);
                let took = began.elapsed();
                let dirs = map.by_key.len();
                drop(map);
                assert_eq!(
                    dirs, serial_dirs,
                    "the parallel walk found {dirs} directories against the stack walk's \
                     {serial_dirs} — the two do not agree about the tree"
                );
                parallel += &format!(
                    " · parallel×{t} {:.3} s ({:.1}×)",
                    took.as_secs_f64(),
                    serial_took.as_secs_f64() / took.as_secs_f64(),
                );
            }

            let mut sink = Count::default();
            let began = Instant::now();
            let report = scour_core::Source::scan(&source, &opts, &mut sink);
            let scan_took = began.elapsed();
            let _ = report;

            println!(
                "round {round}: {serial_dirs} dirs · serial {:.3} s{parallel} · \
                 scan {} entries in {:.3} s",
                serial_took.as_secs_f64(),
                sink.entries,
                scan_took.as_secs_f64(),
            );
        }
    }

    /// What the directory map costs in **process anonymous memory**, which is a
    /// different question from what it costs in allocations.
    ///
    /// [`directory_map_memory_probe`] answers the second one: it reads
    /// `mallinfo2`, and reported the packing as 46.05 MB → 33.03 MB with
    /// retained allocations falling 255,769 → 19. Its author was explicit that
    /// this is a claim about the allocator and not about RSS, and a later
    /// summary repeated the number without that caveat. This test exists so the
    /// RSS half is not a matter of opinion: an allocation that is freed into a
    /// glibc arena and never returned to the kernel is a saving `mallinfo2`
    /// sees and `RssAnon` does not.
    ///
    /// One shape per process, chosen by `SCOUR_DIRMAP_SHAPE`, because the two
    /// shapes in one process share an allocator whose arenas the first one
    /// already grew — which is exactly the confusion being resolved. Scale is
    /// `SCOUR_DIRMAP_DIRS`; the default is both live sources at once, 603,950
    /// directories, at the shape [`Shape`] reproduces.
    ///
    /// ```text
    /// SCOUR_DIRMAP_SHAPE=strings cargo test -p scour-source-fs --release directory_map_rss_probe -- --ignored --nocapture --test-threads=1
    /// SCOUR_DIRMAP_SHAPE=paths   cargo test -p scour-source-fs --release directory_map_rss_probe -- --ignored --nocapture --test-threads=1
    /// SCOUR_DIRMAP_SHAPE=graph   cargo test -p scour-source-fs --release directory_map_rss_probe -- --ignored --nocapture --test-threads=1
    /// ```
    #[test]
    #[ignore = "diagnostic RSS probe"]
    fn directory_map_rss_probe() {
        fn rss_anon_kb() -> u64 {
            let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
            let field = |key: &str| -> u64 {
                status
                    .lines()
                    .find(|l| l.starts_with(key))
                    .and_then(|l| l.split_whitespace().nth(1))
                    .and_then(|n| n.parse().ok())
                    .unwrap_or(0)
            };
            field("RssAnon:")
        }
        fn hwm_kb() -> u64 {
            let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
            status
                .lines()
                .find(|l| l.starts_with("VmHWM:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|n| n.parse().ok())
                .unwrap_or(0)
        }

        let limit: usize = std::env::var("SCOUR_DIRMAP_DIRS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(usize::MAX);
        let shape = std::env::var("SCOUR_DIRMAP_SHAPE").unwrap_or_else(|_| "graph".into());

        let before = rss_anon_kb();
        let started = Instant::now();
        let mut path = String::new();
        let mut dirs = 0usize;
        // Held past the reading, or the drop is what is being measured.
        let held: Box<dyn std::fmt::Debug> = match shape.as_str() {
            // One `String` a directory: what the arena replaced in August.
            "strings" => {
                let mut maps: Vec<HashMap<DirKey, String>> = Vec::new();
                for source in [HOME, DEPO] {
                    let mut one = HashMap::new();
                    for n in 0..source.count.min(limit) {
                        source.path(n, &mut path);
                        one.insert(source.key(n), path.clone());
                        dirs += 1;
                    }
                    maps.push(one);
                }
                Box::new(maps.len())
            }
            // Whole paths packed into one-MiB chunks: what shipped.
            "paths" => {
                let mut maps = Vec::new();
                for source in [HOME, DEPO] {
                    let mut one = full_path::DirMap::default();
                    for n in 0..source.count.min(limit) {
                        source.path(n, &mut path);
                        one.insert_key(source.key(n), &path);
                        dirs += 1;
                    }
                    maps.push(one);
                }
                Box::new(maps.len())
            }
            // One name each, and a parent link.
            _ => {
                let mut maps = Vec::new();
                for source in [HOME, DEPO] {
                    let mut one = DirMap::default();
                    for n in 0..source.count.min(limit) {
                        source.path(n, &mut path);
                        one.insert_key(source.key(n), source.parent_key(n), &path);
                        dirs += 1;
                    }
                    maps.push(one);
                }
                Box::new(maps.len())
            }
        };
        let built = started.elapsed();
        let after = rss_anon_kb();
        std::hint::black_box(&held);

        println!(
            "shape={shape} dirs={dirs} RssAnon {before} kB -> {after} kB \
             (delta {} kB = {:.2} MB) VmHWM {} kB build {:.2} s",
            after - before,
            (after - before) as f64 / 1024.0,
            hwm_kb(),
            built.as_secs_f64(),
        );
    }

    /// What the directory map costs the allocator, and what a lookup costs,
    /// at the scale and the shape of the two live sources.
    ///
    /// **This is the measurement the parent-component graph exists for, so it
    /// carries its own control.** [`full_path::DirMap`] is the packed
    /// whole-path arena that shipped on 2026-08-15; both are built, weighed,
    /// looked up and renamed *inside one process*, because two processes are
    /// two allocator states and the August reading was already misread once as
    /// a claim about RSS. The order alternates round by round for the reason
    /// the walk probe's does: this machine drifts more than 10% across a day.
    ///
    /// The lookup is timed twice for the arena — once borrowing, once with the
    /// `to_owned` the reader actually did — because the graph cannot borrow: it
    /// spells the path into the caller's buffer. Comparing a borrow against a
    /// spelled-out path would flatter the arena by an allocation the reader
    /// paid anyway.
    ///
    /// Run alone so allocator readings do not include another test:
    ///
    /// `cargo test -p scour-source-fs directory_map_memory_probe --release -- --ignored --nocapture --test-threads=1`
    #[test]
    #[ignore = "diagnostic allocator and latency probe"]
    fn directory_map_memory_probe() {
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        fn heap_bytes() -> usize {
            let info = unsafe { libc::mallinfo2() };
            info.uordblks + info.hblkhd
        }

        #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
        fn heap_bytes() -> usize {
            0
        }

        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        fn trim_heap() {
            unsafe {
                libc::malloc_trim(0);
            }
        }

        #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
        fn trim_heap() {}

        #[derive(Default, Clone, Copy)]
        struct Row {
            retained: usize,
            build: Duration,
            borrowed: Duration,
            owned: Duration,
            buffered: Duration,
            rename_small: Duration,
            rename_root: Duration,
        }

        let rounds: usize = std::env::var("SCOUR_DIRMAP_ROUNDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(6);
        let sources = [HOME, DEPO];

        // Generated once and held, so a round measures the map and not the
        // generator — and allocated before the first reading, so it is outside
        // every heap delta below.
        let paths: Vec<Vec<String>> = sources.iter().map(|s| s.paths(s.count)).collect();
        for (s, p) in sources.iter().zip(&paths) {
            println!("shape   {}", summarise(s.root, p));
        }
        println!(
            "on disk /home/hasan 423384 dirs · path 118.1 B (50.0 MB) · name 15.6 B (6.6 MB)\n\
             on disk /mnt/depo   180566 dirs · path  98.7 B (17.8 MB) · name 12.7 B (2.3 MB)\n\
             on disk both        603950 dirs · ~10.6 components · find -xdev -type d, 2026-09-06"
        );

        // The subtree the small rename moves: the node whose descendants come
        // closest to a thousand. Sizes in one pass, children before parents.
        let mut sizes = vec![1u32; HOME.count];
        for n in (1..HOME.count).rev() {
            let up = HOME.parent_of(n).expect("only the root has no parent");
            sizes[up] += sizes[n];
        }
        let small = (1..HOME.count)
            .min_by_key(|&n| sizes[n].abs_diff(1_000))
            .expect("a tree this size has an interior node");
        let renamed_small = format!("{}-tasindi", paths[0][small]);
        println!(
            "rename  node {small} at depth {} moves {} directories; the root moves {}",
            HOME.depth_of(small),
            sizes[small],
            sizes[0],
        );

        // A fixed pseudo-random order, so neither arm sees the inodes in the
        // order it inserted them. 104,729 is prime and coprime with both
        // counts, so this is a permutation rather than a sample.
        let order: Vec<Vec<u64>> = sources
            .iter()
            .map(|s| {
                (0..s.count)
                    .map(|n| s.key(n.wrapping_mul(104_729) % s.count).ino)
                    .collect()
            })
            .collect();
        let lookups: usize = order.iter().map(Vec::len).sum();

        let arena_round = || -> Row {
            let mut row = Row::default();
            trim_heap();
            let before = heap_bytes();
            let began = Instant::now();
            let mut maps: Vec<full_path::DirMap> = Vec::new();
            for (s, paths) in sources.iter().zip(&paths) {
                let mut one = full_path::DirMap::default();
                for (n, path) in paths.iter().enumerate() {
                    one.insert_key(s.key(n), path);
                }
                maps.push(one);
            }
            row.build = began.elapsed();
            row.retained = heap_bytes().saturating_sub(before);

            let mut sink = 0usize;
            let began = Instant::now();
            for (map, inos) in maps.iter().zip(&order) {
                for &ino in inos {
                    sink += map.path_of(ino).map_or(0, str::len);
                }
            }
            row.borrowed = began.elapsed();
            let began = Instant::now();
            for (map, inos) in maps.iter().zip(&order) {
                for &ino in inos {
                    if let Some(path) = map.path_of(ino) {
                        sink += std::hint::black_box(path.to_owned()).len();
                    }
                }
            }
            row.owned = began.elapsed();
            std::hint::black_box(sink);

            let began = Instant::now();
            maps[0].insert_key(HOME.key(small), &renamed_small);
            row.rename_small = began.elapsed();
            let began = Instant::now();
            maps[0].insert_key(HOME.key(0), "/home/tasindi");
            row.rename_root = began.elapsed();

            drop(maps);
            trim_heap();
            row
        };

        // The order the walk hands directories over in, which decides how far
        // apart a node and its parent end up — and therefore what the climb
        // costs. The index order is breadth-first; `ignore` gives each worker
        // its own stack, so a real walk is closer to depth-first, and the two
        // are measured rather than argued about.
        let depth_first: Vec<Vec<usize>> = sources
            .iter()
            .map(|s| {
                let mut out = Vec::with_capacity(s.count);
                let mut stack = vec![0usize];
                while let Some(n) = stack.pop() {
                    out.push(n);
                    let first = n * s.fanout + 1;
                    for child in (first..first + s.fanout).rev() {
                        if child < s.count {
                            stack.push(child);
                        }
                    }
                }
                out
            })
            .collect();

        let graph_round = |dfs: bool| -> Row {
            let mut row = Row::default();
            trim_heap();
            let before = heap_bytes();
            let began = Instant::now();
            let mut maps: Vec<DirMap> = Vec::new();
            for ((s, paths), order) in sources.iter().zip(&paths).zip(&depth_first) {
                let mut one = DirMap::default();
                if dfs {
                    for &n in order {
                        one.insert_key(s.key(n), s.parent_key(n), &paths[n]);
                    }
                } else {
                    for (n, path) in paths.iter().enumerate() {
                        one.insert_key(s.key(n), s.parent_key(n), path);
                    }
                }
                maps.push(one);
            }
            row.build = began.elapsed();
            row.retained = heap_bytes().saturating_sub(before);

            let mut sink = 0usize;
            let began = Instant::now();
            for (map, inos) in maps.iter().zip(&order) {
                for &ino in inos {
                    let mut fresh = String::new();
                    if map.path_of(ino, &mut fresh) {
                        sink += std::hint::black_box(fresh).len();
                    }
                }
            }
            row.owned = began.elapsed();
            let mut buffer = String::new();
            let began = Instant::now();
            for (map, inos) in maps.iter().zip(&order) {
                for &ino in inos {
                    if map.path_of(ino, &mut buffer) {
                        sink += buffer.len();
                    }
                }
            }
            row.buffered = began.elapsed();
            std::hint::black_box(sink);

            let began = Instant::now();
            maps[0].insert_key(HOME.key(small), HOME.parent_key(small), &renamed_small);
            row.rename_small = began.elapsed();
            let began = Instant::now();
            maps[0].insert_key(HOME.key(0), None, "/home/tasindi");
            row.rename_root = began.elapsed();

            drop(maps);
            trim_heap();
            row
        };

        let per = |d: Duration| d.as_secs_f64() * 1e9 / lookups as f64;
        let show = |what: &str, round: usize, row: &Row| {
            println!(
                "round {round} {what:6} retained {:6.2} MB · build {:5.2} s · lookup \
                 borrow {:6.1} ns / owned {:6.1} ns / buffered {:6.1} ns · rename \
                 {} dirs {:?} · rename root {:?}",
                row.retained as f64 / 1e6,
                row.build.as_secs_f64(),
                per(row.borrowed),
                per(row.owned),
                per(row.buffered),
                sizes[small],
                row.rename_small,
                row.rename_root,
            );
        };

        let (mut arena, mut graph, mut deep) = (Vec::new(), Vec::new(), Vec::new());
        for round in 0..rounds {
            // Alternating within the round: two numbers taken minutes apart on
            // a machine that drifts are two different machines.
            let mut run = |which: usize| match which {
                0 => {
                    let row = arena_round();
                    show("arena", round, &row);
                    arena.push(row);
                }
                1 => {
                    let row = graph_round(false);
                    show("graph", round, &row);
                    graph.push(row);
                }
                _ => {
                    let row = graph_round(true);
                    show("depth", round, &row);
                    deep.push(row);
                }
            };
            for which in 0..3 {
                run((which + round) % 3);
            }
        }

        let median = |mut v: Vec<f64>| -> f64 {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        let medians = |rows: &[Row]| -> Row {
            Row {
                retained: median(rows.iter().map(|r| r.retained as f64).collect()) as usize,
                build: Duration::from_secs_f64(median(
                    rows.iter().map(|r| r.build.as_secs_f64()).collect(),
                )),
                borrowed: Duration::from_secs_f64(median(
                    rows.iter().map(|r| r.borrowed.as_secs_f64()).collect(),
                )),
                owned: Duration::from_secs_f64(median(
                    rows.iter().map(|r| r.owned.as_secs_f64()).collect(),
                )),
                buffered: Duration::from_secs_f64(median(
                    rows.iter().map(|r| r.buffered.as_secs_f64()).collect(),
                )),
                rename_small: Duration::from_secs_f64(median(
                    rows.iter().map(|r| r.rename_small.as_secs_f64()).collect(),
                )),
                rename_root: Duration::from_secs_f64(median(
                    rows.iter().map(|r| r.rename_root.as_secs_f64()).collect(),
                )),
            }
        };
        let (a, g, d) = (medians(&arena), medians(&graph), medians(&deep));
        show("arena", rounds, &a);
        show("graph", rounds, &g);
        show("depth", rounds, &d);
        println!(
            "median  retained {:.2} MB -> {:.2} MB ({:+.2} MB, {:+.1}%) · lookup as the \
             reader makes it {:.1} ns -> {:.1} ns ({:.2}×) · rename {} dirs {:?} -> {:?}",
            a.retained as f64 / 1e6,
            g.retained as f64 / 1e6,
            (g.retained as f64 - a.retained as f64) / 1e6,
            100.0 * (g.retained as f64 - a.retained as f64) / a.retained.max(1) as f64,
            per(a.owned),
            per(g.buffered),
            per(g.buffered) / per(a.owned),
            sizes[small],
            a.rename_small,
            g.rename_small,
        );
        println!(
            "median  depth-first insertion order: lookup {:.1} ns ({:.2}x the arena), \
             which is the same map with its nodes laid out the way a walker \
             thread's own stack lays them out",
            per(d.buffered),
            per(d.buffered) / per(a.owned),
        );
        println!(
            "won {} of {rounds} rounds on retained bytes",
            (0..rounds)
                .filter(|&i| graph[i].retained < arena[i].retained)
                .count()
        );
    }

    /// A tree wide and deep enough that several walker threads end with a
    /// partly filled buffer. Returns every directory it made, including `root`.
    fn plant(root: &std::path::Path, breadth: usize, depth: usize) -> Vec<std::path::PathBuf> {
        let mut made = vec![root.to_path_buf()];
        let mut frontier = vec![root.to_path_buf()];
        for level in 0..depth {
            let mut next = Vec::new();
            for parent in &frontier {
                for n in 0..breadth {
                    let dir = parent.join(format!("d{level}-{n}"));
                    std::fs::create_dir(&dir).expect("directory");
                    // A file beside it, so the walk has to reject something as
                    // well as accept something.
                    std::fs::write(dir.join("dosya.txt"), b"x").expect("file");
                    made.push(dir.clone());
                    next.push(dir);
                }
            }
            frontier = next;
        }
        made
    }

    #[test]
    fn the_parallel_walk_finds_every_directory_the_stack_walk_found() {
        // **A directory missing from this map is a directory whose every event
        // resolves to nothing**, so the walk that fills it has to be complete
        // in a way an index can afford not to be: the scan can miss a file and
        // find it next time, and this cannot, because "next time" is the next
        // full scan and everything in between is invisible.
        //
        // The failure this guards is the one the batching introduced. Each
        // walker thread gathers `DIR_BATCH` directories before sending, and
        // `ignore` gives a visitor no way to say it has finished — so without
        // the `Drop` on `DirBatch` every thread silently drops its last partial
        // buffer. The tree is deliberately not a multiple of the batch, so most
        // threads end holding one.
        let root = tempfile::tempdir().expect("temporary directory");
        let made = plant(root.path(), 7, 4);
        assert!(
            made.len() > DIR_BATCH,
            "the tree must be larger than one batch to test the tail, {} is not",
            made.len()
        );

        let rules = Rules::from_options(&ScanOptions::default());
        let roots = vec![root.path().to_path_buf()];
        let parallel = DirMap::build(&roots, &rules, 8);

        let mut lost = Vec::new();
        for dir in &made {
            use std::os::unix::fs::MetadataExt;
            let md = std::fs::symlink_metadata(dir).expect("metadata");
            if resolved(&parallel, md.ino()).as_deref() != Some(path::from_path(dir).as_str()) {
                lost.push(path::from_path(dir));
            }
        }
        assert!(
            lost.is_empty(),
            "{} of {} directories are in no map and would resolve no event; \
             the first is {:?}",
            lost.len(),
            made.len(),
            lost.first()
        );
        assert_eq!(
            parallel.by_key.len(),
            made.len(),
            "the walk recorded a different number of directories than exist"
        );
        // One thread and eight must agree about the tree, or the thread count
        // is a correctness setting rather than a speed one.
        let single = DirMap::build(&roots, &rules, 1);
        assert_eq!(single.by_key.len(), parallel.by_key.len());
    }

    #[test]
    fn a_directory_that_appears_during_the_walk_is_still_reachable() {
        // **The hole `scan.on_start` exists to close, checked on this backend.**
        //
        // On inotify the race is real and was fixed by ordering: a directory
        // walked before it is watched is one whose contents change unheard. On
        // fanotify the mark is on the superblock and the helper set it before
        // this process existed, so coverage never depends on this walk — what
        // depends on it is *resolution*, and an event naming a directory this
        // map has never heard of is dropped rather than escalated.
        //
        // So the guarantee to hold is this: whatever appears while the walk is
        // running, no event about it is lost. It is held by two things
        // together, and both are checked here — the new directory's **parent**
        // is in the map whichever side of the walk it was created on, and
        // `learn` puts the new directory itself in on the strength of that
        // parent's event. The events themselves cannot be lost meanwhile
        // because they are queued by a mark that predates the process, and the
        // subscription is not registered until the walk has finished.
        let root = tempfile::tempdir().expect("temporary directory");
        let made = plant(root.path(), 6, 3);
        let rules = Rules::from_options(&ScanOptions::default());
        let roots = vec![root.path().to_path_buf()];

        // Created *while the walk runs*, in directories that already existed —
        // which is the only shape this race has, because a parent that did not
        // exist when the walk began has a parent that did.
        let parents: Vec<std::path::PathBuf> = made.iter().skip(1).step_by(3).cloned().collect();
        assert!(parents.len() > 8, "too few parents to race against");
        let racing = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let born: Vec<std::path::PathBuf> = {
            let theirs = std::sync::Arc::clone(&racing);
            let parents = parents.clone();
            let creator = std::thread::spawn(move || {
                let mut born = Vec::new();
                let mut n = 0usize;
                while theirs.load(Ordering::Relaxed) || n < parents.len() {
                    let parent = &parents[n % parents.len()];
                    let dir = parent.join(format!("gec-{n}"));
                    if std::fs::create_dir(&dir).is_ok() {
                        std::fs::write(dir.join("icerik.txt"), b"x").expect("file");
                        born.push(dir);
                    }
                    n += 1;
                    if n > 4_000 {
                        break;
                    }
                }
                born
            });
            let map = DirMap::build(&roots, &rules, 8);
            racing.store(false, Ordering::Relaxed);
            let born = creator.join().expect("the creating thread");

            // Every directory that was there before the walk began is in the
            // map. That is what makes the rest of this reachable.
            for dir in &made {
                use std::os::unix::fs::MetadataExt;
                let md = std::fs::symlink_metadata(dir).expect("metadata");
                assert_eq!(
                    resolved(&map, md.ino()).as_deref(),
                    Some(path::from_path(dir).as_str()),
                    "a directory that existed before the walk is missing from the map"
                );
            }

            // And every directory born during it is either already in the map
            // or resolvable through its parent — never neither, which is the
            // hole.
            let mut map = map;
            for dir in &born {
                use std::os::unix::fs::MetadataExt;
                let text = path::from_path(dir);
                let md = std::fs::symlink_metadata(dir).expect("metadata");
                if resolved(&map, md.ino()).as_deref() == Some(text.as_str()) {
                    continue;
                }
                let parent = dir.parent().expect("a created directory has a parent");
                let parent_md = std::fs::symlink_metadata(parent).expect("parent metadata");
                assert_eq!(
                    resolved(&map, parent_md.ino()).as_deref(),
                    Some(path::from_path(parent).as_str()),
                    "neither {text} nor its parent is in the map, so its create \
                     event resolves to nothing and everything below it is invisible"
                );
                // Which is exactly what the reader does with that event.
                map.learn(&text);
                assert_eq!(
                    resolved(&map, md.ino()).as_deref(),
                    Some(text.as_str()),
                    "the parent's event did not bring {text} into the map"
                );
                // And now a file created inside it resolves too.
                let child = dir.join("icerik.txt");
                let child_parent = resolved(&map, md.ino()).expect("the learned directory");
                assert_eq!(
                    format!("{child_parent}/icerik.txt"),
                    path::from_path(&child),
                    "a file in the new directory resolves to the wrong path"
                );
            }
            born
        };
        assert!(
            !born.is_empty(),
            "nothing was created during the walk, so nothing was raced"
        );
    }

    #[test]
    fn a_directory_map_keeps_one_device_candidate_per_device() {
        use std::os::unix::fs::MetadataExt;

        let root = tempfile::tempdir().expect("temporary directory");
        let first = root.path().join("first");
        let second = root.path().join("second");
        std::fs::create_dir(&first).expect("first directory");
        std::fs::create_dir(&second).expect("second directory");

        let mut map = DirMap::default();
        map.learn(&path::from_path(&first));
        map.learn(&path::from_path(&second));
        let bytes = map.names.used_bytes();
        // Learning an already known directory must not grow either table.
        map.learn(&path::from_path(&first));

        let first_md = std::fs::symlink_metadata(&first).expect("first metadata");
        assert_eq!(map.devices, [first_md.dev()]);
        assert_eq!(map.by_key.len(), 2);
        assert_eq!(map.names.used_bytes(), bytes);
        // The directory both of these are *in* is not one of them. It has a
        // node, because their names hang off it, and it is deliberately not
        // answerable: an event in it belongs to whatever source walked it.
        assert_eq!(map.outside.len(), 1);
        assert_eq!(map.nodes.len(), 3);
        assert_eq!(
            resolved(&map, first_md.ino()).as_deref(),
            Some(path::from_path(&first).as_str())
        );
        let parent_md = std::fs::symlink_metadata(root.path()).expect("parent metadata");
        assert_eq!(
            resolved(&map, parent_md.ino()),
            None,
            "a directory that is only somebody's parent resolves no event"
        );
    }

    #[test]
    fn a_renamed_directory_replaces_its_path_without_growing_the_key_map() {
        use std::os::unix::fs::MetadataExt;

        let root = tempfile::tempdir().expect("temporary directory");
        let before = root.path().join("before");
        let after = root.path().join("after");
        std::fs::create_dir(&before).expect("directory");

        let before_text = path::from_path(&before);
        let after_text = path::from_path(&after);
        let md = std::fs::symlink_metadata(&before).expect("metadata");
        let mut map = DirMap::default();
        map.learn(&before_text);
        let names = map.names.used_bytes();
        std::fs::rename(&before, &after).expect("rename directory");
        map.learn(&after_text);

        assert_eq!(map.by_key.len(), 1);
        assert_eq!(
            resolved(&map, md.ino()).as_deref(),
            Some(after_text.as_str())
        );
        // A rename writes the new *name*, not the new path. The old name is
        // left where it is — five bytes against a compaction pass.
        assert_eq!(map.names.used_bytes(), names + "after".len());
    }

    #[test]
    fn a_renamed_directory_rebases_nested_paths_at_component_boundaries() {
        use std::os::unix::fs::MetadataExt;

        let temporary = tempfile::tempdir().expect("temporary directory");
        let parent = temporary.path().join("a");
        let before = parent.join("b");
        let child = before.join("child");
        let grandchild = child.join("nested");
        let sibling_prefix = parent.join("bc").join("untouched");
        let after = parent.join("moved-tree");
        std::fs::create_dir_all(&grandchild).expect("nested tree");
        std::fs::create_dir_all(&sibling_prefix).expect("prefix sibling");

        let root_ino = std::fs::symlink_metadata(&before).expect("root").ino();
        let child_ino = std::fs::symlink_metadata(&child).expect("child").ino();
        let grandchild_ino = std::fs::symlink_metadata(&grandchild)
            .expect("grandchild")
            .ino();
        let sibling_ino = std::fs::symlink_metadata(&sibling_prefix)
            .expect("sibling")
            .ino();
        let mut map = DirMap::default();
        for directory in [&before, &child, &grandchild, &sibling_prefix] {
            map.learn(&path::from_path(directory));
        }

        let keys = map.by_key.len();
        let capacity = map.by_key.capacity();
        let devices = map.devices.clone();
        let names = map.names.used_bytes();
        std::fs::rename(&before, &after).expect("rename tree");
        let after_text = path::from_path(&after);
        map.learn(&after_text);

        assert_eq!(
            resolved(&map, root_ino).as_deref(),
            Some(after_text.as_str())
        );
        assert_eq!(
            resolved(&map, child_ino).as_deref(),
            Some(path::from_path(&after.join("child")).as_str())
        );
        assert_eq!(
            resolved(&map, grandchild_ino).as_deref(),
            Some(path::from_path(&after.join("child/nested")).as_str())
        );
        assert_eq!(
            resolved(&map, sibling_ino).as_deref(),
            Some(path::from_path(&sibling_prefix).as_str()),
            "a byte prefix without a component boundary is not a descendant"
        );
        assert_eq!(map.by_key.len(), keys);
        assert_eq!(map.by_key.capacity(), capacity);
        assert_eq!(map.devices, devices);
        // The descendants moved without being written down again: the whole
        // rename cost the arena one basename.
        assert_eq!(map.names.used_bytes(), names + "moved-tree".len());

        let bytes = map.names.used_bytes();
        let chunks = map.names.chunks.len();
        map.learn(&after_text);
        assert_eq!(map.names.used_bytes(), bytes);
        assert_eq!(map.names.chunks.len(), chunks);
        assert_eq!(map.by_key.len(), keys);
    }

    /// A synthetic tree at the depth the real one has, built the way the
    /// parallel walk builds it: out of order.
    ///
    /// Turkish names because the arena stores bytes and the climb slices them:
    /// a map that only ever held `[a-z]` would not notice an off-by-one on a
    /// multi-byte boundary, and `Çalışmalar` is what is actually in `~`.
    struct Planted {
        paths: Vec<String>,
        keys: Vec<DirKey>,
        parents: Vec<Option<DirKey>>,
    }

    fn plant_synthetic(dirs: usize, fanout: usize) -> Planted {
        const WORDS: [&str; 8] = [
            "Çalışmalar",
            "Belgeler",
            "İndirilenler",
            "Müzik",
            "Şablonlar",
            "Günlük-notlar",
            "Öğrenciler",
            "Iğdır",
        ];
        let up = |n: usize| (n > 0).then(|| (n - 1) / fanout);
        let key = |n: usize| DirKey {
            dev: 42,
            ino: n as u64 + 10,
        };
        let mut paths: Vec<String> = Vec::with_capacity(dirs);
        for n in 0..dirs {
            let path = match up(n) {
                None => "/home/hasan".to_owned(),
                Some(parent) => format!("{}/{}-{n:x}", paths[parent], WORDS[n % WORDS.len()]),
            };
            paths.push(path);
        }
        Planted {
            keys: (0..dirs).map(key).collect(),
            parents: (0..dirs).map(|n| up(n).map(key)).collect(),
            paths,
        }
    }

    /// A deterministic permutation of `0..n`, so a child usually arrives before
    /// its parent — which is what several walker threads flushing batches
    /// independently does, and what the old representation never had to care
    /// about because it stored whole paths.
    fn shuffled(n: usize) -> Vec<usize> {
        (0..n).map(|i| i.wrapping_mul(104_729) % n).collect()
    }

    #[test]
    fn a_hundred_thousand_directories_spell_out_the_paths_they_were_given() {
        // The whole claim of this representation in one test: nothing stores a
        // path any more, so every path has to come back out of the chain
        // byte-identical — at the depth the real tree has, on names that are
        // not ASCII, inserted in an order that puts most children before their
        // parents.
        const DIRS: usize = 100_000;
        let tree = plant_synthetic(DIRS, 3);
        let deepest = tree.paths.iter().map(|p| p.matches('/').count()).max();
        assert!(
            deepest.is_some_and(|d| d >= 9),
            "a tree {deepest:?} deep does not exercise the climb"
        );

        // The premise, checked rather than assumed: this order really does put
        // most children in before their parents. If it did not, the whole
        // placeholder path would go untested and this would still pass.
        let order = shuffled(DIRS);
        let mut arrived = vec![false; DIRS];
        let mut early = 0usize;
        for &n in &order {
            if n > 0 && !arrived[(n - 1) / 3] {
                early += 1;
            }
            arrived[n] = true;
        }
        assert!(
            early > DIRS / 4,
            "only {early} of {DIRS} arrived before their parent, so the \
             out-of-order case is barely exercised"
        );

        let mut map = DirMap::default();
        for n in order {
            map.insert_key(tree.keys[n], tree.parents[n], &tree.paths[n]);
        }

        assert_eq!(map.by_key.len(), DIRS);
        // Every directory that was first met as somebody's parent was claimed
        // when the walk reached it: a node left over here would be a subtree
        // that a rename above it could not move.
        assert!(
            map.outside.is_empty(),
            "{} placeholder(s) were never claimed",
            map.outside.len()
        );
        assert_eq!(map.nodes.len(), DIRS, "one node a directory, and no more");

        let mut wrong = Vec::new();
        for n in 0..DIRS {
            let got = resolved(&map, tree.keys[n].ino);
            if got.as_deref() != Some(tree.paths[n].as_str()) {
                wrong.push((tree.paths[n].clone(), got));
            }
        }
        assert!(
            wrong.is_empty(),
            "{} of {DIRS} paths came back different; the first is {:?}",
            wrong.len(),
            wrong.first()
        );
    }

    #[test]
    fn renaming_a_directory_moves_its_descendants_exactly_where_a_rebuild_puts_them() {
        // **The one behaviour a graph could plausibly lose.** The arena moved a
        // subtree by rewriting every descendant's string; this moves one node
        // and lets the climb do the rest. So the test is not "the paths look
        // right", it is "the paths are the same ones" — against two
        // independent oracles: the arena doing the rename its own way, and a
        // map built from scratch out of the renamed paths.
        const DIRS: usize = 5_000;
        let tree = plant_synthetic(DIRS, 4);
        let moved = 1usize;
        let old_prefix = tree.paths[moved].clone();
        let new_prefix = format!("{}/{}", tree.paths[0], "Taşınmış-klasör");
        let after: Vec<String> = tree
            .paths
            .iter()
            .map(
                |p| match full_path::descendant_suffix_start(p, &old_prefix) {
                    Some(at) => format!("{new_prefix}{}", &p[at..]),
                    None => p.clone(),
                },
            )
            .collect();
        let descendants = tree
            .paths
            .iter()
            .filter(|p| full_path::descendant_suffix_start(p, &old_prefix).is_some())
            .count();
        assert!(
            descendants >= 1_000,
            "only {descendants} directories move, which is not the case worth testing"
        );

        // Out of order again, because a rename has to move the subtrees that
        // were stitched together late as well as the ones that were not.
        let mut graph = DirMap::default();
        let mut arena = full_path::DirMap::default();
        for n in shuffled(DIRS) {
            graph.insert_key(tree.keys[n], tree.parents[n], &tree.paths[n]);
            arena.insert_key(tree.keys[n], &tree.paths[n]);
        }
        graph.insert_key(tree.keys[moved], tree.parents[moved], &new_prefix);
        arena.insert_key(tree.keys[moved], &new_prefix);

        let mut rebuilt = DirMap::default();
        for (n, path) in after.iter().enumerate() {
            rebuilt.insert_key(tree.keys[n], tree.parents[n], path);
        }

        let mut wrong = Vec::new();
        for (n, want) in after.iter().enumerate() {
            let ino = tree.keys[n].ino;
            let got = resolved(&graph, ino);
            if got.as_deref() != Some(want.as_str())
                || got.as_deref() != arena.path_of(ino)
                || got != resolved(&rebuilt, ino)
            {
                wrong.push((want.clone(), got, arena.path_of(ino).map(str::to_owned)));
            }
        }
        assert!(
            wrong.is_empty(),
            "{} of {DIRS} paths disagree after the rename; the first is {:?}",
            wrong.len(),
            wrong.first()
        );

        // And it moved them without writing any of them down: one name, once.
        let names = graph.names.used_bytes();
        graph.insert_key(tree.keys[moved], tree.parents[moved], &new_prefix);
        assert_eq!(
            graph.names.used_bytes(),
            names,
            "renaming to the path it already has wrote something"
        );
    }

    #[test]
    fn a_directory_nested_deeper_than_the_stack_buffer_still_resolves() {
        // The climb keeps [`INLINE_DEPTH`] components on the stack. Past that
        // it spills to a `Vec` rather than giving up, and the difference
        // matters because the failure mode of giving up is invisible: an event
        // that resolves to nothing looks exactly like an event that never
        // happened, so a subtree nested past a fixed cap would simply stop
        // being watched with nothing said. `PATH_MAX` allows about 2,000
        // components; this is four times the buffer.
        const DEEP: usize = INLINE_DEPTH * 4;
        let key = |n: usize| DirKey {
            dev: 42,
            ino: n as u64 + 10,
        };

        let mut map = DirMap::default();
        let mut path = "/home/hasan".to_owned();
        let mut expected = vec![path.clone()];
        map.insert_key(key(0), None, &path);
        for n in 1..DEEP {
            path.push_str(&format!("/kat-{n:03}"));
            map.insert_key(key(n), Some(key(n - 1)), &path);
            expected.push(path.clone());
        }
        assert!(
            path.len() < 4096,
            "the test tree must stay inside PATH_MAX, {} does not",
            path.len()
        );
        for (n, want) in expected.iter().enumerate() {
            assert_eq!(
                resolved(&map, key(n).ino).as_deref(),
                Some(want.as_str()),
                "the directory {n} levels down did not come back"
            );
        }

        // And a chain that never reaches a root resolves to nothing rather
        // than spinning the reader thread. The kernel refuses to move a
        // directory inside itself, so this cannot be reached through
        // `insert_key` — which is exactly why it is worth pinning here.
        map.nodes[0].parent = DEEP as u32 - 1;
        for n in [0usize, DEEP / 2, DEEP - 1] {
            assert_eq!(
                resolved(&map, key(n).ino),
                None,
                "a cycle produced a path instead of nothing"
            );
        }
    }

    #[test]
    fn a_handle_is_read_by_its_type_and_its_length_together() {
        // btrfs: inode first, then the subvolume id.
        let mut btrfs = Vec::new();
        btrfs.extend_from_slice(&430u64.to_le_bytes());
        btrfs.extend_from_slice(&257u64.to_le_bytes());
        btrfs.extend_from_slice(&0x90u32.to_le_bytes());
        assert_eq!(handle_ino(0x4d, &btrfs), Some(430));

        // ntfs3 and friends: 32-bit inode, then a generation.
        let mut ntfs = Vec::new();
        ntfs.extend_from_slice(&0x0003_0f45u32.to_le_bytes());
        ntfs.extend_from_slice(&0x27u32.to_le_bytes());
        assert_eq!(handle_ino(1, &ntfs), Some(0x0003_0f45));

        // tmpfs uses the same type number with the fields the other way round,
        // which is why the length is part of the decision.
        let mut tmp = Vec::new();
        tmp.extend_from_slice(&0x8f45_42b1u32.to_le_bytes());
        tmp.extend_from_slice(&99u64.to_le_bytes());
        assert_eq!(handle_ino(1, &tmp), Some(99));
    }

    #[test]
    fn an_unknown_handle_is_refused_rather_than_guessed() {
        assert_eq!(handle_ino(0x63, &[1, 2, 3, 4]), None);
        assert_eq!(
            handle_ino(1, &[1, 2, 3]),
            None,
            "too short to hold an inode"
        );
        assert_eq!(handle_ino(0x4d, &[0; 12]), None, "btrfs handles are longer");
    }

    #[test]
    fn a_truncated_buffer_does_not_take_the_reader_down() {
        let mut out = Vec::new();
        for len in 0..40usize {
            let buf = vec![0u8; len];
            assert!(parse(&buf, &mut out), "len {len}");
        }
        // An event whose length runs past the buffer is dropped, not read.
        let mut buf = vec![0u8; 24];
        buf[0..4].copy_from_slice(&9999u32.to_le_bytes());
        assert!(parse(&buf, &mut out));
        assert!(out.is_empty());
    }

    #[test]
    fn only_a_filesystem_with_a_disk_behind_it_counts_as_a_mount() {
        let text = "\
25 1 0:23 / /proc rw - proc proc rw
26 1 0:5 / /sys rw - sysfs sysfs rw
31 1 259:5 /@ / rw - btrfs /dev/nvme0n1p5 rw,subvolid=256
48 1 259:5 /@home /home rw - btrfs /dev/nvme0n1p5 rw,subvolid=257
60 1 259:9 / /mnt/depo rw - ntfs3 /dev/nvme1n1p2 rw
61 1 0:44 / /run/user/1000 rw - tmpfs tmpfs rw
";
        let m = mounts(text);
        let mut at: Vec<&str> = m.iter().map(|(_, p)| p.as_str()).collect();
        at.sort_unstable();
        assert_eq!(at, ["/", "/home", "/mnt/depo"]);
    }

    #[test]
    fn two_mounts_of_one_disk_are_two_mounts_because_a_remount_loses_the_mark() {
        // Both lines are `/dev/nvme0n1p5`, and they are deliberately *not*
        // folded together: what matters here is not which superblock it is but
        // whether this particular mount is one the marks were placed before.
        let text = "\
31 1 259:5 /@ / rw - btrfs /dev/nvme0n1p5 rw,subvolid=256
48 1 259:5 /@home /home rw - btrfs /dev/nvme0n1p5 rw,subvolid=257
";
        assert_eq!(mounts(text).len(), 2);

        // The same filesystem unmounted and mounted again comes back with a
        // different mount id, which is what makes it visible as new.
        let after = "49 1 259:5 /@home /home rw - btrfs /dev/nvme0n1p5 rw,subvolid=257\n";
        let before = mounts(text);
        let now = mounts(after);
        assert_eq!(now.difference(&before).count(), 1);
    }

    #[test]
    fn a_mount_point_with_a_space_in_it_survives_being_read() {
        let text = "70 1 259:9 / /mnt/My\\040Disk rw - ntfs3 /dev/sdb1 rw\n";
        let m = mounts(text);
        assert_eq!(
            m.iter().next().map(|(_, p)| p.as_str()),
            Some("/mnt/My Disk")
        );
    }

    #[test]
    fn a_dropped_batch_is_reported_rather_than_silently_shortened() {
        let mut buf = vec![0u8; 24];
        buf[0..4].copy_from_slice(&24u32.to_le_bytes());
        buf[6..8].copy_from_slice(&24u16.to_le_bytes());
        buf[8..16].copy_from_slice(&FAN_Q_OVERFLOW.to_le_bytes());
        let mut out = Vec::new();
        assert!(!parse(&buf, &mut out), "overflow has to be visible");
        assert!(out.is_empty(), "and it carries nothing to act on");
    }
}
