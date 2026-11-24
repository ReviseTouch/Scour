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

/// The most events one window collects before it stops; the rest are read by the
/// next. The kernel queue is unlimited, so this is the memory bound the deadline
/// is not: about 20 MB of [`Seen`], reached to within one buffer — [`parse`]
/// empties one.
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
const FAN_DELETE: u64 = 0x0000_0200;
const FAN_MOVED_FROM: u64 = 0x0000_0040;
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

/// Which filesystem a directory is on and, on btrfs, which subvolume: what an
/// event says about its parent beside the inode, and what a device is asked
/// once. **An inode number alone names nothing**: here 2.6% of the NTFS
/// volume's directories and 2.7% of `/`'s share one with a directory in the
/// home folder, and every btrfs subvolume's root is 256. An event offered by
/// number alone went to whichever map knew that number first — the wrong
/// path, a `stat` that failed, and the real change missed until the daily check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct FsTag {
    /// `__kernel_fsid_t`, as the event and `statfs` both give it.
    fsid: [u32; 2],
    /// The subvolume, from a btrfs handle; `None` on every other filesystem.
    root: Option<u64>,
}

impl FsTag {
    /// Whether an event with tag `event` can be about a directory on this one.
    /// btrfs mixes the subvolume into the low half of `statfs`'s fsid, and which
    /// subvolume's fsid an event carries depends on the path the helper marked —
    /// measured: `@tmp` `d30e8e93:6f61cd83`, `@home` `d30e8e93:6f61cd87`. So on
    /// btrfs the high half names the filesystem and the handle the subvolume.
    /// An event with no fsid — a test's — goes anywhere, as every event used to.
    fn holds(&self, event: &FsTag) -> bool {
        if event.fsid == [0, 0] {
            return true;
        }
        match (self.root, event.root) {
            (Some(mine), Some(theirs)) => mine == theirs && self.fsid[0] == event.fsid[0],
            (None, None) => self.fsid == event.fsid,
            _ => false,
        }
    }

    /// Ask the directory at `path`: `statfs` for the fsid, its own handle for the
    /// subvolume. `None` when either cannot be had, which matches any event.
    fn of(path: &str) -> Option<FsTag> {
        let c = std::ffi::CString::new(path).ok()?;
        // SAFETY: a zeroed `statfs` is valid to write into, and `c` outlives the call.
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
            return None;
        }
        // SAFETY: `fsid_t` is two `c_int`s with a private name; same size and layout.
        let fsid: [i32; 2] = unsafe { std::mem::transmute(st.f_fsid) };
        let (kind, bytes) = handle_of(&c)?;
        Some(FsTag {
            fsid: [fsid[0] as u32, fsid[1] as u32],
            root: btrfs_root(kind, &bytes),
        })
    }
}

/// The file handle the kernel gives `path`: its type and bytes, as an event
/// would carry them for the same directory.
fn handle_of(c: &std::ffi::CStr) -> Option<(i32, Vec<u8>)> {
    // `struct file_handle`: bytes u32, type i32, then the handle. As u32s, so
    // the header is aligned.
    let mut raw = [0u32; 2 + 32];
    raw[0] = 128;
    let mut mount_id: libc::c_int = 0;
    // SAFETY: `raw` is a `file_handle` header with room for the 128 bytes it
    // declares; `c` and `mount_id` outlive the call.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_name_to_handle_at,
            libc::AT_FDCWD,
            c.as_ptr(),
            raw.as_mut_ptr(),
            &mut mount_id as *mut libc::c_int,
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    let kind = raw[1] as i32;
    let len = (raw[0] as usize).min(128);
    let bytes: Vec<u8> = raw[2..]
        .iter()
        .flat_map(|w| w.to_ne_bytes())
        .take(len)
        .collect();
    Some((kind, bytes))
}

/// Whether events under `root` can be turned back into paths. A filesystem
/// whose handles this cannot read (vfat, bcachefs, ZFS…) would take the mark
/// and have every event dropped, while counting as watched — which also stops
/// the pulse walks that would otherwise keep it fresh.
fn decodable(root: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(root.as_os_str().as_bytes()) else {
        return false;
    };
    handle_of(&c).is_some_and(|(kind, bytes)| handle_ino(kind, &bytes).is_some())
}

