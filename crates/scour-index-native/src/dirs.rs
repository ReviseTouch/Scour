//! The directory table.
//!
//! 631,008 entries live in 73,434 directories, so storing each once and
//! numbering it costs 3.63 bytes an entry against 117.7 for the raw path. Rows
//! are front-coded and sorted, so renaming a folder changes one row.

use std::collections::HashMap;
use std::sync::Arc;

use crate::varint;

/// Rows every `RESTART` restart the front coding from nothing, so a lookup is a
/// binary search over the restarts plus at most 15 decode steps. Sixteen keeps
/// the restart rows — which store a whole path — a sixteenth of the table.
const RESTART: usize = 16;

/// A hidden or generated component counts for this many ordinary ones. At one,
/// `build/index.js` outranks the source; at six, `~/.config/fish/config.fish`
/// falls off the first page. Three sinks the caches and keeps the dotfiles.
const AWAY: u32 = 3;

/// The most steps that can count: what keeps the whole penalty under one rung
/// of the relevance score. The deepest real directory measured scores 32.
const STEP_CAP: u32 = 60;

/// Directories whose contents were generated rather than written. Not a filter:
/// a wrong name here costs a few places in the order, never a missing file.
const GENERATED: [&str; 11] = [
    "target",
    "build",
    "out",
    "dist",
    "node_modules",
    "vendor",
    "__pycache__",
    "site-packages",
    "obj",
    ".gradle",
    "cmakefiles",
];

/// How far this directory is from being something the user wrote. Every path
/// component is a step; a hidden or generated one is [`AWAY`] steps. The search
/// uses it as a tie-break within a rung of the name score, never to overturn one.
pub fn steps_of(dir: &str) -> u8 {
    let mut steps = 0u32;
    for part in dir.split('/').filter(|p| !p.is_empty()) {
        steps += if part.starts_with('.') || GENERATED.iter().any(|g| part.eq_ignore_ascii_case(g))
        {
            AWAY
        } else {
            1
        };
        if steps >= STEP_CAP {
            return STEP_CAP as u8;
        }
    }
    steps as u8
}

/// The directory part of a full path. For the merge across segments, which has
/// the path and not the number, and must agree with [`steps_of`] on the table.
pub fn dir_part(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) => "/",
        Some(i) => &path[..i],
        None => "",
    }
}

/// Builds the table. Paths are added in any order and sorted at the end.
#[derive(Debug, Default)]
pub struct DirWriter {
    paths: Vec<Arc<str>>,
    seen: HashMap<Arc<str>, u32>,
}

impl DirWriter {
    pub fn new() -> DirWriter {
        DirWriter::default()
    }

