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

/// A component that is not where anyone keeps their own work counts for this
/// many ordinary ones.
///
/// Three, and the number was measured rather than picked. At one — plain depth
/// — searching `index` puts a generated `build/index.js` bundle first. At six,
/// `~/.config/fish/config.fish` falls off the first page of `config` entirely
/// and a Flutter engine `.gni` file takes its place, which is worse: a dotfile
/// under `~/.config` is the user's own writing, and only a *deep* one is not.
/// Three sinks the caches and keeps the dotfiles.
const AWAY: u32 = 3;

/// The most steps that can count. Never reached in practice — the deepest
/// directory on the machine this was measured on scores 32 out of 230,351 —
/// so it is a guarantee rather than a policy: it is what keeps the whole
/// penalty under one rung of the relevance score.
const STEP_CAP: u32 = 60;

/// Directories whose contents were generated rather than written.
///
/// Deliberately short and deliberately not a filter: this only changes the
/// *order* of results, so a name on it that should not be costs a few places
/// and never a missing file.
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

/// How far this directory is from being something the user wrote.
///
/// One number standing for what were originally three separate rules — depth,
/// hidden, build output — because measurement showed they are the same idea
/// counted in the same unit. Every path component is a step; a component that
/// is hidden or is a build directory is [`AWAY`] steps. What the number means
/// is *distance*, and the search uses it as exactly that: a tie-break within a
/// rung of the name score, never enough to overturn one.
///
/// The alternative was a penalty per reason — so much for being hidden, so
/// much for being generated, so much per level. It ranks the same results and
/// takes three constants to explain instead of one.
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

