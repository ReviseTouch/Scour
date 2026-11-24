use super::*;

/// The tests that use the one [`SUBS`] list, one at a time: the lock test
/// asks whether anyone holds that lock, and a neighbour holding it for its
/// own reasons made it fail.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// What stopped indexing for good: a send under the lock waited on a full
/// queue that the worker could not drain while it waited for the lock. Every
/// send a window makes, an overflow's included, must find the lock free.
#[test]
fn a_window_sends_nothing_while_it_holds_the_lock() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|p| p.into_inner());
    #[derive(Debug, Default)]
    struct Probe {
        sent: std::sync::atomic::AtomicUsize,
        locked: std::sync::atomic::AtomicUsize,
    }
    impl ChangeSink for Probe {
        fn emit(&self, _change: Change) {
            self.sent.fetch_add(1, Ordering::Relaxed);
            if SUBS.try_lock().is_err() {
                self.locked.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    let tmp = tempfile::tempdir().expect("tmp");
    let root = tmp.path().join("kok");
    std::fs::create_dir_all(root.join("yeni")).expect("mkdir");
    std::fs::write(root.join("f.txt"), b"x").expect("write");
    let id = SourceId(4242);
    let probe = Arc::new(Probe::default());
    let mut map = DirMap::default();
    let md = std::fs::symlink_metadata(&root).expect("stat");
    map.insert(&md, None, &path::from_path(&root));
    {
        use std::os::unix::fs::MetadataExt;
        let ino = md.ino();
        SUBS.lock().expect("subs").push(Sub {
            id,
            real_modes: true,
            rules: Arc::new(Rules::from_options(&ScanOptions::default())),
            sink: probe.clone(),
            roots: vec![root.clone()],
            map,
            threads: 1,
            live: Arc::new(AtomicBool::new(true)),
            uncovered: Arc::default(),
        });
        let event = |name: &str, fresh: bool, is_dir: bool| Seen {
            parent_ino: ino,
            parent_fs: FsTag::default(),
            name: name.into(),
            fresh,
            is_dir,
            entries: fresh,
            settled: true,
        };
        let mut seen = vec![event("f.txt", false, false), event("yeni", true, true)];
        let mut dir = String::new();
        assert!(window(&mut seen, false, &mut dir));
        assert!(window(&mut Vec::new(), true, &mut dir), "an overflow");
    }
    SUBS.lock().expect("subs").retain(|s| s.id != id);
    // A file, a new directory and its walk, and the overflow's walk of the root.
    assert!(probe.sent.load(Ordering::Relaxed) >= 4);
    assert_eq!(
        probe.locked.load(Ordering::Relaxed),
        0,
        "sent with the lock held"
    );
}

/// After an engine swap a source has a new subscription under the same id.
/// The new handle must teach its own map, and the old one must leave: the
/// old entry used to stay, and `cover` and `retune` found it first.
#[test]
fn a_swapped_source_teaches_its_own_map_and_the_old_one_leaves() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().expect("tmp");
    let root = tmp.path().join("kok");
    std::fs::create_dir_all(root.join("a/b")).expect("mkdir");
    #[derive(Debug)]
    struct Quiet;
    impl ChangeSink for Quiet {
        fn emit(&self, _change: Change) {}
    }
    let id = SourceId(4343);
    let sub = |live: &Arc<AtomicBool>| Sub {
        id,
        real_modes: true,
        rules: Arc::new(Rules::from_options(&ScanOptions::default())),
        sink: Arc::new(Quiet),
        roots: vec![root.clone()],
        map: DirMap::default(),
        threads: 1,
        live: Arc::clone(live),
        uncovered: Arc::default(),
    };
    let old_live = Arc::new(AtomicBool::new(true));
    let new_live = Arc::new(AtomicBool::new(true));
    SUBS.lock().expect("subs").push(sub(&old_live));
    SUBS.lock().expect("subs").push(sub(&new_live));
    let old = FanWatch {
        id,
        live: old_live,
        uncovered: Arc::default(),
    };
    let new = FanWatch {
        id,
        live: new_live,
        uncovered: Arc::default(),
    };
    Box::new(old).stop();
    new.cover(&path::from_path(&root));
    let (entries, learned) = {
        let subs = SUBS.lock().expect("subs");
        let mine: Vec<_> = subs.iter().filter(|s| s.id == id).collect();
        (
            mine.len(),
            mine.first().map(|s| s.map.by_key.len()).unwrap_or(0),
        )
    };
    drop(new);
    assert_eq!(entries, 1, "the stopped subscription is still there");
    assert!(learned >= 2, "the live map learned {learned} directories");
    assert!(
        SUBS.lock().expect("subs").iter().all(|s| s.id != id),
        "dropped, gone"
    );
}

#[test]
fn a_subtree_about_to_be_walked_is_learned_to_its_last_directory() {
    let tmp = tempfile::tempdir().expect("tmp");
    let top = tmp.path().join("t");
    std::fs::create_dir_all(top.join("a/b/c")).expect("mkdir");
    std::fs::create_dir_all(top.join("target/debug")).expect("mkdir");
    std::fs::create_dir_all(tmp.path().join("elsewhere")).expect("mkdir");
    std::fs::write(top.join("a/f.txt"), b"x").expect("write");
    std::os::unix::fs::symlink(tmp.path().join("elsewhere"), top.join("a/link")).expect("link");
    let rules = Rules::from_options(&ScanOptions {
        exclude_dirs: vec!["target".into()],
        ..ScanOptions::default()
    });
    let top_text = path::from_path(&top);
    let found = directories_under(&top_text, &rules);
    let mut names: Vec<String> = found
        .iter()
        .map(|(_, _, p)| p.trim_start_matches(top_text.as_str()).to_owned())
        .collect();
    names.sort();
    // The top, every level below it; not the file, the link or the excluded tree.
    assert_eq!(names, ["", "/a", "/a/b", "/a/b/c"]);
    // Each hangs under the directory above it, so the map can name it.
    let key_of = |rel: &str| {
        found
            .iter()
            .find(|(_, _, p)| p.trim_start_matches(top_text.as_str()) == rel)
            .map(|(k, _, _)| *k)
    };
    let parent_of = |rel: &str| {
        found
            .iter()
            .find(|(_, _, p)| p.trim_start_matches(top_text.as_str()) == rel)
            .and_then(|(_, parent, _)| *parent)
    };
    assert_eq!(parent_of("/a/b/c"), key_of("/a/b"));
    assert_eq!(parent_of("/a"), key_of(""));
    assert_eq!(parent_of(""), dir_key(tmp.path()));
}

#[test]
fn a_mount_is_the_business_of_the_source_it_lands_in_and_only_while_it_is_there() {
    let roots = [
        std::path::PathBuf::from("/usr"),
        std::path::PathBuf::from("/var"),
    ];
    let mut uncovered = Vec::new();
    // `/efi` mounted on a read: nothing to do with these roots.
    let look = cover_mounts(&mut uncovered, &roots, &["/efi".into()], &[]);
    assert!(look.is_empty() && uncovered.is_empty(), "{uncovered:?}");
    // A container's filesystem under `/var`: it cannot be watched, and it is walked.
    let look = cover_mounts(
        &mut uncovered,
        &roots,
        &["/var/lib/containers/x/merged".into()],
        &[],
    );
    assert_eq!(look, ["/var/lib/containers/x/merged"]);
    assert_eq!(uncovered, ["/var/lib/containers/x/merged"]);
    // `/varnish` is not under `/var`.
    cover_mounts(&mut uncovered, &roots, &["/varnish".into()], &[]);
    assert_eq!(uncovered.len(), 1);
    // Gone again: covered, and what was under it is walked.
    let look = cover_mounts(
        &mut uncovered,
        &roots,
        &[],
        &["/var/lib/containers/x/merged".into(), "/efi".into()],
    );
    assert_eq!(look, ["/var/lib/containers/x/merged"]);
    assert!(uncovered.is_empty());
}

#[test]
fn an_event_on_a_directory_itself_is_about_the_directory_not_a_child_called_dot() {
    let roots = [std::path::PathBuf::from("/home/u")];
    assert_eq!(
        event_path("/home/u/a/skills", ".", &roots).as_deref(),
        Some("/home/u/a/skills")
    );
    assert_eq!(
        event_path("/home/u/a", "note.txt", &roots).as_deref(),
        Some("/home/u/a/note.txt")
    );
    // A root has no row of its own, so nothing is made for it.
    assert_eq!(event_path("/home/u", ".", &roots), None);
    assert_eq!(event_path("/home/u/", ".", &roots), None);
    assert_eq!(
        event_path("/home/u", "top.txt", &roots).as_deref(),
        Some("/home/u/top.txt")
    );
}

/// [`DirMap::path_of`] as a test would rather read it: the reader keeps one
/// buffer for the life of its thread.
fn resolved(map: &DirMap, ino: u64) -> Option<String> {
    let mut out = String::new();
    map.path_of(ino, &FsTag::default(), &mut out).then_some(out)
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
            unsafe { std::str::from_utf8_unchecked(&self.chunks[slot.chunk as usize][start..end]) }
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

/// The fsid is read from the record, and a btrfs handle gives its subvolume:
/// the handle the probe above caught in `@tmp`, subvolume 261.
#[test]
fn an_event_says_which_filesystem_and_subvolume_it_is_from() {
    let mut e = event(0, "x");
    // fsid as the kernel wrote it for `@tmp` here.
    e[28..32].copy_from_slice(&0xd30e_8e93u32.to_le_bytes());
    e[32..36].copy_from_slice(&0x6f61_cd83u32.to_le_bytes());
    let handle: [u8; 20] = [
        0, 1, 0, 0, 0, 0, 0, 0, 5, 1, 0, 0, 0, 0, 0, 0, 0x0b, 0, 0, 0,
    ];
    let mut b = e[..24 + 12].to_vec();
    b.extend_from_slice(&20u32.to_le_bytes());
    b.extend_from_slice(&0x4di32.to_le_bytes());
    b.extend_from_slice(&handle);
    b.extend_from_slice(b"x\0");
    let info_len = (b.len() - 24) as u16;
    b[26..28].copy_from_slice(&info_len.to_le_bytes());
    let total = b.len() as u32;
    b[0..4].copy_from_slice(&total.to_le_bytes());
    let mut seen = Vec::new();
    assert!(parse(&b, &mut seen));
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].parent_ino, 256);
    assert_eq!(
        seen[0].parent_fs,
        FsTag {
            fsid: [0xd30e_8e93, 0x6f61_cd83],
            root: Some(261)
        }
    );
}

