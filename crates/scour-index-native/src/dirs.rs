//! The directory table.
//!
//! The largest single saving in the whole design, and the simplest. On the
//! real corpus, 631,008 entries live in 73,434 directories — 8.6 files each.
//! Storing the directory once and giving every entry a number into this table
//! costs **3.63 bytes an entry** against 117.7 for the raw path.
//!
//! Front coding does the rest of the work: the table is sorted, so each row
//! records how many bytes it shares with the row before it and then only the
//! part that differs. `/home/u/Projeler/Scour/crates/scour-core/src` followed
//! by `/home/u/Projeler/Scour/crates/scour-core/tests` costs five bytes for
//! the second.
//!
//! What this buys beyond space is the operation a search engine is worst at:
//! **renaming a folder is a change to one row.** Every entry beneath it keeps
//! its number and its name; nothing is reindexed. In the engine this replaces,
//! the same operation was a delete and re-add of every descendant.

use std::collections::HashMap;

use crate::varint;

/// Rows are front-coded against the previous one, and every `RESTART` rows the
/// coding restarts from nothing.
///
/// Without restarts, reading row 70,000 means decoding 70,000 rows. With them
/// it means a binary search over the restart points and then at most 15 steps.
/// Sixteen is small enough that lookups stay cheap and large enough that the
/// restart rows — which store their whole path — stay a fifteenth of the
/// table.
const RESTART: usize = 16;

/// Builds the table. Paths are added in any order and sorted at the end.
#[derive(Debug, Default)]
pub struct DirWriter {
    paths: Vec<String>,
    seen: HashMap<String, u32>,
}

impl DirWriter {
    pub fn new() -> DirWriter {
        DirWriter::default()
    }

    /// The number for this directory, adding it if it is new.
    ///
    /// Returns a *provisional* number: the table is sorted when it is written,
    /// so [`DirWriter::finish`] hands back the map from provisional to final.
    pub fn intern(&mut self, path: &str) -> u32 {
        if let Some(&id) = self.seen.get(path) {
            return id;
        }
        let id = self.paths.len() as u32;
        self.paths.push(path.to_owned());
        self.seen.insert(path.to_owned(), id);
        id
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// Encode the table, and return the mapping from the numbers handed out by
    /// [`DirWriter::intern`] to the ones in the encoded table.
    ///
    /// Sorting is not decoration: front coding only pays when neighbours share
    /// a prefix, and it is what lets a subtree be found by binary search
    /// instead of a scan.
    pub fn finish(self) -> (Vec<u8>, Vec<u32>) {
        let mut order: Vec<u32> = (0..self.paths.len() as u32).collect();
        order.sort_unstable_by(|&a, &b| self.paths[a as usize].cmp(&self.paths[b as usize]));

        let mut remap = vec![0u32; self.paths.len()];
        for (final_id, &provisional) in order.iter().enumerate() {
            remap[provisional as usize] = final_id as u32;
        }

        let mut rows = Vec::new();
        let mut restarts: Vec<u32> = Vec::new();
        let mut previous = "";
        for (i, &provisional) in order.iter().enumerate() {
            let path = self.paths[provisional as usize].as_str();
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

        // Layout: count, restart count, the restart offsets, then the rows.
        let mut out = Vec::with_capacity(rows.len() + restarts.len() * 4 + 16);
        out.extend_from_slice(&(order.len() as u32).to_le_bytes());
        out.extend_from_slice(&(restarts.len() as u32).to_le_bytes());
        for r in &restarts {
            out.extend_from_slice(&r.to_le_bytes());
        }
        out.extend_from_slice(&rows);
        (out, remap)
    }
}

/// A directory table, read in place out of a mapped file.
#[derive(Debug, Clone, Copy)]
pub struct DirTable<'a> {
    count: usize,
    restarts: &'a [u8],
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
        if bytes.len() < end {
            return None;
        }
        Some(DirTable {
            count,
            restarts: &bytes[8..end],
            rows: &bytes[end..],
        })
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

    /// The path with this number.
    ///
    /// Decodes from the nearest restart, so at most `RESTART - 1` steps.
    pub fn get(&self, id: u32) -> Option<String> {
        let id = id as usize;
        if id >= self.count {
            return None;
        }
        let block = id / RESTART;
        let mut at = self.restart_at(block)?;
        let mut path = String::new();
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
        Some(path)
    }

    /// Every directory number at or beneath `prefix`.
    ///
    /// The table is sorted, so a subtree is a contiguous run and this is a
    /// binary search followed by a walk. It is how a folder is deleted or
    /// scoped to without touching a single entry.
    pub fn subtree(&self, prefix: &str) -> std::ops::Range<u32> {
        let prefix = prefix.trim_end_matches('/');
        let start = self.lower_bound(prefix);
        let mut end = start;
        while (end as usize) < self.count {
            match self.get(end) {
                Some(p) if under(&p, prefix) => end += 1,
                _ => break,
            }
        }
        start..end
    }

    /// The first number whose path is not less than `prefix`.
    fn lower_bound(&self, prefix: &str) -> u32 {
        let (mut lo, mut hi) = (0usize, self.count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            match self.get(mid as u32) {
                Some(p) if p.as_str() < prefix => lo = mid + 1,
                _ => hi = mid,
            }
        }
        lo as u32
    }
}

/// Is `path` inside `prefix`, or the prefix itself?
///
/// By component, not by characters: `/ab` is not inside `/a`. The mistake this
/// prevents is the one every path-prefix comparison makes once.
fn under(path: &str, prefix: &str) -> bool {
    if prefix.is_empty() {
        return true;
    }
    path == prefix || (path.starts_with(prefix) && path.as_bytes().get(prefix.len()) == Some(&b'/'))
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
    fn a_subtree_is_a_contiguous_range() {
        // What makes deleting or scoping to a folder O(1) in the entries.
        let paths = ["/a", "/a/x", "/a/x/deep", "/a/y", "/ab", "/ab/z", "/b"];
        let (bytes, _) = build(&paths);
        let table = DirTable::open(&bytes).expect("open");

        let range = table.subtree("/a");
        let got: Vec<String> = range.map(|i| table.get(i).expect("row")).collect();
        assert_eq!(got, vec!["/a", "/a/x", "/a/x/deep", "/a/y"]);

        // The mistake this exists to prevent.
        assert!(
            !got.iter().any(|p| p.starts_with("/ab")),
            "/ab is not inside /a"
        );

        assert_eq!(table.subtree("/b").count(), 1);
        assert_eq!(table.subtree("/nowhere").count(), 0);
    }

    #[test]
    fn front_coding_actually_shrinks_it() {
        // The claim is 117.7 bytes a path down to a few. Check the direction
        // on a shape like the real corpus: deep, repetitive, sorted.
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
    fn an_empty_table_is_not_a_panic() {
        let (bytes, remap) = build(&[]);
        let table = DirTable::open(&bytes).expect("open");
        assert!(table.is_empty());
        assert_eq!(table.get(0), None);
        assert_eq!(table.subtree("/a").count(), 0);
        assert!(remap.is_empty());
        assert_eq!(DirTable::open(&[1, 2, 3]).map(|t| t.len()), None);
    }
}