    /// The number for this directory, adding it if it is new. *Provisional*:
    /// the table is sorted when written, and `finish` returns the remapping.
    pub fn intern(&mut self, path: &str) -> u32 {
        if let Some(&id) = self.seen.get(path) {
            return id;
        }
        let id = self.paths.len() as u32;
        let path: Arc<str> = Arc::from(path);
        self.paths.push(Arc::clone(&path));
        self.seen.insert(path, id);
        id
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Encode the table, and return the remapping from provisional numbers.
    /// Sorted: front coding pays only when neighbours share a prefix, and a
    /// subtree is then found by binary search instead of a scan.
    pub fn finish(self) -> (Vec<u8>, Vec<u32>) {
        // The ordered references keep each path alive; free the table first.
        drop(self.seen);
        let mut order: Vec<u32> = (0..self.paths.len() as u32).collect();
        order.sort_unstable_by(|&a, &b| self.paths[a as usize].cmp(&self.paths[b as usize]));

        let mut remap = vec![0u32; self.paths.len()];
        for (final_id, &provisional) in order.iter().enumerate() {
            remap[provisional as usize] = final_id as u32;
        }

        let mut rows = Vec::new();
        let mut restarts: Vec<u32> = Vec::new();
        // One byte a directory, computed here because this is the only place the
        // paths exist as strings; a search reads it by number to rank a row.
        let mut pens: Vec<u8> = Vec::with_capacity(order.len());
        let mut previous = "";
        for (i, &provisional) in order.iter().enumerate() {
            let path = self.paths[provisional as usize].as_ref();
            pens.push(steps_of(path));
            let shared = if i % RESTART == 0 {
                restarts.push(rows.len() as u32);
                0
            } else {
                common_prefix(previous, path)
            };
            varint::put(&mut rows, shared as u64);
            varint::put(&mut rows, (path.len() - shared) as u64);
            rows.extend_from_slice(&path.as_bytes()[shared..]);
            previous = path;
        }

        // Layout: count, restart count, restart offsets, one distance byte a
        // directory, then the rows — which run to the end of the buffer.
        let mut out = Vec::with_capacity(rows.len() + restarts.len() * 4 + pens.len() + 16);
        out.extend_from_slice(&(order.len() as u32).to_le_bytes());
        out.extend_from_slice(&(restarts.len() as u32).to_le_bytes());
        for r in &restarts {
            out.extend_from_slice(&r.to_le_bytes());
        }
        out.extend_from_slice(&pens);
        out.extend_from_slice(&rows);
        (out, remap)
    }
}

/// A directory table, read in place out of a mapped file.
#[derive(Debug, Clone, Copy)]
pub struct DirTable<'a> {
    count: usize,
    restarts: &'a [u8],
    /// One [`steps_of`] byte a directory, in the same order as the rows.
    pens: &'a [u8],
    rows: &'a [u8],
}

