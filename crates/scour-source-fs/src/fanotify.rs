//! Watching a whole filesystem with one mark.
//!
//! `FAN_MARK_FILESYSTEM` costs one mark a superblock: 0.005 ms and no measurable
//! kernel memory. Events merge — a create, write, close and delete arrive as one
//! mask — so the mask is never a description; a path is stated instead.

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
/// The marks need `CAP_SYS_ADMIN`: a process handed the descriptor reads every
/// event while `fanotify_init` and `fanotify_mark` still return EPERM to it.
const FD_ENV: &str = "SCOUR_FANOTIFY_FD";

/// How long to let events pile up before draining them.
/// At 2,120 events a second: 5 wakes and 0.078% of a core here, against 1,417
/// wakes and 0.838% at 0 ms. Past 500 ms the batch stops fitting in cache.
const WINDOW: Duration = Duration::from_millis(200);

/// Read buffer. 256 KiB carries 3.98× as many events a call as 64 KiB for 1.04×
/// the speed, so it is sized for the batch: about 3,600 events at 71 bytes each.
const BUF: usize = 256 * 1024;

/// The most events one window collects before it stops and says it lost track.
/// The kernel queue is unlimited, so this is the memory bound the deadline is not:
/// about 20 MB of [`Seen`], reached to within one buffer — [`parse`] empties one.
const MAX_SEEN: usize = 262_144;

/// The capacity one window may leave behind for the next.
/// `clear` keeps capacity, so without this a burst's peak becomes the floor.
/// Ten thousand events is about 800 KB.
const KEEP_SEEN: usize = 10_000;

// Not in `libc` at the time of writing, and their values are kernel ABI.
const FAN_REPORT_DIR_FID: u32 = 0x0000_0400;
const FAN_REPORT_NAME: u32 = 0x0000_0800;
/// What the helper has to have opened the group with, or nothing below works.
/// Both bits: `DIR_FID` puts the parent's handle in the event and `NAME` the
/// entry name. Without them events still parse, so paths would be wrong, not absent.
const REQUIRED_FLAGS: u32 = FAN_REPORT_DIR_FID | FAN_REPORT_NAME;
const FAN_Q_OVERFLOW: u64 = 0x0000_4000;
const FAN_EVENT_INFO_TYPE_DFID_NAME: u8 = 2;
const FAN_ONDIR: u64 = 0x4000_0000;
const FAN_CREATE: u64 = 0x0000_0100;
const FAN_MOVED_TO: u64 = 0x0000_0080;
const FAN_CLOSE_WRITE: u64 = 0x0000_0008;

/// What identifies a directory across a reboot, a remount and a rename.
/// The device number, because that is what `stat` hands the walk; the event
/// carries btrfs's subvolume id, matched through one row a mounted subvolume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DirKey {
    dev: u64,
    ino: u64,
}

/// One name in [`NameArena`]. The field order keeps this at eight bytes: a
/// component is at most `NAME_MAX`, a chunk one MiB, both far below `u16::MAX`.
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
/// Twelve bytes each rather than a whole path: 603,950 directories weigh 39.58 MB
/// against 87.82, a rename is 17.4 ms → 2.3 µs, and a lookup 114.3 ns → 371.6 ns.
#[derive(Debug, Clone, Copy)]
struct Node {
    parent: u32,
    name: NameSlot,
}

const _: () = assert!(std::mem::size_of::<Node>() == 12);

/// [`Node::parent`] for a directory that is nobody's child here: a walk root, or
/// one whose parent this map never learned. It holds its whole path as its name.
const NO_PARENT: u32 = u32::MAX;

/// How many components [`DirMap::components`] keeps on the stack.
/// Six times the measured average of 10.6, which is 512 bytes to zero a lookup.
/// Deeper is not refused, it spills — see [`DirMap::components`].
const INLINE_DEPTH: usize = 64;

/// How far a climb follows parent links before calling the chain broken.
/// A cycle guard, not a depth limit: `PATH_MAX` is 4096 bytes and a component
/// costs at least two, so no real path reaches this.
const MAX_DEPTH: usize = 4096;

