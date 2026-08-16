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

/// One path in [`PathArena`].
///
/// The field order keeps this at eight bytes. A path that `symlink_metadata`
/// accepted on Linux is far below `u16::MAX`, a chunk is one MiB, and 65,536
/// chunks would already mean 64 GiB of directory names. Keeping the slot small
/// matters because `HashMap` reserves it once per directory, including spare
/// buckets.
#[derive(Debug, Clone, Copy)]
struct PathSlot {
    start: u32,
    chunk: u16,
    len: u16,
}

const _: () = assert!(std::mem::size_of::<PathSlot>() == 8);

/// Paths in coarse allocations rather than one allocator object a directory.
///
/// A single growing `Vec` would briefly need both the old and new allocation
/// whenever it grows. Fixed-size chunks keep peak memory bounded, waste less
/// than one chunk at the end, and never move bytes that an existing slot names.
#[derive(Debug, Default)]
struct PathArena {
    chunks: Vec<Vec<u8>>,
    stale_bytes: usize,
}

impl PathArena {
    const CHUNK: usize = 1024 * 1024;

    fn push(&mut self, path: &str) -> PathSlot {
        self.push_parts(path, "")
    }

    fn push_parts(&mut self, prefix: &str, suffix: &str) -> PathSlot {
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

    /// Append a path made from a new prefix and a suffix already in the arena.
    ///
    /// No temporary `String` per descendant: when source and destination share
    /// a chunk, `extend_from_within` copies by offsets; otherwise the chunks are
    /// borrowed separately. The capacity check happens before either branch,
    /// so appending to the source chunk cannot invalidate its range.
    fn push_rebased(&mut self, old: PathSlot, suffix_start: usize, prefix: &str) -> PathSlot {
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

    fn get(&self, slot: PathSlot) -> &str {
        let start = slot.start as usize;
        let end = start + slot.len as usize;
        // SAFETY: the arena's append methods copy valid UTF-8 from `&str` or
        // another valid arena range; slots name only the exact appended bytes.
        unsafe { std::str::from_utf8_unchecked(&self.chunks[slot.chunk as usize][start..end]) }
    }

    fn used_bytes(&self) -> usize {
        self.chunks.iter().map(Vec::len).sum()
    }

    fn should_compact(&self) -> bool {
        Self::should_compact_at(self.stale_bytes, self.used_bytes())
    }

    fn should_compact_at(stale_bytes: usize, used_bytes: usize) -> bool {
        stale_bytes >= Self::CHUNK && stale_bytes >= used_bytes / 4
    }
}

/// Where the part below `prefix` starts, with a component boundary.
///
/// `/a/b` therefore owns `/a/b/child` but not `/a/bc`. The root is special:
/// its descendants already carry the separator that has to follow a new root.
fn descendant_suffix_start(path: &str, prefix: &str) -> Option<usize> {
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
/// needed here is narrower: one key and one path per directory. Paths are
/// packed because hundreds of thousands of separate `String` allocations cost
/// both allocator metadata and a 24-byte value in every occupied or reserved
/// hash bucket. The ignored `directory_map_memory_probe` test measures the
/// complete allocator footprint and lookup cost at live-machine scale.
#[derive(Debug, Default)]
struct DirMap {
    by_key: HashMap<DirKey, PathSlot>,
    paths: PathArena,
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

/// One walker thread's outgoing buffer of directories.
struct DirBatch {
    tx: crossbeam_channel::Sender<Vec<(DirKey, String)>>,
    buf: Vec<(DirKey, String)>,
}

impl DirBatch {
    fn new(tx: crossbeam_channel::Sender<Vec<(DirKey, String)>>) -> DirBatch {
        DirBatch {
            tx,
            buf: Vec::with_capacity(DIR_BATCH),
        }
    }

    /// Returns false once the far end is gone, which is a walk to abandon.
    fn push(&mut self, key: DirKey, path: String) -> bool {
        self.buf.push((key, path));
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
        let (tx, rx) = crossbeam_channel::bounded::<Vec<(DirKey, String)>>(DIR_IN_FLIGHT);
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
                        if batch.push(key, text) {
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
                for (key, text) in batch {
                    map.insert_key(key, &text);
                }
            }
        });
        map
    }

    fn insert(&mut self, md: &std::fs::Metadata, path: &str) {
        use std::os::unix::fs::MetadataExt;
        self.insert_key(
            DirKey {
                dev: md.dev(),
                ino: md.ino(),
            },
            path,
        );
    }

    fn insert_key(&mut self, key: DirKey, path: &str) {
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

        // A directory keeps `(dev, ino)` across a rename. Every descendant's
        // fanotify handle keeps its key too, but its spelled path changes with
        // the parent; updating only this one entry leaves all later events from
        // below it resolving to the old tree. One owned prefix keeps the arena
        // free of a temporary allocation per descendant.
        let old_prefix = self.paths.get(old).to_owned();
        let moved = self.rebase_paths(&old_prefix, path);
        debug_assert!(moved != 0);
    }

    /// Rewrite one path and every component-bounded descendant below it.
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

        // Appending a large moved tree and compacting afterwards briefly keeps
        // the old paths, their rewritten copies and the compacted arena — three
        // copies at the worst possible rename. If this update already crosses
        // the compaction threshold, rebuild directly into the final arena and
        // keep the peak to the old and new copies.
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

    /// Reclaim stale paths while applying a large subtree rename.
    ///
    /// Built directly from the old arena rather than after appending moved
    /// paths to it, which avoids a third simultaneous copy at peak.
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

    /// Record a directory that appeared after the map was built.
    ///
    /// Without this a `mkdir` is seen once — the create in its parent, which
    /// does resolve — and then everything inside it arrives against a handle
    /// nothing knows, so a `git clone` would be a stream of unresolvable
    /// events. The walk that [`crate::watch::look`] already queues for a fresh
    /// directory covers the entries; this covers the *events*.
    fn learn(&mut self, path: &str) {
        if let Ok(md) = std::fs::symlink_metadata(path)
            && md.is_dir()
        {
            self.insert(&md, path);
        }
    }

    fn path_of(&self, ino: u64) -> Option<&str> {
        self.devices
            .iter()
            .find_map(|&dev| self.by_key.get(&DirKey { dev, ino }).copied())
            .map(|slot| self.paths.get(slot))
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
    let map = DirMap::build(&roots, &rules, walk_threads(source, opts));
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

    Some(Ok(Box::new(FanWatch { live, uncovered })))
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
            let Some((i, dir)) = subs.iter().enumerate().find_map(|(i, s)| {
                if !s.live.load(Ordering::Relaxed) {
                    return None;
                }
                s.map.path_of(ev.parent_ino).map(|d| (i, d.to_owned()))
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
                map.insert(&md, &text);
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
    /// `SCOUR_DIRMAP_DIRS`; the default is one source's worth, and the two live
    /// sources on this machine are 511,116 directories between them.
    ///
    /// ```text
    /// SCOUR_DIRMAP_SHAPE=old    cargo test -p scour-source-fs --release directory_map_rss_probe -- --ignored --nocapture --test-threads=1
    /// SCOUR_DIRMAP_SHAPE=packed cargo test -p scour-source-fs --release directory_map_rss_probe -- --ignored --nocapture --test-threads=1
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

        let dirs: usize = std::env::var("SCOUR_DIRMAP_DIRS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(255_769);
        let shape = std::env::var("SCOUR_DIRMAP_SHAPE").unwrap_or_else(|_| "packed".into());

        fn path(n: usize) -> String {
            let branch = n % 41;
            let project = (n / 41) % 997;
            let depth = (n / (41 * 997)) % 7;
            format!(
                "/home/hasan/Projeler/project-{project:03}/src/component-{branch:02}/depth-{depth}/directory-{n:06}"
            )
        }
        fn key(n: usize) -> DirKey {
            DirKey {
                dev: if n & 1 == 0 { 42 } else { 84 },
                ino: n as u64 + 10,
            }
        }

        let before = rss_anon_kb();
        let started = Instant::now();
        // Held past the reading, or the drop is what is being measured.
        let held: Box<dyn std::fmt::Debug> = match shape.as_str() {
            "old" => {
                let mut old: HashMap<DirKey, String> = HashMap::new();
                for n in 0..dirs {
                    old.insert(key(n), path(n));
                }
                Box::new(old.len())
            }
            _ => {
                let mut packed = DirMap::default();
                for n in 0..dirs {
                    packed.insert_key(key(n), &path(n));
                }
                Box::new(packed.by_key.len())
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

    /// Reproduce the userspace cost of the directory map at the scale of the
    /// two live sources on the development machine.
    ///
    /// Run alone so allocator readings do not include another test:
    ///
    /// `cargo test -p scour-source-fs directory_map_memory_probe --release -- --ignored --nocapture --test-threads=1`
    #[test]
    #[ignore = "diagnostic allocator and latency probe"]
    fn directory_map_memory_probe() {
        const DIRS: usize = 255_769;

        fn path(n: usize) -> String {
            let branch = n % 41;
            let project = (n / 41) % 997;
            let depth = (n / (41 * 997)) % 7;
            format!(
                "/home/hasan/Projeler/project-{project:03}/src/component-{branch:02}/depth-{depth}/directory-{n:06}"
            )
        }

        fn key(n: usize) -> DirKey {
            DirKey {
                dev: if n & 1 == 0 { 42 } else { 84 },
                ino: n as u64 + 10,
            }
        }

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

        let started = Instant::now();
        let mut old_startup = HashMap::new();
        let mut old_startup_devices = Vec::new();
        for n in 0..DIRS {
            let key = key(n);
            if !old_startup_devices.contains(&key.dev) {
                old_startup_devices.push(key.dev);
            }
            old_startup.insert(key, path(n));
        }
        let old_build = started.elapsed();
        std::hint::black_box(&old_startup);
        drop(old_startup);
        trim_heap();

        let started = Instant::now();
        let mut packed_startup = DirMap::default();
        for n in 0..DIRS {
            packed_startup.insert_key(key(n), &path(n));
        }
        let packed_build = started.elapsed();
        std::hint::black_box(&packed_startup);
        drop(packed_startup);
        trim_heap();

        let paths: Vec<String> = (0..DIRS).map(path).collect();
        let keys: Vec<DirKey> = (0..DIRS).map(key).collect();
        let path_bytes: usize = paths.iter().map(String::len).sum();
        let average = path_bytes as f64 / DIRS as f64;

        trim_heap();
        let before = heap_bytes();
        let mut old: HashMap<DirKey, String> = HashMap::new();
        let mut old_devices = Vec::new();
        for (key, path) in keys.iter().copied().zip(&paths) {
            if !old_devices.contains(&key.dev) {
                old_devices.push(key.dev);
            }
            old.insert(key, path.clone());
        }
        let old_heap = heap_bytes().saturating_sub(before);
        let old_capacity = old.capacity();
        let old_path_capacity: usize = old.values().map(String::capacity).sum();
        let started = Instant::now();
        for round in 0..8usize {
            for n in 0..DIRS {
                let at = (n.wrapping_mul(104_729) + round * 65_537) % DIRS;
                let ino = keys[at].ino;
                std::hint::black_box(
                    old_devices
                        .iter()
                        .find_map(|&dev| old.get(&DirKey { dev, ino }))
                        .map(String::as_str),
                );
            }
        }
        let old_lookup = started.elapsed();
        drop(old);
        trim_heap();

        let before = heap_bytes();
        let mut packed = DirMap::default();
        for (key, path) in keys.iter().copied().zip(&paths) {
            packed.insert_key(key, path);
        }
        let packed_heap = heap_bytes().saturating_sub(before);
        let packed_capacity = packed.by_key.capacity();
        let packed_path_capacity: usize = packed.paths.chunks.iter().map(Vec::capacity).sum();
        let packed_chunks = packed.paths.chunks.len();
        let started = Instant::now();
        for round in 0..8usize {
            for n in 0..DIRS {
                let at = (n.wrapping_mul(104_729) + round * 65_537) % DIRS;
                std::hint::black_box(packed.path_of(keys[at].ino));
            }
        }
        let packed_lookup = started.elapsed();

        let root = DirKey {
            dev: 42,
            ino: u64::MAX,
        };
        packed.insert_key(root, "/home/hasan/Projeler");
        let rename_map_capacity = packed.by_key.capacity();
        let rename_devices = packed.devices.clone();
        let rename_before_capacity: usize = packed.paths.chunks.iter().map(Vec::capacity).sum();
        let rename_before_used = packed.paths.used_bytes();
        let started = Instant::now();
        packed.insert_key(root, "/home/hasan/Workspace");
        let rename = started.elapsed();
        let rename_after_capacity: usize = packed.paths.chunks.iter().map(Vec::capacity).sum();
        let rename_after_used = packed.paths.used_bytes();
        let rename_after_stale = packed.paths.stale_bytes;
        let leaf_at = DIRS / 2;
        let leaf = paths[leaf_at].replacen("/home/hasan/Projeler", "/home/hasan/Workspace", 1);
        let renamed_leaf = format!("{leaf}-renamed");
        let leaf_before_used = packed.paths.used_bytes();
        let started = Instant::now();
        packed.insert_key(keys[leaf_at], &renamed_leaf);
        let leaf_rename = started.elapsed();
        let leaf_after_used = packed.paths.used_bytes();
        let leaf_after_stale = packed.paths.stale_bytes;
        let started = Instant::now();
        packed.insert_key(keys[leaf_at], &renamed_leaf);
        let duplicate_learn = started.elapsed();

        println!("directories={DIRS} path_bytes={path_bytes} average_path={average:.1}B");
        println!(
            "strings heap={old_heap}B map_capacity={old_capacity} path_capacity={old_path_capacity}B retained_path_allocations={DIRS} startup_build={old_build:?} lookup={old_lookup:?}"
        );
        println!(
            "packed  heap={packed_heap}B map_capacity={packed_capacity} path_capacity={packed_path_capacity}B retained_path_allocations={packed_chunks} startup_build={packed_build:?} lookup={packed_lookup:?}"
        );
        println!(
            "saved={}B ({:.1}%)",
            old_heap.saturating_sub(packed_heap),
            100.0 * old_heap.saturating_sub(packed_heap) as f64 / old_heap.max(1) as f64
        );
        println!(
            "subtree rename={rename:?} map_capacity={rename_map_capacity} path_used={rename_before_used}B->{rename_after_used}B retained_capacity={rename_before_capacity}B->{rename_after_capacity}B transient_path_capacity_ceiling={}B stale={}B",
            rename_before_capacity + rename_after_capacity,
            rename_after_stale,
        );
        println!(
            "leaf rename={leaf_rename:?} path_used={leaf_before_used}B->{leaf_after_used}B stale={leaf_after_stale}B duplicate_learn={duplicate_learn:?}",
        );

        assert_eq!(packed.by_key.len(), DIRS + 1);
        assert_eq!(packed.by_key.capacity(), rename_map_capacity);
        assert_eq!(packed.devices, rename_devices);
        assert_eq!(
            packed.path_of(keys[leaf_at].ino),
            Some(renamed_leaf.as_str())
        );
        assert_eq!(packed.paths.used_bytes(), leaf_after_used);
        assert_eq!(packed.paths.stale_bytes, leaf_after_stale);
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
            if parallel.path_of(md.ino()) != Some(path::from_path(dir).as_str()) {
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
                    map.path_of(md.ino()),
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
                if map.path_of(md.ino()) == Some(text.as_str()) {
                    continue;
                }
                let parent = dir.parent().expect("a created directory has a parent");
                let parent_md = std::fs::symlink_metadata(parent).expect("parent metadata");
                assert_eq!(
                    map.path_of(parent_md.ino()),
                    Some(path::from_path(parent).as_str()),
                    "neither {text} nor its parent is in the map, so its create \
                     event resolves to nothing and everything below it is invisible"
                );
                // Which is exactly what the reader does with that event.
                map.learn(&text);
                assert_eq!(
                    map.path_of(md.ino()),
                    Some(text.as_str()),
                    "the parent's event did not bring {text} into the map"
                );
                // And now a file created inside it resolves too.
                let child = dir.join("icerik.txt");
                let child_parent = map.path_of(md.ino()).expect("the learned directory");
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
        let bytes = map.paths.used_bytes();
        // Learning an already known directory must not grow either table.
        map.learn(&path::from_path(&first));

        let first_md = std::fs::symlink_metadata(&first).expect("first metadata");
        assert_eq!(map.devices, [first_md.dev()]);
        assert_eq!(map.by_key.len(), 2);
        assert_eq!(map.paths.used_bytes(), bytes);
        assert_eq!(map.paths.stale_bytes, 0);
        assert_eq!(
            map.path_of(first_md.ino()),
            Some(path::from_path(&first).as_str())
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
        std::fs::rename(&before, &after).expect("rename directory");
        map.learn(&after_text);

        assert_eq!(map.by_key.len(), 1);
        assert_eq!(map.path_of(md.ino()), Some(after_text.as_str()));
        assert_eq!(map.paths.stale_bytes, before_text.len());
        assert_eq!(map.paths.used_bytes(), before_text.len() + after_text.len());
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
        std::fs::rename(&before, &after).expect("rename tree");
        let after_text = path::from_path(&after);
        map.learn(&after_text);

        assert_eq!(map.path_of(root_ino), Some(after_text.as_str()));
        assert_eq!(
            map.path_of(child_ino),
            Some(path::from_path(&after.join("child")).as_str())
        );
        assert_eq!(
            map.path_of(grandchild_ino),
            Some(path::from_path(&after.join("child/nested")).as_str())
        );
        assert_eq!(
            map.path_of(sibling_ino),
            Some(path::from_path(&sibling_prefix).as_str()),
            "a byte prefix without a component boundary is not a descendant"
        );
        assert_eq!(map.by_key.len(), keys);
        assert_eq!(map.by_key.capacity(), capacity);
        assert_eq!(map.devices, devices);

        let bytes = map.paths.used_bytes();
        let stale = map.paths.stale_bytes;
        let chunks = map.paths.chunks.len();
        map.learn(&after_text);
        assert_eq!(map.paths.used_bytes(), bytes);
        assert_eq!(map.paths.stale_bytes, stale);
        assert_eq!(map.paths.chunks.len(), chunks);
        assert_eq!(map.by_key.len(), keys);
    }

    #[test]
    fn replaced_paths_are_compacted_without_changing_lookups() {
        const DIRS: usize = 20_000;

        let mut map = DirMap::default();
        let root = DirKey {
            dev: 42,
            ino: DIRS as u64,
        };
        map.insert_key(root, "/home/hasan/old-tree");
        for ino in 0..DIRS {
            let path = format!(
                "/home/hasan/old-tree/component-{ino:05}/a-directory-name-long-enough-to-fill-the-arena"
            );
            map.insert_key(
                DirKey {
                    dev: 42,
                    ino: ino as u64,
                },
                &path,
            );
        }
        map.insert_key(root, "/home/hasan/new-tree");

        assert_eq!(map.by_key.len(), DIRS + 1);
        assert!(map.paths.stale_bytes < PathArena::CHUNK);
        assert_eq!(map.path_of(root.ino), Some("/home/hasan/new-tree"));
        for ino in 0..DIRS {
            let expected = format!(
                "/home/hasan/new-tree/component-{ino:05}/a-directory-name-long-enough-to-fill-the-arena"
            );
            assert_eq!(map.path_of(ino as u64), Some(expected.as_str()));
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