/// The directory part of a full path — everything before the last separator.
///
/// For the merge across segments, which has the path and not the directory
/// number, and must reach the same answer [`steps_of`] gave the table.
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
        // One byte a directory, computed here because this is the only place
        // the paths exist as strings. A search then reads it by number and
        // never rebuilds a path to rank a row.
        let mut pens: Vec<u8> = Vec::with_capacity(order.len());
        let mut previous = "";
        for (i, &provisional) in order.iter().enumerate() {
            let path = self.paths[provisional as usize].as_str();
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

        // Layout: count, restart count, the restart offsets, one distance byte
        // a directory, then the rows. The distances come before the rows
        // because the rows run to the end of the buffer and nothing records
        // where they stop.
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

    /// How far this directory is from being something the user wrote.
    ///
    /// Read, not computed: [`steps_of`] ran once when the table was built.
    /// Zero for a number the table does not hold, which is the same thing as
    /// no opinion.
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

    /// How deep every directory is, by number: the count of `/` in its path.
    ///
    /// One sequential pass, which is what front coding is for — a row is its
    /// predecessor truncated and extended, so the total work is the length of
    /// the suffixes rather than of the paths. Slash positions are carried
    /// along instead of being recounted, so a row costs its own suffix and
    /// nothing else.
    ///
    /// Built when a query asks about depth and not otherwise: 255,089
    /// directories is half a megabyte, which is cheap once and wasteful always.
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

    /// What every row's name is appended to, flattened, in table order.
    ///
    /// A path is a directory joined to a name, and [`crate::search::Segment`]
    /// makes that join with a separator unless the directory is empty or is the
    /// root. So a path *is* one of these followed by a name, and two rows can
    /// be ordered by their paths without either being built — see
    /// [`crate::order`].
    ///
    /// Materialised because ordering a segment by path compares millions of
    /// pairs and a front-coded row cannot be compared without being decoded
    /// first. One sequential pass, exactly as [`DirTable::depths`] does it: a
    /// row is its predecessor truncated and extended, so the whole table costs
    /// the length of the suffixes. 257,167 directories are 13 MB flattened,
    /// held for the length of one sort and then dropped.
    ///
    /// The offsets carry a final entry at the end, so `at[i]..at[i + 1]` is
    /// always the whole of one directory, and there are always `len() + 1` of
    /// them however far the decode got.
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
            // a row with no directory is its own name, and one at the root is
            // `/` and then its name.
            if !path.is_empty() && path != "/" {
                out.push(b'/');
            }
        }
        // A decode that stopped short leaves the rest of the table pointing at
        // nothing, which orders those rows first rather than reading past the
        // buffer. Damage is refused where the segment is opened, not here.
        offsets.resize(self.count, out.len() as u32);
        offsets.push(out.len() as u32);
        (out, offsets)
    }

    /// Every directory number at or beneath `prefix`.
    ///
    /// **Not one range**, and the reason is a mistake worth recording. A
    /// subtree looks like it should be contiguous in a sorted table, and it
    /// almost is — but a sibling can sort *between* a directory and its own
    /// children. `/home/u/Projeler-414` falls between `/home/u/Projeler` and
    /// `/home/u/Projeler/Belgeler`, because `-` is 0x2D and `/` is 0x2F. A
    /// walk that stops at the first non-descendant therefore stops one row in,
    /// and a search scoped to a folder silently returns only the files sitting
    /// directly in it.
    ///
    /// The descendants *are* contiguous, as `[prefix + "/", prefix + "0")` —
    /// `0` being the byte after `/`. The directory's own row sits earlier, on
    /// its own. So: two ranges, found by two binary searches.
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

    /// The path stored at a restart, borrowed rather than built.
    ///
    /// **A restart shares nothing with the row before it** — that is what a
    /// restart is — so its bytes are its whole path and sit contiguously in
    /// the mapped file. No decoding, no allocation, and this is what makes the
    /// binary search below cheap.
    fn restart_path(&self, block: usize) -> Option<&'a str> {
        let at = self.restart_at(block)?;
        let (shared, used) = varint::get(self.rows.get(at..)?)?;
        debug_assert_eq!(shared, 0, "a restart row shares nothing");
        let at = at + used;
        let (len, used) = varint::get(self.rows.get(at..)?)?;
        let at = at + used;
        std::str::from_utf8(self.rows.get(at..at + len as usize)?).ok()
    }

    /// The first number whose path is not less than `prefix`.
    ///
    /// **Two levels, and the first one touches no bytes it does not compare.**
    /// The obvious version binary-searches all `count` rows and calls
    /// [`DirTable::get`] per probe — and `get` decodes from the nearest
    /// restart and allocates a `String` every time. Over 247,769 directories
    /// that is eighteen probes, each decoding up to sixteen rows and
    /// allocating, and the whole of it is paid **twice per subtree and once
    /// per segment**: measured at 75 µs of a 89 µs subtree lookup across 64
    /// segments, and paid again by every `under:` search, which is the same
    /// call.
    ///
    /// So: binary-search the *restarts*, whose paths are already whole and
    /// borrowable — fourteen probes, no decoding, no allocation — and then
    /// walk the one block that can hold the answer, at most sixteen rows,
    /// through a single reused buffer.
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
        // `lo` is the first block that starts at or after `prefix`, so the
        // answer is inside the block before it — or at row zero, when there is
        // no block before it.
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

    /// Could any number between `lo` and `hi` be in this scope?
    ///
    /// For the zone map: a block whose directory numbers all fall outside a
    /// scope holds nothing under it, and a hundred and twenty-eight rows go
    /// without being looked at.
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
        // The bug this test was written for, found by comparing against brute
        // force: `/a-x` sorts *after* `/a` and *before* `/a/y`, because `-` is
        // 0x2D and `/` is 0x2F. A walk from `/a` that stops at the first
        // non-descendant stops at `/a-x` and never sees `/a/y` at all — so a
        // search scoped to a folder silently returned only the files sitting
        // directly in it.
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
    fn distance_counts_a_cache_as_further_than_a_project() {
        assert!(steps_of("/home/u/Projeler/Scour/crates") < steps_of("/home/u/.cargo/registry"));
        // A build directory is as far as a hidden one, and both count once
        // each time they appear.
        assert_eq!(steps_of("/home/u/p/target"), steps_of("/home/u/p/.git"));
        assert!(steps_of("/home/u/p/target/debug") > steps_of("/home/u/p/src/debug"));
        // Shallow and hidden is still close: a dotfile in `~/.config` is the
        // user's own writing and only a deep one is not.
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

    /// **Against brute force, over every path in the table and past both
    /// edges.** The two-level search reads restart rows for the outer probe
    /// and decodes the one block that can hold the answer; a mistake in either
    /// half lands one row out, and one row out in a directory table is a
    /// search scoped to the wrong folder — which returns files, silently, and
    /// looks like a working search.
    ///
    /// The sizes cross a restart boundary in both directions (RESTART is 16),
    /// so the "block before `lo`" arithmetic is exercised at the start of a
    /// block, in the middle of one, and past the last.
    #[test]
    fn the_first_row_not_below_a_prefix_is_the_one_brute_force_finds() {
        for n in [0usize, 1, 15, 16, 17, 31, 32, 33, 200] {
            let mut paths: Vec<String> = Vec::new();
            for i in 0..n {
                // Deep, shared prefixes, and the sibling that sorts *between*
                // a directory and its children — `-` is 0x2D and `/` is 0x2F.
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

            // Every stored path, every path with a byte appended, every one
            // with its last byte removed, and two that fall outside.
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
