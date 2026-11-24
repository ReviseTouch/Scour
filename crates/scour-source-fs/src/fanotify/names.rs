//! Every watched directory's name, held compactly, and the map from a handle
//! to the path it stands for.

use super::*;
use crate::path;
use crate::rules::Rules;
use std::collections::HashMap;

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
pub(super) struct Node {
    pub(super) parent: u32,
    name: NameSlot,
}

const _: () = assert!(std::mem::size_of::<Node>() == 12);

/// [`Node::parent`] for a directory that is nobody's child here: a walk root, or
/// one whose parent this map never learned. It holds its whole path as its name.
const NO_PARENT: u32 = u32::MAX;

/// How many components [`DirMap::components`] keeps on the stack.
/// Six times the measured average of 10.6, which is 512 bytes to zero a lookup.
/// Deeper is not refused, it spills — see [`DirMap::components`].
pub(super) const INLINE_DEPTH: usize = 64;

/// How far a climb follows parent links before calling the chain broken.
/// A cycle guard, not a depth limit: `PATH_MAX` is 4096 bytes and a component
/// costs at least two, so no real path reaches this.
pub(super) const MAX_DEPTH: usize = 4096;

/// Names in coarse allocations rather than one allocator object a directory.
/// Fixed chunks never need the old and the new allocation at once, and never
/// move bytes an existing slot names.
#[derive(Debug, Default)]
pub(super) struct NameArena {
    pub(super) chunks: Vec<Vec<u8>>,
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
    pub(super) fn used_bytes(&self) -> usize {
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
pub(super) fn dir_key(at: &std::path::Path) -> Option<DirKey> {
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
pub(super) fn directories_under(top: &str, rules: &Rules) -> Vec<(DirKey, Option<DirKey>, String)> {
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

/// Directory identity to path: an event names its parent by opaque file handle,
/// and this turns one into a path. No path is stored — [`Node`] holds a name and a
/// parent link — and the walk records whole trees, so a parent is always present.
#[derive(Debug, Default)]
pub(super) struct DirMap {
    pub(super) by_key: HashMap<DirKey, u32>,
    pub(super) nodes: Vec<Node>,
    pub(super) names: NameArena,
    /// Directories that are only somebody's parent — a walk root's parent, or one
    /// whose batch has not arrived yet. Deliberately not answerable by
    /// [`DirMap::path_of`]: that would name a path no scan of this source produces.
    pub(super) outside: HashMap<DirKey, u32>,
    /// Device candidates an event's inode is looked up against.
    /// The event gives an inode and an [`FsTag`] but no device number, so the
    /// inode is offered to each device whose tag says it can be there.
    pub(super) devices: Vec<u64>,
    /// Beside `devices`, one each: asked once, when the device is first met.
    pub(super) tags: Vec<Option<FsTag>>,
}

/// How many directories a walker thread gathers before handing them over.
/// A send per directory on a bounded channel cost the scan 9.5 context switches a file.
pub(super) const DIR_BATCH: usize = 512;

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
    pub(super) fn build(roots: &[std::path::PathBuf], rules: &Rules, threads: usize) -> DirMap {
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

    pub(super) fn insert(&mut self, md: &std::fs::Metadata, parent: Option<DirKey>, path: &str) {
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
    pub(super) fn insert_key(&mut self, key: DirKey, parent: Option<DirKey>, path: &str) {
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
    pub(super) fn learn(&mut self, path: &str) {
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
    pub(super) fn path_of(&self, ino: u64, tag: &FsTag, out: &mut String) -> bool {
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