/// The subvolume a btrfs handle names: `u64` at 8, after the inode.
fn btrfs_root(fh_type: i32, bytes: &[u8]) -> Option<u64> {
    match (fh_type, bytes.len()) {
        (0x4d..=0x4f, 20..) => Some(u64::from_le_bytes(bytes.get(8..16)?.try_into().ok()?)),
        _ => None,
    }
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

/// Every directory under `top`, top included, as the map records one: its key,
/// its parent's key and its path. Pruned by the rules the map's own walk prunes
/// by, and following no link, so the two agree on what a path is.
fn directories_under(top: &str, rules: &Rules) -> Vec<(DirKey, Option<DirKey>, String)> {
    use std::os::unix::fs::MetadataExt;
    let key = |m: &std::fs::Metadata| DirKey {
        dev: m.dev(),
        ino: m.ino(),
    };
    let start = path::to_path(top);
    let Ok(md) = std::fs::symlink_metadata(&start) else {
        return Vec::new();
    };
    if !md.is_dir() || rules.excludes_path(top) {
        return Vec::new();
    }
    let mut out = vec![(key(&md), start.parent().and_then(dir_key), top.to_owned())];
    let mut stack = vec![(start, key(&md))];
    while let Some((dir, at)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in entries.flatten() {
            if !e.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let p = e.path();
            let text = path::from_path(&p);
            if rules.excludes_path(&text) {
                continue;
            }
            // `DirEntry::metadata` does not follow a link, and a link is no directory here.
            let Ok(m) = e.metadata() else {
                continue;
            };
            out.push((key(&m), Some(at), text));
            stack.push((p, key(&m)));
        }
    }
    out
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
        // XFS with 64-bit inodes (its default): ino u64 @0, generation u32 @8.
        (0x81, 12) => le64(0),
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
    /// The event gives an inode and an [`FsTag`] but no device number, so the
    /// inode is offered to each device whose tag says it can be there.
    devices: Vec<u64>,
    /// Beside `devices`, one each: asked once, when the device is first met.
    tags: Vec<Option<FsTag>>,
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

impl DirMap {
    /// Walk the roots and record what each directory is.
    /// The mark predates this process, so what happens during the walk is queued
    /// behind it. `/mnt/depo`'s 152,530 directories: 1.03 s → 0.19 s warm on eight.
    fn build(roots: &[std::path::PathBuf], rules: &Rules, threads: usize) -> DirMap {
        let mut map = DirMap::default();
        if roots.is_empty() {
            return map;
        }

        // No name is filtered here: a change under `~/.config` is a change.
        // And links are not followed: the map must agree with the walk about
        // them, or an event resolves to a path no scan ever produces.
        let walk = surefoot::Walk::new(roots.iter().cloned())
            .threads(surefoot::Threads::Fixed(threads))
            // The same nice value and idle I/O class the scan's walkers take,
            // and neither costs anything on an idle machine.
            .priority(surefoot::Priority::Background)
            .batch(DIR_BATCH)
            .queue(DIR_IN_FLIGHT);

        // Drained while the walk runs: holding a quarter of a million `String`s once
        // more in a joining vector would put a 13 MB peak straight back.
        walk.run(
            |found| -> surefoot::Visit<Walked> {
                use surefoot::Visit;
                if !found.is_dir() {
                    return Visit::Pass;
                }
                let text = path::from_path(found.path());
                if rules.excludes_path(&text) {
                    // Pruned, not merely skipped: resolving what the scan discards is how a
                    // `cargo test` under a build tree took a query from 8 ms to 13 seconds.
                    return Visit::Prune;
                }
                let Ok(md) = found.metadata() else {
                    return Visit::Pass;
                };
                use std::os::unix::fs::MetadataExt;
                let key = DirKey {
                    dev: md.dev(),
                    ino: md.ino(),
                };
                // Named here rather than at the far end: this thread has just read the
                // parent, and the far end would pay every `statx` in series.
                let parent = found.path().parent().and_then(dir_key);
                Visit::Take((key, parent, text))
            },
            |event| {
                // A directory that cannot be read resolves no events.
                if let surefoot::Event::Batch(batch) = event {
                    for (key, parent, text) in batch {
                        map.insert_key(key, parent, &text);
                    }
                }
                std::ops::ControlFlow::Continue(())
            },
        );
        // Grown by doubling while the walk ran; the room left over is a quarter
        // of the list, kept for the few directories made after it.
        map.nodes.shrink_to_fit();
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
            self.tags.push(FsTag::of(path));
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
    fn path_of(&self, ino: u64, tag: &FsTag, out: &mut String) -> bool {
        out.clear();
        let Some(id) = self.node_of(ino, tag) else {
            return false;
        };
        self.write_path(id, out)
    }

    fn node_of(&self, ino: u64, tag: &FsTag) -> Option<u32> {
        self.devices
            .iter()
            .zip(&self.tags)
            .filter(|(_, mine)| mine.is_none_or(|m| m.holds(tag)))
            .find_map(|(&dev, _)| self.by_key.get(&DirKey { dev, ino }).copied())
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
    /// Where the parent is: an inode number means nothing without it.
    parent_fs: FsTag,
    name: String,
    fresh: bool,
    is_dir: bool,
    /// A name came or went in the directory, which changes the directory's own
    /// times — and the kernel says nothing about the directory.
    entries: bool,
    /// The kernel says the content is final: a descriptor opened for writing was
    /// closed — `munmap` for a mapping. See [`crate::revisit::forget`].
    settled: bool,
}

/// Collect one window's events, and say whether anything was lost.
/// Only the kernel can say that, with its overflow record. [`MAX_SEEN`] and the
/// two-second deadline end a window, and lose nothing: the group's queue is
/// unlimited, so what was not read waits there for the next window. They used
/// to report lost, and a `cargo clean` — 850,000 events, most under an excluded
/// `target` and counted before any rule — sent every root of every source to
/// be walked again.
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
        // The memory bound, checked before the time one — see [`MAX_SEEN`]. The
        // rest stays queued in the kernel and opens the next window.
        if seen.len() >= MAX_SEEN {
            break;
        }
        // A burst larger than the buffer is read out here, but not forever: at
        // 4.6 million events a second, two seconds is far past any real batch.
        if Instant::now() > deadline {
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
                        let half =
                            |at: usize| u32::from_le_bytes(buf[at..at + 4].try_into().unwrap());
                        let parent_fs = FsTag {
                            fsid: [half(p + 4), half(p + 8)],
                            root: btrfs_root(ht, &buf[body..body + hb]),
                        };
                        let tail = &buf[body + hb..p + info_len];
                        let n = tail.iter().position(|&c| c == 0).unwrap_or(tail.len());
                        if n > 0
                            && let Ok(name) = std::str::from_utf8(&tail[..n])
                        {
                            out.push(Seen {
                                parent_ino: ino,
                                parent_fs,
                                name: name.to_owned(),
                                fresh: mask & (FAN_CREATE | FAN_MOVED_TO) != 0,
                                is_dir: mask & FAN_ONDIR != 0,
                                entries: mask
                                    & (FAN_CREATE | FAN_DELETE | FAN_MOVED_FROM | FAN_MOVED_TO)
                                    != 0,
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

/// The path an event is about. The kernel reports an event on a directory itself
/// — its attributes, its removal — with the directory's own handle and the name
/// `.`, which read as a child made a row called `.` in every directory whose mode
/// or times changed: seven of them, removed by every reconciling walk and made
/// again by the next such event. A root is not a row of its own, so its `.` is
/// nobody's business.
fn event_path(dir: &str, name: &str, roots: &[std::path::PathBuf]) -> Option<String> {
    if name == "." {
        let own = if dir.len() > 1 {
            dir.trim_end_matches('/')
        } else {
            dir
        };
        let is_root = roots
            .iter()
            .any(|r| path::from_path(r).trim_end_matches('/') == own);
        return (!is_root).then(|| own.to_owned());
    }
    Some(if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    })
}

/// Take the descriptor the helper left, if it left one.
/// Absent is the ordinary case and not an error: [`try_start`] returns `None`.
fn inherited() -> Option<OwnedFd> {
    let raw = handed()?;
    // SAFETY: the descriptor is open — `fdinfo` for it was just read — and the
    // environment variable is the helper's contract for handing over ownership,
    // so nothing else in this process holds it.
    Some(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// Close the helper's descriptor on every exec from here on. It crossed one
/// exec to get here and must cross no other: a thumbnailer started on a
/// downloaded file would otherwise read every name created on the disk.
/// Called first thing, since a service that watches nothing never takes it.
pub fn keep_from_children() {
    if let Some(raw) = handed() {
        // SAFETY: `handed` checked that the descriptor is open; only its flags change.
        unsafe {
            let f = libc::fcntl(raw, libc::F_GETFD);
            if f >= 0 {
                libc::fcntl(raw, libc::F_SETFD, f | libc::FD_CLOEXEC);
            }
        }
    }
}

/// The descriptor the variable names, once it is known to be a group opened
/// the way the reader needs.
fn handed() -> Option<RawFd> {
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
    Some(raw)
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
    /// Cleared when the handle is dropped, and the entry taken out with it. Also
    /// what a handle finds its own entry by: a source keeps its id across an
    /// engine swap, so after one there were two entries under it, and `cover`
    /// and `retune` found the first — the dead one.
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
    /// Which source this is, for the record; the entry is found by `live`.
    #[allow(dead_code)]
    id: SourceId,
    /// Shared with this handle's entry in [`SUBS`], and how it is found there.
    live: Arc<AtomicBool>,
    /// Filesystems that appeared after the marks were set: not covered, and not
    /// coverable from here, so they are named for the layer above to report.
    uncovered: Arc<Mutex<Vec<String>>>,
}

impl WatchHandle for FanWatch {
    fn unwatched(&self) -> Vec<String> {
        self.uncovered.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// The mark already covers a directory created a moment ago; the map does not
    /// know it. A tree made in a burst — `mkdir -p`, an archive opened, a clone —
    /// reached the map as its top directory alone: the events naming the ones inside
    /// were read before the top was learned, and dropped. Everything that happened
    /// in those directories afterwards was dropped as well, until a walk of the
    /// whole source: 73,759 rows in a day here, found by the daily check. So every
    /// directory under a subtree about to be walked is learned first, by a walk of
    /// its own under no lock.
    fn cover(&self, path: &str) {
        let Some(rules) = SUBS.lock().ok().and_then(|subs| {
            subs.iter()
                .find(|s| self.owns(s))
                .map(|s| Arc::clone(&s.rules))
        }) else {
            return;
        };
        let found = directories_under(path, &rules);
        if found.is_empty() {
            return;
        }
        if let Ok(mut subs) = SUBS.lock()
            && let Some(s) = subs.iter_mut().find(|s| self.owns(s))
        {
            for (key, parent, at) in &found {
                s.map.insert_key(*key, *parent, at);
            }
        }
    }

    /// Take the new rules, and rebuild the directory map behind them: the map is how
    /// an event gets a name, and a rule switched off re-opens a subtree it never heard
    /// of. Built before the lock — walking both roots is 0.7 s warm, 0.38 s cold on NTFS.
    fn retune(&self, opts: &ScanOptions) {
        let rules = Arc::new(Rules::from_options(opts));
        let Some((roots, threads)) = SUBS.lock().ok().and_then(|subs| {
            subs.iter()
                .find(|s| self.owns(s))
                .map(|s| (s.roots.clone(), s.threads))
        }) else {
            return;
        };
        let map = DirMap::build(&roots, &rules, threads);
        if let Ok(mut subs) = SUBS.lock()
            && let Some(s) = subs.iter_mut().find(|s| self.owns(s))
        {
            s.rules = rules;
            s.map = map;
        }
    }

    /// Leave the reader running: it serves every source, so one stopping is no reason
    /// to take it down. The subscription goes, and the thread stays.
    fn stop(self: Box<Self>) {
        self.leave();
    }
}

impl FanWatch {
    fn owns(&self, sub: &Sub) -> bool {
        Arc::ptr_eq(&sub.live, &self.live)
    }

    /// Go quiet and take this entry out, map and all: a swapped-out engine's
    /// entry held a whole source's directory map, tens of megabytes, for good.
    /// The reader uses positions in [`SUBS`] only while it holds the lock.
    fn leave(&self) {
        self.live.store(false, Ordering::Relaxed);
        if let Ok(mut subs) = SUBS.lock() {
            subs.retain(|s| !self.owns(s));
        }
    }
}

impl Drop for FanWatch {
    fn drop(&mut self) {
        self.leave();
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
    // Marked or not: a group that cannot hear these roots watches nothing.
    if !crate::marks::covered(handed()?, source.roots()) {
        return None;
    }
    if let Some(deaf) = source.roots().iter().find(|r| !decodable(r)) {
        eprintln!(
            "scourd: {} cannot be watched by fanotify on this filesystem; its changes are found by walking",
            path::from_path(deaf)
        );
        return None;
    }
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
                    let gone: Vec<String> =
                        known.difference(&now).map(|(_, at)| at.clone()).collect();
                    known = now;
                    if !fresh.is_empty() || !gone.is_empty() {
                        note_mounts(&fresh, &gone);
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

        if !window(&mut seen, lost, &mut dir) {
            return;
        }
    }
}

/// One window's events: read and taught to the map under the lock, sent after it.
/// False when the lock is poisoned, which ends the reader.
fn window(seen: &mut Vec<Seen>, lost: bool, dir: &mut String) -> bool {
    // **The lock is for the map, and nothing is sent while it is held.** A send
    // waits while the engine's queue is full, and the engine's worker takes this
    // lock to learn a subtree before walking it: the two waited on each other for
    // good, and nothing was indexed again until a restart. So the window is read
    // and the map taught under the lock, and what to look at is done after it.
    let mut looks: Vec<Look> = Vec::new();
    let mut rescans: Vec<(Arc<dyn ChangeSink>, String)> = Vec::new();
    {
        let Ok(mut subs) = SUBS.lock() else {
            return false;
        };

        if lost {
            // Nothing in the overflow record says what was missed, so the subtree is the
            // only unit available — for everyone, because the queue was shared.
            for s in subs.iter().filter(|s| s.live.load(Ordering::Relaxed)) {
                for r in &s.roots {
                    rescans.push((Arc::clone(&s.sink), path::from_path(r)));
                }
            }
        } else {
            // Distinct paths a subscriber, keeping "this might be new" if any event said so.
            let mut batch: HashMap<(usize, String), (bool, bool, bool)> = HashMap::new();
            // Directories whose names changed, looked at once each after their children.
            let mut parents: std::collections::HashSet<(usize, String)> = Default::default();
            for ev in seen.drain(..) {
                // The subscriber that walked this directory owns the path. An event nobody
                // recognises is **dropped, not escalated**: it is almost always another
                // source's tree, and a rescan of every root would turn traffic into a storm.
                let Some(i) = subs.iter().enumerate().find_map(|(i, s)| {
                    if !s.live.load(Ordering::Relaxed) {
                        return None;
                    }
                    s.map
                        .path_of(ev.parent_ino, &ev.parent_fs, dir)
                        .then_some(i)
                }) else {
                    continue;
                };
                let Some(full) = event_path(dir, &ev.name, &subs[i].roots) else {
                    continue;
                };
                if subs[i].rules.excludes_path(&full) {
                    continue;
                }
                // Its row's times moved with the name; a root has no row.
                if ev.entries
                    && ev.name != "."
                    && let Some(own) = event_path(dir, ".", &subs[i].roots)
                {
                    parents.insert((i, own));
                }
                let e = batch.entry((i, full)).or_insert((false, false, false));
                e.0 |= ev.fresh;
                e.1 |= ev.is_dir;
                e.2 |= ev.settled;
            }

            // A directory that also spoke for itself is looked at with its own event.
            parents.retain(|k| !batch.contains_key(k) && !subs[k.0].rules.excludes_path(&k.1));

            for ((i, full), (fresh, is_dir, settled)) in batch {
                let s = &mut subs[i];
                if fresh && is_dir {
                    s.map.learn(&full);
                }
                looks.push(Look {
                    sink: Arc::clone(&s.sink),
                    id: s.id,
                    real_modes: s.real_modes,
                    path: full,
                    fresh,
                    settled: Some(settled),
                });
            }

            // One `stat` a directory a window, for its times: a folder sorted by date
            // stayed where it was when its last child came or went, until a walk —
            // most of the rows a reconciling walk found changed were directories.
            for (i, dir) in parents {
                let s = &subs[i];
                looks.push(Look {
                    sink: Arc::clone(&s.sink),
                    id: s.id,
                    real_modes: s.real_modes,
                    path: dir,
                    fresh: false,
                    settled: None,
                });
            }
        }
    }

    for (sink, at) in rescans {
        sink.emit(Change::Rescan { path: at });
    }
    for l in looks {
        let md = crate::watch::look(l.id, l.real_modes, &l.path, l.fresh, l.sink.as_ref());
        // A write through a mapping produces no event, so a path that has just spoken
        // is worth another look — unless the kernel has said the content is final.
        match l.settled {
            Some(true) => crate::revisit::forget(&l.path),
            Some(false) => crate::revisit::note(&l.path, l.id, l.real_modes, &l.sink, md.as_ref()),
            None => {}
        }
    }
    true
}

/// One path a window has to look at, carried out of the lock with what looking needs.
/// `settled` is whether the kernel said the content is final; `None` for a
/// directory looked at only for its times.
struct Look {
    sink: Arc<dyn ChangeSink>,
    id: SourceId,
    real_modes: bool,
    path: String,
    fresh: bool,
    settled: Option<bool>,
}

/// A filesystem that appeared after the marks were set.
/// Its contents can still be indexed, but nothing here can watch it: a new
/// filesystem is a new superblock and a mark needs a privilege this process lacks.
/// A mount that went away is covered again — the directory under it is on the
/// marked filesystem — and is looked at again, since its contents changed back.
///
/// Only mounts under a source's roots are that source's business. Every new
/// mount on the machine used to count against every source: systemd mounting
/// `/efi` when something read it left all three unwatched until a restart, and
/// each was walked whole whenever its write counter moved — every minute.
fn note_mounts(fresh: &[String], gone: &[String]) {
    // Sent after the lock, as the reader's window is: see there.
    let mut rescans: Vec<(Arc<dyn ChangeSink>, String)> = Vec::new();
    {
        let Ok(subs) = SUBS.lock() else { return };
        for s in subs.iter().filter(|s| s.live.load(Ordering::Relaxed)) {
            let fresh: Vec<String> = fresh
                .iter()
                .filter(|at| !s.rules.excludes_path(at))
                .cloned()
                .collect();
            let look = match s.uncovered.lock() {
                Ok(mut u) => cover_mounts(&mut u, &s.roots, &fresh, gone),
                Err(_) => continue,
            };
            rescans.extend(look.into_iter().map(|at| (Arc::clone(&s.sink), at)));
        }
    }
    for (sink, at) in rescans {
        sink.emit(Change::Rescan { path: at });
    }
}

/// What one source makes of mounts that came and went: the new ones under its
/// roots join what it cannot watch, the vanished ones leave, and both are to be
/// walked again. Returns the paths to walk.
fn cover_mounts(
    uncovered: &mut Vec<String>,
    roots: &[std::path::PathBuf],
    fresh: &[String],
    gone: &[String],
) -> Vec<String> {
    let under = |at: &str| {
        roots
            .iter()
            .any(|r| std::path::Path::new(at).starts_with(r))
    };
    let mut look = Vec::new();
    for at in fresh.iter().filter(|at| under(at)) {
        if !uncovered.contains(at) {
            uncovered.push(at.clone());
        }
        look.push(at.clone());
    }
    for at in gone {
        let was = uncovered.len();
        uncovered.retain(|u| u != at);
        if uncovered.len() != was || under(at) {
            look.push(at.clone());
        }
    }
    look
}

#[cfg(test)]
mod tests;