impl<'a> DirTable<'a> {
    pub fn open(bytes: &'a [u8]) -> Option<DirTable<'a>> {
        if bytes.len() < 8 {
            return None;
        }
        let count = u32::from_le_bytes(bytes[0..4].try_into().ok()?) as usize;
        let n_restarts = u32::from_le_bytes(bytes[4..8].try_into().ok()?) as usize;
        let end = 8 + n_restarts * 4;
        let rows_at = end.checked_add(count)?;
        if bytes.len() < rows_at {
            return None;
        }
        Some(DirTable {
            count,
            restarts: &bytes[8..end],
            pens: &bytes[end..rows_at],
            rows: &bytes[rows_at..],
        })
    }

    /// How far this directory is from being something the user wrote: read, not
    /// computed. Zero for a number the table does not hold — no opinion.
    pub fn steps(&self, id: u32) -> u8 {
        self.pens.get(id as usize).copied().unwrap_or(0)
    }

    pub fn len(&self) -> usize {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    fn restart_at(&self, block: usize) -> Option<usize> {
        let at = block * 4;
        let b = self.restarts.get(at..at + 4)?;
        Some(u32::from_le_bytes(b.try_into().ok()?) as usize)
    }

    /// The path with this number, decoded from the nearest restart: at most
    /// `RESTART - 1` steps.
    pub fn get(&self, id: u32) -> Option<String> {
        let mut path = String::new();
        self.get_into(id, &mut path)?;
        Some(path)
    }

    /// Decode into reusable storage; private caches need no allocation on a hit.
    pub(crate) fn get_into(&self, id: u32, path: &mut String) -> Option<()> {
        path.clear();
        let id = id as usize;
        if id >= self.count {
            return None;
        }
        let block = id / RESTART;
        let mut at = self.restart_at(block)?;
        for _ in 0..=(id % RESTART) {
            let (shared, used) = varint::get(self.rows.get(at..)?)?;
            at += used;
            let (rest, used) = varint::get(self.rows.get(at..)?)?;
            at += used;
            let rest = rest as usize;
            path.truncate(shared as usize);
            path.push_str(std::str::from_utf8(self.rows.get(at..at + rest)?).ok()?);
            at += rest;
        }
        Some(())
    }

    /// How deep every directory is, by number: the count of `/` in its path.
    /// One sequential pass — a row is its predecessor truncated and extended, and
    /// slash positions are carried along — so a row costs only its own suffix.
    /// Built when a query asks about depth: 255,089 directories is half a megabyte.
    pub fn depths(&self) -> Vec<u16> {
        let mut out = Vec::with_capacity(self.count);
        let mut path = String::new();
        // Byte offsets of the `/` in `path`, which is its depth by length.
        let mut slashes: Vec<usize> = Vec::new();
        let mut at = match self.restart_at(0) {
            Some(a) => a,
            None => return out,
        };
        for _ in 0..self.count {
            let Some((shared, used)) = varint::get(self.rows.get(at..).unwrap_or_default()) else {
                break;
            };
            at += used;
            let Some((rest, used)) = varint::get(self.rows.get(at..).unwrap_or_default()) else {
                break;
            };
            at += used;
            let shared = shared as usize;
            let rest = rest as usize;
            while slashes.last().is_some_and(|&p| p >= shared) {
                slashes.pop();
            }
            path.truncate(shared);
            let Some(bytes) = self.rows.get(at..at + rest) else {
                break;
            };
            let Ok(suffix) = std::str::from_utf8(bytes) else {
                break;
            };
            for (i, b) in suffix.bytes().enumerate() {
                if b == b'/' {
                    slashes.push(shared + i);
                }
            }
            path.push_str(suffix);
            at += rest;
            out.push(slashes.len().min(u16::MAX as usize) as u16);
        }
        out
    }

    /// What every row's name is appended to, flattened, in table order, so two
    /// rows can be ordered by path without either being built (see
    /// [`crate::order`]). 257,167 directories are 13 MB, held for one sort.
    /// The offsets carry a final entry, so `at[i]..at[i + 1]` is always whole
    /// and there are always `len() + 1` of them however far the decode got.
    pub fn join_prefixes(&self) -> (Vec<u8>, Vec<u32>) {
        let mut out: Vec<u8> = Vec::new();
        let mut offsets: Vec<u32> = Vec::with_capacity(self.count + 1);
        let mut path = String::new();
        let mut at = self.restart_at(0).unwrap_or(0);
        for _ in 0..self.count {
            let Some((shared, used)) = varint::get(self.rows.get(at..).unwrap_or_default()) else {
                break;
            };
            at += used;
            let Some((rest, used)) = varint::get(self.rows.get(at..).unwrap_or_default()) else {
                break;
            };
            at += used;
            let rest = rest as usize;
            path.truncate(shared as usize);
            let Some(bytes) = self.rows.get(at..at + rest) else {
                break;
            };
            let Ok(suffix) = std::str::from_utf8(bytes) else {
                break;
            };
            path.push_str(suffix);
            at += rest;
            offsets.push(out.len() as u32);
            out.extend_from_slice(path.as_bytes());
            // The separator the join uses, and the two directories it does not:
            // no directory at all, and the root.
            if !path.is_empty() && path != "/" {
                out.push(b'/');
            }
        }
        // A short decode leaves the rest pointing at nothing, ordering those
        // rows first rather than reading past the buffer.
        offsets.resize(self.count, out.len() as u32);
        offsets.push(out.len() as u32);
        (out, offsets)
    }

    /// Every directory number at or beneath `prefix`, as **two** ranges: a
    /// sibling can sort between a directory and its children, since `-` is 0x2D
    /// and `/` is 0x2F. The descendants are contiguous as
    /// `[prefix + "/", prefix + "0")`; the directory's own row sits earlier.
    pub fn subtree(&self, prefix: &str) -> DirScope {
        let prefix = prefix.trim_end_matches('/');
        let own = self.exact(prefix);
        let below = {
            let from = self.lower_bound(&format!("{prefix}/"));
            let to = self.lower_bound(&format!("{prefix}0"));
            from..to.max(from)
        };
        DirScope { own, below }
    }

    /// The number of exactly this path, if the table holds it.
    pub fn exact(&self, path: &str) -> Option<u32> {
        let path = path.trim_end_matches('/');
        let at = self.lower_bound(path);
        (self.get(at).as_deref() == Some(path)).then_some(at)
    }

    /// The path stored at a restart, borrowed rather than built: a restart
    /// shares nothing, so its bytes are its whole path, contiguous in the map.
    fn restart_path(&self, block: usize) -> Option<&'a str> {
        let at = self.restart_at(block)?;
        let (shared, used) = varint::get(self.rows.get(at..)?)?;
        debug_assert_eq!(shared, 0, "a restart row shares nothing");
        let at = at + used;
        let (len, used) = varint::get(self.rows.get(at..)?)?;
        let at = at + used;
        std::str::from_utf8(self.rows.get(at..at + len as usize)?).ok()
    }

    /// The first number whose path is not less than `prefix`, in two levels:
    /// binary-search the *restarts*, whose paths are whole and borrowable, then
    /// walk the one block that can hold the answer through a reused buffer.
    /// Called twice per subtree and once per segment, so probing rows directly
    /// cost 75 µs of an 89 µs subtree lookup across 64 segments.
    fn lower_bound(&self, prefix: &str) -> u32 {
        if self.count == 0 {
            return 0;
        }
        let blocks = self.restarts.len() / 4;
        let (mut lo, mut hi) = (0usize, blocks);
        while lo < hi {
            let mid = (lo + hi) / 2;
            match self.restart_path(mid) {
                Some(p) if p < prefix => lo = mid + 1,
                _ => hi = mid,
            }
        }
        // `lo` is the first block starting at or after `prefix`, so the answer
        // is in the block before it, or at row zero.
        let block = lo.saturating_sub(1);
        let Some(mut at) = self.restart_at(block) else {
            return self.count as u32;
        };
        let first = block * RESTART;
        let mut path = String::new();
        for step in 0..RESTART {
            let id = first + step;
            if id >= self.count {
                break;
            }
            let Some((shared, used)) = varint::get(&self.rows[at..]) else {
                break;
            };
            at += used;
            let Some((rest, used)) = varint::get(&self.rows[at..]) else {
                break;
            };
            at += used;
            let rest = rest as usize;
            let Some(bytes) = self.rows.get(at..at + rest) else {
                break;
            };
            at += rest;
            path.truncate(shared as usize);
            let Ok(tail) = std::str::from_utf8(bytes) else {
                break;
            };
            path.push_str(tail);
            if path.as_str() >= prefix {
                return id as u32;
            }
        }
        (first + RESTART).min(self.count) as u32
    }
}

/// A directory and everything below it, as the two ranges it really is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirScope {
    /// The directory's own number, when the table holds it.
    pub own: Option<u32>,
    /// Its descendants, which *are* contiguous.
    pub below: std::ops::Range<u32>,
}