/// Names in coarse allocations rather than one allocator object a directory.
/// Fixed chunks never need the old and the new allocation at once, and never
/// move bytes an existing slot names.
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
    /// Checked rather than `from_utf8_unchecked`: everything written here came from
    /// a `&str`, and 15 bytes a component is under the climb's cache misses.
    fn get(&self, slot: NameSlot) -> &str {
        let start = slot.start as usize;
        let end = start + slot.len as usize;
        self.chunks
            .get(slot.chunk as usize)
            .and_then(|chunk| chunk.get(start..end))
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .unwrap_or_default()
    }

    /// What the names actually cost, for the probes and the rename tests.
    #[cfg(test)]
    fn used_bytes(&self) -> usize {
        self.chunks.iter().map(Vec::len).sum()
    }
}

/// A path's parent and its last component, `/`-separated: `/a/b` is `("/a", "b")`,
/// `/a` is `("/", "a")`, and anything with no last component is kept whole. The
/// string is [`crate::path::from_path`]'s, so no invalid byte can read as a separator.
fn split_name(path: &str) -> Option<(&str, &str)> {
    let at = path.rfind('/')?;
    if at + 1 == path.len() {
        return None;
    }
    Some((if at == 0 { "/" } else { &path[..at] }, &path[at + 1..]))
}

/// What identifies the directory at this path, if it is one.
/// One `statx`, 0.55 µs warm, and it is what buys the parent link. The
/// alternative is a second index from path to node — the memory this removes.
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

/// The inode number a file handle carries, by filesystem — undocumented layouts,
/// read anyway because `open_by_handle_at` needs a capability this process does
/// not hold. `(type, length)` distinguishes them: tmpfs and ntfs3 both report type 1.
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
        // tmpfs: generation u32 @0, ino u64 @4 — twelve bytes, hence the length match.
        (1, 12) => le64(4),
        // FILEID_INO32_GEN, what ntfs3 and most others use: ino u32 @0, generation @4.
        (1, 8) => le32(0),
        _ => None,
    }
}

/// Directory identity to path: an event names its parent by opaque file handle,
/// and this turns one into a path. No path is stored — [`Node`] holds a name and a
/// parent link — and the walk records whole trees, so a parent is always present.
#[derive(Debug, Default)]
struct DirMap {
    by_key: HashMap<DirKey, u32>,
    nodes: Vec<Node>,
    names: NameArena,
    /// Directories that are only somebody's parent — a walk root's parent, or one
    /// whose batch has not arrived yet. Deliberately not answerable by
    /// [`DirMap::path_of`]: that would name a path no scan of this source produces.
    outside: HashMap<DirKey, u32>,
    /// Device candidates an event's inode is looked up against.
    /// The event gives an inode but no device — btrfs reports the superblock's fsid
    /// whatever subvolume it came from — so the inode is offered to each in turn.
    devices: Vec<u64>,
}

/// How many directories a walker thread gathers before handing them over.
/// A send per directory on a bounded channel cost the scan 9.5 context switches a file.
const DIR_BATCH: usize = 512;

/// How many batches may be in the air — threads times batch is the whole of the
/// extra memory the parallel walk costs over the stack walk it replaced.
const DIR_IN_FLIGHT: usize = 64;

/// One directory as a walker thread hands it over. The parent travels with it
/// because the walker has just read that directory and can name it cheaply.
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
/// `ignore` gives a visitor no way to say it has finished but does drop the box
/// when the thread ends; a directory missing from the map resolves none of its events.
impl Drop for DirBatch {
    fn drop(&mut self) {
        self.flush();
    }
}

impl DirMap {
    /// Walk the roots and record what each directory is.
    /// The mark predates this process, so what happens during the walk is queued
    /// behind it. `/mnt/depo`'s 152,530 directories: 1.03 s → 0.19 s warm on eight.
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
            // `ignore` as a concurrent directory walk and nothing else, exactly as in the scan.
            .standard_filters(false)
            // No name is filtered here: a change under `~/.config` is a change.
            .hidden(false)
            // The map must agree with the walk about links, or an event resolves to a
            // path no scan ever produces.
            .follow_links(false)
            .same_file_system(false)
            .threads(threads);