/// The rule, on the numbers measured here: btrfs folds the subvolume into
/// the fsid's low half, so `@home`'s directory takes an event whose fsid is
/// `@tmp`'s as long as the handle names `@home`; another filesystem does not.
#[test]
fn a_directory_takes_only_events_from_where_it_is() {
    let home = FsTag {
        fsid: [0xd30e_8e93, 0x6f61_cd87],
        root: Some(257),
    };
    let ev = |root: u64| FsTag {
        fsid: [0xd30e_8e93, 0x6f61_cd83],
        root: Some(root),
    };
    assert!(home.holds(&ev(257)));
    assert!(!home.holds(&ev(261)), "another subvolume");
    let other_btrfs = FsTag {
        fsid: [0x1234_5678, 0x6f61_cd87],
        root: Some(257),
    };
    assert!(
        !other_btrfs.holds(&ev(257)),
        "another btrfs with a subvolume 257"
    );
    let ntfs = FsTag {
        fsid: [0x5a34_36bf, 0x225a_3464],
        root: None,
    };
    assert!(ntfs.holds(&FsTag {
        fsid: ntfs.fsid,
        root: None
    }));
    assert!(!ntfs.holds(&ev(257)));
    assert!(!home.holds(&FsTag {
        fsid: ntfs.fsid,
        root: None
    }));
    assert!(
        ntfs.holds(&FsTag::default()),
        "an event without an fsid goes anywhere"
    );
}