impl DirScope {
    pub fn contains(&self, id: u32) -> bool {
        self.own == Some(id) || self.below.contains(&id)
    }

    /// Could any number between `lo` and `hi` be in this scope? For the zone
    /// map: a block outside the scope holds nothing under it, unread.
    pub fn intersects(&self, lo: u32, hi: u32) -> bool {
        self.own.is_some_and(|o| lo <= o && o <= hi)
            || (self.below.start <= hi && self.below.end > lo)
    }

    /// Nothing at all — what an unknown path resolves to.
    pub fn is_empty(&self) -> bool {
        self.own.is_none() && self.below.is_empty()
    }

    pub fn len(&self) -> usize {
        usize::from(self.own.is_some()) + self.below.len()
    }

    pub fn ids(&self) -> impl Iterator<Item = u32> + '_ {
        self.own.into_iter().chain(self.below.clone())
    }
}

fn common_prefix(a: &str, b: &str) -> usize {
    let n = a
        .as_bytes()
        .iter()
        .zip(b.as_bytes())
        .take_while(|(x, y)| x == y)
        .count();
    // Never split a multi-byte character: the tail is pushed as a `str`.
    let mut n = n.min(a.len()).min(b.len());
    while n > 0 && !b.is_char_boundary(n) {
        n -= 1;
    }
    n
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build(paths: &[&str]) -> (Vec<u8>, Vec<u32>) {
        let mut w = DirWriter::new();
        for p in paths {
            w.intern(p);
        }
        w.finish()
    }

    #[test]
    fn every_path_comes_back() {
        let paths = [
            "/home/u",
            "/home/u/Projeler",
            "/home/u/Projeler/Scour",
            "/home/u/Projeler/Scour/crates",
            "/home/u/Projeler/Scour/crates/scour-core",
            "/home/u/Projeler/Scour/crates/scour-core/src",
            "/home/u/Projeler/Scour/crates/scour-core/tests",
            "/home/u/Belgeler",
            "/etc",
        ];
        let (bytes, remap) = build(&paths);
        let table = DirTable::open(&bytes).expect("open");
        assert_eq!(table.len(), paths.len());
        for (provisional, path) in paths.iter().enumerate() {
            assert_eq!(table.get(remap[provisional]).as_deref(), Some(*path));
        }
        assert_eq!(table.get(paths.len() as u32), None);
    }

    #[test]
    fn interning_the_same_directory_twice_costs_nothing() {
        let mut w = DirWriter::new();
        let a = w.intern("/home/u");
        let b = w.intern("/home/u");
        assert_eq!(a, b);
        assert_eq!(w.len(), 1);
    }

    #[test]
    fn a_subtree_survives_a_sibling_that_sorts_between_it_and_its_children() {
        // `/a-x` sorts after `/a` and before `/a/y`, so a walk that stops at
        // the first non-descendant never sees `/a/y`.
        let paths = [
            "/a",
            "/a-x",
            "/a-x/deep",
            "/a/x",
            "/a/x/deep",
            "/a/y",
            "/ab",
            "/ab/z",
            "/b",
        ];
        let (bytes, _) = build(&paths);
        let table = DirTable::open(&bytes).expect("open");

        let scope = table.subtree("/a");
        let mut got: Vec<String> = scope.ids().map(|i| table.get(i).expect("row")).collect();
        got.sort();
        assert_eq!(got, vec!["/a", "/a/x", "/a/x/deep", "/a/y"]);

        // Both near misses stay out.
        assert!(
            !got.iter().any(|p| p.starts_with("/ab")),
            "/ab is not inside /a"
        );
        assert!(
            !got.iter().any(|p| p.starts_with("/a-")),
            "/a-x is not inside /a"
        );

        assert_eq!(table.subtree("/b").len(), 1);
        assert!(table.subtree("/nowhere").is_empty());
        // A directory with no children is still itself.
        assert_eq!(table.subtree("/ab/z").len(), 1);
    }

    #[test]
    fn an_exact_lookup_does_not_match_a_longer_name() {
        let paths = ["/a", "/ab", "/abc"];
        let (bytes, _) = build(&paths);
        let table = DirTable::open(&bytes).expect("open");
        assert_eq!(
            table.exact("/a").and_then(|i| table.get(i)).as_deref(),
            Some("/a")
        );
        assert_eq!(
            table.exact("/ab/").and_then(|i| table.get(i)).as_deref(),
            Some("/ab")
        );
        assert_eq!(table.exact("/abcd"), None);
    }

    #[test]
    fn front_coding_actually_shrinks_it() {
        // 117.7 bytes a path down to a few, on a corpus-like shape.
        let mut paths = Vec::new();
        for a in 0..40 {
            for b in 0..40 {
                paths.push(format!(
                    "/home/u/Projeler/project-{a:02}/crates/module-{b:02}/src"
                ));
            }
        }
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let raw: usize = paths.iter().map(String::len).sum();
        let (bytes, _) = build(&refs);
        assert!(
            bytes.len() * 4 < raw,
            "front coding should be several times smaller: {} vs {raw}",
            bytes.len()
        );

        // And it still reads back exactly.
        let table = DirTable::open(&bytes).expect("open");
        let mut all: Vec<String> = (0..table.len() as u32)
            .map(|i| table.get(i).expect("row"))
            .collect();
        all.sort();
        let mut want = paths.clone();
        want.sort();
        assert_eq!(all, want);
    }

    #[test]
    fn non_ascii_paths_survive() {
        // A prefix must never be cut inside a character.
        let paths = [
            "/home/u/Müzik",
            "/home/u/Müzik/Şarkılar",
            "/home/u/Belgeler/İş",
        ];
        let (bytes, remap) = build(&paths);
        let table = DirTable::open(&bytes).expect("open");
        for (i, p) in paths.iter().enumerate() {
            assert_eq!(table.get(remap[i]).as_deref(), Some(*p));
        }
    }

    #[test]
    fn distance_counts_a_cache_as_further_than_a_project() {
        assert!(steps_of("/home/u/Projeler/Scour/crates") < steps_of("/home/u/.cargo/registry"));
        // A build directory is as far as a hidden one, counted per appearance.
        assert_eq!(steps_of("/home/u/p/target"), steps_of("/home/u/p/.git"));
        assert!(steps_of("/home/u/p/target/debug") > steps_of("/home/u/p/src/debug"));
        // Shallow and hidden is still close: `~/.config` is the user's writing.
        assert!(steps_of("/home/u/.config/fish") < steps_of("/home/u/.cargo/registry/src/crates"));
        // Case does not save a build directory, and the cap holds.
        assert_eq!(steps_of("/a/Target"), steps_of("/a/target"));
        assert_eq!(steps_of(&"/.x".repeat(100)), STEP_CAP as u8);
    }

    #[test]
    fn the_distance_of_every_directory_comes_back_from_the_table() {
        let paths = [
            "/home/u",
            "/home/u/Projeler/Scour/src",
            "/home/u/.cargo/registry/src/crates.io/lzma-sys-0.1.20",
            "/home/u/Projeler/Scour/target/debug/build",
        ];
        let (bytes, remap) = build(&paths);
        let table = DirTable::open(&bytes).expect("open");
        for (i, p) in paths.iter().enumerate() {
            assert_eq!(table.steps(remap[i]), steps_of(p), "{p}");
        }
        // A number the table does not hold is no opinion, not a panic.
        assert_eq!(table.steps(9_999), 0);
    }

    #[test]
    fn a_path_gives_up_its_directory() {
        assert_eq!(dir_part("/home/u/a.txt"), "/home/u");
        assert_eq!(dir_part("/a.txt"), "/");
        assert_eq!(dir_part("a.txt"), "");
    }

    /// Against brute force. One row out here is a search scoped to the wrong
    /// folder, which returns files and looks like a working search. The sizes
    /// straddle a restart boundary in both directions (`RESTART` is 16).
    #[test]
    fn the_first_row_not_below_a_prefix_is_the_one_brute_force_finds() {
        for n in [0usize, 1, 15, 16, 17, 31, 32, 33, 200] {
            let mut paths: Vec<String> = Vec::new();
            for i in 0..n {
                // The sibling that sorts between a directory and its children.
                paths.push(format!("/home/u/Projeler/p{i:03}"));
                if i % 3 == 0 {
                    paths.push(format!("/home/u/Projeler/p{i:03}-yedek"));
                    paths.push(format!("/home/u/Projeler/p{i:03}/src"));
                }
            }
            paths.sort();
            paths.dedup();
            let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
            let (bytes, _) = build(&refs);
            let t = DirTable::open(&bytes).expect("table");

            // Every stored path, extended, shortened, and two outside.
            let mut asked: Vec<String> = vec![String::new(), "/".into(), "~".into(), "/zzz".into()];
            for p in &paths {
                asked.push(p.clone());
                asked.push(format!("{p}/"));
                asked.push(format!("{p}0"));
                asked.push(p[..p.len() - 1].to_owned());
            }
            for q in &asked {
                let brute = paths.partition_point(|p| p.as_str() < q.as_str()) as u32;
                assert_eq!(t.lower_bound(q), brute, "n={n} prefix={q:?}");
            }
        }
    }

    #[test]
    fn an_empty_table_is_not_a_panic() {
        let (bytes, remap) = build(&[]);
        let table = DirTable::open(&bytes).expect("open");
        assert!(table.is_empty());
        assert_eq!(table.get(0), None);
        assert!(table.subtree("/a").is_empty());
        assert!(remap.is_empty());
        assert_eq!(DirTable::open(&[1, 2, 3]).map(|t| t.len()), None);
    }
}