        // Drained while the walk runs: holding a quarter of a million `String`s once
        // more in a joining vector would put a 13 MB peak straight back.
        let (tx, rx) = crossbeam_channel::bounded::<Vec<Walked>>(DIR_IN_FLIGHT);
        std::thread::scope(|scope| {
            let walker_tx = tx.clone();
            scope.spawn(move || {
                builder.build_parallel().run(|| {
                    let mut batch = DirBatch::new(walker_tx.clone());
                    // On the first entry rather than here, because `ignore` builds the visitor
                    // on the thread that spawns the workers.
                    let mut polite = false;
                    Box::new(move |result| {
                        if !polite {
                            polite = true;
                            // The same nice value and idle I/O class the scan's walkers take, and
                            // neither costs anything on an idle machine.
                            crate::scan::stand_aside();
                        }
                        let Ok(de) = result else {
                            // A directory that cannot be read resolves no events.
                            return ignore::WalkState::Continue;
                        };
                        if !de.file_type().is_some_and(|t| t.is_dir()) {
                            return ignore::WalkState::Continue;
                        }
                        let text = path::from_path(de.path());
                        if rules.excludes_path(&text) {
                            // Pruned, not merely skipped: resolving what the scan discards is how a
                            // `cargo test` under a build tree took a query from 8 ms to 13 seconds.
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
                        // Named here rather than at the far end: this thread has just read the
                        // parent, and the far end would pay every `statx` in series.
                        let parent = de.path().parent().and_then(dir_key);
                        if batch.push(key, parent, text) {
                            ignore::WalkState::Continue
                        } else {
                            ignore::WalkState::Quit
                        }
                    })
                });
            });
            // The senders go when the visitors do, which is what ends the loop below,
            // so this clone has to go with them. Each tail is flushed by [`DirBatch`]'s `Drop`.
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
    /// `parent` is the directory this one is in, as the caller already knows it.
    /// `None`, or a parent this map never heard of, keeps the whole path instead.
    fn insert_key(&mut self, key: DirKey, parent: Option<DirKey>, path: &str) {
        if !self.devices.contains(&key.dev) {
            self.devices.push(key.dev);
        }
        // A directory that is its own parent would be a chain that never reaches a root:
        // every event below it unresolvable, with only the depth cap before a spin.
        let (parent_id, name) = self.place_under(parent.filter(|p| *p != key), path);

        if let Some(id) = self.by_key.get(&key).copied() {
            // `learn` runs on every created directory, and most are already here.
            if self.nodes[id as usize].parent == parent_id && self.path_matches(id, path) {
                return;
            }
            // A directory keeps `(dev, ino)` across a rename and so does every descendant,
            // so the whole subtree moves with this one assignment.
            let slot = self.names.push(name);
            self.nodes[id as usize] = Node {
                parent: parent_id,
                name: slot,
            };
            return;
        }

        // Met as somebody's parent before the walk reached it. Take that node over —
        // its children already point at it — rather than leaving two for one directory.
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
    /// The `&str` comes out of `path` rather than the arena, so the caller can push
    /// it while still holding the map mutably.
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
    /// Without it a `mkdir` resolves once — the create in its parent — and everything
    /// inside then arrives against a handle nothing knows.
    fn learn(&mut self, path: &str) {
        if let Ok(md) = std::fs::symlink_metadata(path::to_path(path))
            && md.is_dir()
        {
            // The parent is in this map but its *key* is not, and one `statx` turns the
            // path back into the link. Only paid when a directory is created.
            let parent = split_name(path).and_then(|(at, _)| dir_key(&path::to_path(at)));
            self.insert(&md, parent, path);
        }
    }

    /// The path of the directory this inode names, written into `out`.
    /// The caller's buffer, because there is no path anywhere to borrow. False leaves
    /// `out` empty: a chain that misses a component names a different file.
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

    /// Every component of this node's path, root first — names rather than node
    /// numbers, so the caller makes one random access each. [`INLINE_DEPTH`] on the
    /// stack, spilling past it; false for a chain that does not end. See [`MAX_DEPTH`].
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
            // The only path ending in a separator is `/`, where a second would spell `//x`.
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
    /// Compared component by component rather than through a rebuilt string: this
    /// runs on every `learn`, and most of those are unchanged.
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
/// Keyed by mount id, not device: the mark survives the unmount of the path it
/// was placed through, but a remount of the filesystem does not bring it back.
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
/// `poll` returns `POLLERR | POLLPRI` when the mount table changes, 300 ms from
/// the mount. A new disk is a new superblock, and its events simply never come.
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
    /// The kernel says the content is final: a descriptor opened for writing was
    /// closed — `munmap` for a mapping. See [`crate::revisit::forget`].
    settled: bool,
}

/// Collect one window's events, and say whether anything was lost.
/// [`MAX_SEEN`] and the two-second deadline both report lost rather than
/// complete: a truncated window called complete has a sweep delete rows that exist.
fn fill(buf: &mut [u8], seen: &mut Vec<Seen>, mut read: impl FnMut(&mut [u8]) -> isize) -> bool {
    seen.clear();
    // Before the window, so the last burst's peak does not become the floor.
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
        // The memory bound, checked before the time one — see [`MAX_SEEN`].
        if seen.len() >= MAX_SEEN {
            lost = true;
            break;
        }
        // A burst larger than the buffer is read out here, but not forever: at
        // 4.6 million events a second, two seconds is far past any real batch.
        if Instant::now() > deadline {
            lost = true;
            break;
        }
    }
    lost
}

/// Pull every event out of one buffer.
/// `None` for the whole batch when the kernel dropped events: the overflow record
/// carries nothing — `event_len` equals `metadata_len` and the descriptor is `FAN_NOFD`.
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
/// Absent is the ordinary case and not an error: [`try_start`] returns `None`.
fn inherited() -> Option<OwnedFd> {
    let raw: RawFd = std::env::var(FD_ENV).ok()?.trim().parse().ok()?;
    if raw < 0 {
        return None;
    }
    // **Check what it is before reading it.** A stale variable naming another open
    // file would be read as events, and a group without the report flags would parse
    // into wrong paths. `fdinfo`'s first line answers both.
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

/// One source's share of the one reader: a single group that cannot be split,
/// since two readers of one queue each silently drop the other's half. Routing is
/// recognition — the event's parent handle is offered to each subscriber in turn.
struct Sub {
    id: SourceId,
    real_modes: bool,
    rules: Arc<Rules>,
    sink: Arc<dyn ChangeSink>,
    roots: Vec<std::path::PathBuf>,
    map: DirMap,
    /// What [`walk_threads`] answered when the map was built, kept so that rebuilding
    /// it in [`WatchHandle::retune`] need not ask an `FsSource` out of reach.
    threads: usize,
    /// Cleared when the handle is dropped; the entry stays, because an index has to
    /// keep meaning what it meant.
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
    id: SourceId,
    live: Arc<AtomicBool>,
    /// Filesystems that appeared after the marks were set: not covered, and not
    /// coverable from here, so they are named for the layer above to report.
    uncovered: Arc<Mutex<Vec<String>>>,
}

impl WatchHandle for FanWatch {
    fn unwatched(&self) -> Vec<String> {
        self.uncovered.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// Nothing to do, and that is the point of this backend: a filesystem mark covers
    /// the superblock, so a directory created a moment ago is already watched.
    fn cover(&self, _path: &str) {}

    /// Take the new rules, and rebuild the directory map behind them: the map is how
    /// an event gets a name, and a rule switched off re-opens a subtree it never heard
    /// of. Built before the lock — walking both roots is 0.7 s warm, 0.38 s cold on NTFS.
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

    /// Leave the reader running: it serves every source, so one stopping is no reason
    /// to take it down. The subscription goes quiet and the thread stays.
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
/// The scan's answer, because the question is about the device: a spinning disk
/// turns every extra reader into a seek, whoever is asking.
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
/// `None` means no descriptor was handed over, which is the ordinary case on a
/// machine where the helper was never installed.
pub fn try_start(
    source: &FsSource,
    opts: &ScanOptions,
    sink: Arc<dyn ChangeSink>,
) -> Option<Result<Box<dyn WatchHandle>>> {
    // The descriptor is taken once: two owners is a double close, and two readers
    // of one group is half the events each.
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
/// At most five wakes a second: `poll` returns on the first event and the window
/// runs before anything is read, then its events reduce to distinct paths.
fn drain(fd: OwnedFd) {
    let mut buf = vec![0u8; BUF];
    let mut seen: Vec<Seen> = Vec::new();
    // One buffer for every path the reader resolves, for the life of the thread:
    // the allocation the caller used to make per event, moved rather than added.
    let mut dir = String::new();
    let raw = fd.as_raw_fd();

    // The mount table, watched beside the events: one `poll` over two descriptors
    // keeps the whole watcher a single place that can be stopped.
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
                // `POLLPRI` is how the mount table signals a change; `POLLERR` always arrives.
                events: libc::POLLPRI,
                revents: 0,
            },
        ];
        let pr = unsafe { libc::poll(pfds.as_mut_ptr(), 2, 500) };
        if pr <= 0 {
            continue;
        }

        if pfds[1].revents != 0 {
            // Re-read from the start, or `poll` keeps reporting the same change.
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
            // Nothing in the overflow record says what was missed, so the subtree is the
            // only unit available — for everyone, because the queue was shared.
            for s in subs.iter().filter(|s| s.live.load(Ordering::Relaxed)) {
                for r in &s.roots {
                    s.sink.emit(Change::Rescan {
                        path: path::from_path(r),
                    });
                }
            }
            continue;
        }

        // Distinct paths a subscriber, keeping "this might be new" if any event said so.
        let mut batch: HashMap<(usize, String), (bool, bool, bool)> = HashMap::new();
        for ev in seen.drain(..) {
            // The subscriber that walked this directory owns the path. An event nobody
            // recognises is **dropped, not escalated**: it is almost always another
            // source's tree, and a rescan of every root would turn traffic into a storm.
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
            // A write through a mapping produces no event, so a path that has just spoken
            // is worth another look — unless the kernel has said the content is final.
            if settled {
                crate::revisit::forget(&full);
            } else {
                crate::revisit::note(&full, s.id, s.real_modes, &s.sink, md.as_ref());
            }
        }
    }
}

/// A filesystem that appeared after the marks were set.
/// Its contents can still be indexed, but nothing here can watch it: a new
/// filesystem is a new superblock and a mark needs a privilege this process lacks.
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

    /// [`DirMap::path_of`] as a test would rather read it: the reader keeps one
    /// buffer for the life of its thread.
    fn resolved(map: &DirMap, ino: u64) -> Option<String> {
        let mut out = String::new();
        map.path_of(ino, &mut out).then_some(out)
    }

    /// The full-path arena this replaced, kept as the explicit control.
    /// Two representations have to be weighed in one process, or the reading is of two
    /// allocator states. Only the probes and the rename test below use it.
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
                // SAFETY: the arena's append methods copy valid UTF-8 from `&str` or another
                // valid arena range; slots name only the exact appended bytes.
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
    /// 603,950 directories at about 10.6 components: 68.2 MB of path against 8.9 MB of
    /// name. Nothing is stored — parent, name and key are all functions of the index.
    #[derive(Clone, Copy)]
    struct Shape {
        root: &'static str,
        dev: u64,
        count: usize,
        fanout: usize,
        stem: usize,
        /// Every other name one byte longer, which is how a non-integer average is reached.
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

        /// The name this directory is known by inside its parent; for a root, its whole path.
        fn name(&self, n: usize, out: &mut String) {
            use std::fmt::Write;
            if n == 0 {
                out.push_str(self.root);
                return;
            }
            let want = self.stem + self.depth_of(n) + usize::from(self.jitter && n & 1 == 1);
            let from = out.len();
            // Five bytes and not ASCII, so a slicing bug on a multi-byte boundary shows.
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

    /// What a generated shape actually came out as. Read off the paths rather than
    /// off the generator, so a bug in it cannot hide in its own summary.
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

    /// One `FAN_CREATE` event for `name` in the directory with inode `ino`, laid out
    /// the way [`parse`] reads it. Built by hand, because these tests need a stream
    /// that never ends.
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
        // `struct file_handle`: eight bytes of handle, type 1 — the `FILEID_INO32_GEN` shape.
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
        // A builder that produced nothing would make every ceiling below pass.
        let mut seen = Vec::new();
        assert!(parse(&event(4242, "rapor.pdf"), &mut seen));
        assert_eq!(seen.len(), 1);
        assert_eq!(seen[0].parent_ino, 4242);
        assert_eq!(seen[0].name, "rapor.pdf");
        assert!(seen[0].fresh, "FAN_CREATE means the entry may be new");
    }

    #[test]
    fn a_window_stops_collecting_before_it_can_eat_the_heap() {
        // The kernel queue is unlimited on purpose, and nothing bounded the userspace
        // vector draining it: at 4.6 million events a second, two seconds is about nine
        // million owned names. A reader that never runs out of events is the whole test.
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
        // `parse` empties a whole buffer before the length is looked at.
        assert!(
            seen.len() < MAX_SEEN + 1_000,
            "the window collected {} events against a ceiling of {MAX_SEEN}",
            seen.len()
        );
        assert!(
            seen.len() >= MAX_SEEN,
            "it stopped early for some other reason than the ceiling"
        );
        // Stopped because of the count, not the deadline: this stream is served from memory.
        assert!(
            calls < MAX_SEEN,
            "the deadline ended the window, not the cap"
        );
    }

    #[test]
    fn a_burst_does_not_become_the_reader_s_floor() {
        // `clear` keeps capacity. Without the shrink in `fill`, one burst sets the
        // reader's allocation for the rest of the process's life.
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
        // The negative control the ceiling needs: a window that fits must not be
        // reported as lost, or every ordinary burst becomes a full walk of every root.
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
        // The overflow record was the only way `lost` could be set; the ceiling
        // must not displace it.
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

    /// What the directory map's own walk costs against a real tree, beside the scan's
    /// parallel walk of the same roots.
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

        // The rules scourd actually runs with, or the walk prunes nothing.
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

        /// The stack walk this replaced, kept here as the explicit control: an A/B
        /// against a git revision nobody will rebuild is not an A/B.
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
        // The shipped setting by default, so an ordinary run measures what the service does.
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
            // Alternating within the round: this machine drifts more than 10% across a day.
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

    /// What the directory map costs in **process anonymous memory**, which `mallinfo2`
    /// does not answer: memory freed into a glibc arena is a saving `RssAnon` never
    /// sees. `SCOUR_DIRMAP_SHAPE` is strings|paths|graph, `SCOUR_DIRMAP_DIRS` the scale.
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

    /// What the directory map costs the allocator, and what a lookup costs, at the
    /// scale and shape of the two live sources. [`full_path::DirMap`] is weighed in
    /// the *same process*, and the lookup timed twice because the graph cannot borrow.
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

        // Generated once and held, and allocated before the first reading.
        let paths: Vec<Vec<String>> = sources.iter().map(|s| s.paths(s.count)).collect();
        for (s, p) in sources.iter().zip(&paths) {
            println!("shape   {}", summarise(s.root, p));
        }
        println!(
            "on disk /home/hasan 423384 dirs · path 118.1 B (50.0 MB) · name 15.6 B (6.6 MB)\n\
             on disk /mnt/depo   180566 dirs · path  98.7 B (17.8 MB) · name 12.7 B (2.3 MB)\n\
             on disk both        603950 dirs · ~10.6 components · find -xdev -type d, 2026-09-06"
        );

        // The subtree the small rename moves: sizes in one pass, children before parents.
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

        // A fixed pseudo-random order, so neither arm sees the inodes in insertion
        // order. 104,729 is prime and coprime with both counts, so it is a permutation.
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

        // The order the walk hands directories over in decides how far apart a node and
        // its parent end up, and therefore what the climb costs. Index order is
        // breadth-first; `ignore` gives each worker its own stack, so a real walk is not.
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
            // Alternating within the round: two numbers taken minutes apart are two machines.
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
                    // A file beside it, so the walk rejects as well as accepts.
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
        // A directory missing from this map resolves none of its events, so the walk that
        // fills it must be complete in a way an index can afford not to be. The tree is
        // deliberately not a multiple of [`DIR_BATCH`], so most threads end holding one.
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
        // One thread and eight must agree, or the thread count is a correctness setting.
        let single = DirMap::build(&roots, &rules, 1);
        assert_eq!(single.by_key.len(), parallel.by_key.len());
    }

    #[test]
    fn a_directory_that_appears_during_the_walk_is_still_reachable() {
        // Coverage never depends on this walk — the mark predates the process — but
        // *resolution* does, and an event naming a directory this map never heard of is
        // dropped. So: whatever appears while the walk runs, its parent is in the map.
        let root = tempfile::tempdir().expect("temporary directory");
        let made = plant(root.path(), 6, 3);
        let rules = Rules::from_options(&ScanOptions::default());
        let roots = vec![root.path().to_path_buf()];

        // Created *while the walk runs*, in directories that already existed — the only
        // shape this race has, since a parent that did not exist has one that did.
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

            // Every directory that was there before the walk began is in the map.
            for dir in &made {
                use std::os::unix::fs::MetadataExt;
                let md = std::fs::symlink_metadata(dir).expect("metadata");
                assert_eq!(
                    resolved(&map, md.ino()).as_deref(),
                    Some(path::from_path(dir).as_str()),
                    "a directory that existed before the walk is missing from the map"
                );
            }

            // And every directory born during it is either already in the map or resolvable
            // through its parent — never neither, which is the hole.
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
        // The directory both of these are *in* is deliberately not answerable: an event
        // in it belongs to whatever source walked it.
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
        // A rename writes the new *name*, not the new path: five bytes against a compaction pass.
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
        // The descendants moved without being written down: the rename cost one basename.
        assert_eq!(map.names.used_bytes(), names + "moved-tree".len());

        let bytes = map.names.used_bytes();
        let chunks = map.names.chunks.len();
        map.learn(&after_text);
        assert_eq!(map.names.used_bytes(), bytes);
        assert_eq!(map.names.chunks.len(), chunks);
        assert_eq!(map.by_key.len(), keys);
    }

    /// A synthetic tree at the depth the real one has, built out of order the way the
    /// parallel walk builds it. Turkish names, because the arena stores bytes and the
    /// climb slices them.
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

    /// A deterministic permutation of `0..n`, so a child usually arrives before its
    /// parent — which is what several walker threads flushing independently does.
    fn shuffled(n: usize) -> Vec<usize> {
        (0..n).map(|i| i.wrapping_mul(104_729) % n).collect()
    }

    #[test]
    fn a_hundred_thousand_directories_spell_out_the_paths_they_were_given() {
        // Nothing stores a path, so every path has to come back out of the chain
        // byte-identical: at real depth, on non-ASCII names, inserted out of order.
        const DIRS: usize = 100_000;
        let tree = plant_synthetic(DIRS, 3);
        let deepest = tree.paths.iter().map(|p| p.matches('/').count()).max();
        assert!(
            deepest.is_some_and(|d| d >= 9),
            "a tree {deepest:?} deep does not exercise the climb"
        );

        // The premise, checked rather than assumed: this order really does put most
        // children in before their parents.
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
        // Every directory first met as somebody's parent was claimed when the walk
        // reached it: a node left over here is a subtree a rename could not move.
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
        // **The one behaviour a graph could plausibly lose.** The arena moved a subtree
        // by rewriting every descendant, so the test is not "the paths look right" but
        // "the paths are the same ones", against two independent oracles.
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

        // Out of order again, because a rename has to move late-stitched subtrees too.
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
        // The climb keeps [`INLINE_DEPTH`] components on the stack and spills past it.
        // Giving up instead is invisible — an event that resolves to nothing looks like
        // one that never happened. `PATH_MAX` allows about 2,000 components.
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

        // And a chain that never reaches a root resolves to nothing rather than spinning
        // the reader. The kernel refuses to move a directory inside itself.
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

        // tmpfs uses the same type number the other way round, hence the length.
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
        // Both lines are `/dev/nvme0n1p5` and are deliberately *not* folded: what matters
        // is whether this particular mount is one the marks were placed before.
        let text = "\
31 1 259:5 /@ / rw - btrfs /dev/nvme0n1p5 rw,subvolid=256
48 1 259:5 /@home /home rw - btrfs /dev/nvme0n1p5 rw,subvolid=257
";
        assert_eq!(mounts(text).len(), 2);

        // The same filesystem remounted comes back with a different mount id.
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