/// Two directories with one inode number on two filesystems: an event goes
/// to the one its filesystem names, whichever was learned first. Before, the
/// first learned took it.
#[test]
fn one_inode_number_on_two_filesystems_is_two_directories() {
    let mut map = DirMap::default();
    map.insert_key(
        DirKey {
            dev: 1,
            ino: 146_968,
        },
        None,
        "/home/u/server",
    );
    map.insert_key(
        DirKey {
            dev: 2,
            ino: 146_968,
        },
        None,
        "/mnt/depo/cli",
    );
    let home = FsTag {
        fsid: [0xd30e_8e93, 0x6f61_cd87],
        root: Some(257),
    };
    let depo = FsTag {
        fsid: [0x5a34_36bf, 0x225a_3464],
        root: None,
    };
    map.tags = vec![Some(home), Some(depo)];
    let mut out = String::new();
    assert!(map.path_of(146_968, &depo, &mut out));
    assert_eq!(out, "/mnt/depo/cli");
    assert!(map.path_of(146_968, &home, &mut out));
    assert_eq!(out, "/home/u/server");
    let elsewhere = FsTag {
        fsid: [1, 2],
        root: None,
    };
    assert!(!map.path_of(146_968, &elsewhere, &mut out), "known nowhere");
}

/// A real directory answers both questions: what `of` asks the kernel works.
#[test]
fn a_directory_can_be_asked_where_it_is() {
    let tmp = tempfile::tempdir().expect("tmp");
    let tag = FsTag::of(&path::from_path(tmp.path())).expect("statfs and a handle");
    assert_ne!(tag.fsid, [0, 0]);
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
        !lost,
        "a window cut at the ceiling lost nothing: the rest wait in the kernel's \
             unlimited queue, and calling it lost walked every root again"
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

/// A burst past the ceiling is read over several windows, every event
/// once, none of them called lost.
#[test]
fn a_burst_past_the_ceiling_is_read_in_windows_and_nothing_is_lost() {
    let mut buf = vec![0u8; BUF];
    let mut seen: Vec<Seen> = Vec::new();
    let stream = buffer_of(1_000);
    // 600,000 events in the queue: more than two windows' worth.
    let mut left = 600usize;
    let mut total = 0usize;
    let mut windows = 0usize;
    loop {
        let lost = fill(&mut buf, &mut seen, |b| {
            if left == 0 {
                return 0;
            }
            left -= 1;
            b[..stream.len()].copy_from_slice(&stream);
            stream.len() as isize
        });
        assert!(!lost, "window {windows} called itself lost");
        if seen.is_empty() {
            break;
        }
        total += seen.len();
        windows += 1;
    }
    assert_eq!(total, 600_000);
    assert!(windows >= 3, "{windows} windows");
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
    use crate::fs::MediumExt;
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
                if map.path_of(ino, &FsTag::default(), &mut fresh) {
                    sink += std::hint::black_box(fresh).len();
                }
            }
        }
        row.owned = began.elapsed();
        let mut buffer = String::new();
        let began = Instant::now();
        for (map, inos) in maps.iter().zip(&order) {
            for &ino in inos {
                if map.path_of(ino, &FsTag::default(), &mut buffer) {
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
    // XFS, 64-bit inodes: ino u64 @0, generation u32 @8.
    let mut xfs = vec![0u8; 12];
    xfs[..8].copy_from_slice(&0x1_0000_0042u64.to_le_bytes());
    assert_eq!(handle_ino(0x81, &xfs), Some(0x1_0000_0042));
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
