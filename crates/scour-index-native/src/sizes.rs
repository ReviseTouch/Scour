//! What a folder weighs, as fast as reading a column.
//!
//! `usage.rs` answers this too, and answers more of it — the age spread, the
//! heaviest children, the whole tree at once — in 384 ms over this disk. That
//! is a report. A *column* needs the same number for thirty folders on a page
//! between two keystrokes, and three hundred milliseconds a folder is not a
//! column, it is a wait.
//!
//! ## Why it can be O(1)
//!
//! Directory numbers are handed out in **sorted path order**, so a subtree is
//! a contiguous run of them. Two runs, in fact — a sibling can sort between a
//! directory and its children, because `Projeler-414` falls between `Projeler`
//! and `Projeler/Belgeler` — which [`DirTable::subtree`] already knows and
//! answers with two binary searches.
//!
//! A contiguous run plus a **prefix sum over those numbers** is a subtraction.
//! So: one pass over a segment's rows to total what sits directly in each
//! directory, one pass over the directories to prefix-sum it, and after that
//! every folder in that segment costs two binary searches and some arithmetic.
//!
//! Measured on this index — 17 segments, 247,541 directories, 1.97 M rows:
//! **90 ms to build, 3.8 MB, 12.1 µs a folder, 0.36 ms for a page of thirty**.
//!
//! ## Why it stays right
//!
//! **A segment's bytes never change.** Once written it is immutable, so its
//! prefix sums are too — except for one thing, which is which of its rows are
//! still alive. That is what [`Live::deaths`] is for: a count that moves only
//! when a row is retired, compared in `O(1)`, and a segment whose count has
//! moved is rebuilt. In practice the large segments are cold and the small
//! newest one is what churns, so the steady-state cost is rebuilding a segment
//! of a few thousand rows rather than of two million.
//!
//! ## Hard links, and why this cannot simply add up sizes
//!
//! A file with four names is four rows, so summing `Disk` over a tree counts
//! its blocks four times. Each row carries its share — `disk / links` — which
//! is exactly what `usage.rs` does, and it has to be exactly what `usage.rs`
//! does or the column disagrees with the report printed beside it. There is a
//! test that holds them to each other.
//!
//! ## What the number is, and is not
//!
//! It is the size of **what this index holds** under that folder. `target/`,
//! `node_modules/` and everything else the rules exclude are not in it, so it
//! reads smaller than `du` and the interface says so rather than hiding it.
//! Making it agree with `du` would take a walk of the filesystem, which is the
//! thing this whole design exists to avoid.

use std::collections::HashMap;

use crate::columns::Field;
use crate::search::Segment;
use crate::segment::Live;

/// One segment's totals, by directory number, prefix-summed.
///
/// `disk[i]` is everything in directories `0..i`, so the total of the run
/// `a..b` is `disk[b] - disk[a]`. One extra slot at the end, which is what
/// makes that true for the last directory as well.
#[derive(Debug)]
pub struct Prefix {
    disk: Vec<u64>,
    files: Vec<u64>,
    /// What the segment's death count was when this was built. Anything else
    /// means the alive bits have moved and these numbers are stale.
    deaths: u64,
}

impl Prefix {
    fn of(seg: &Segment<'_>, deaths: u64) -> Prefix {
        let n = seg.dirs.len();
        let mut disk = vec![0u64; n + 1];
        let mut files = vec![0u64; n + 1];
        for row in 0..seg.rows() {
            // Directories are skipped, not because their own size is large but
            // because it is meaningless: a directory's `st_size` is the size of
            // its *entry table*, and adding it to a subtree total would report
            // a few kilobytes of bookkeeping as content.
            if !seg.is_alive(row) || seg.num_of(Field::IsDir, row) != 0 {
                continue;
            }
            let d = seg.dir_id(row) as usize;
            if d >= n {
                continue;
            }
            let links = seg.num_of(Field::Links, row).max(1) as u64;
            disk[d] += seg.num_of(Field::Disk, row).max(0) as u64 / links;
            files[d] += 1;
        }
        // In place, and shifted by one: after this, `v[i]` is everything
        // *before* `i`.
        for v in [&mut disk, &mut files] {
            let mut run = 0u64;
            for slot in v.iter_mut() {
                let own = *slot;
                *slot = run;
                run += own;
            }
        }
        Prefix {
            disk,
            files,
            deaths,
        }
    }

    /// This segment's share of one subtree: bytes on disk, and files.
    fn subtree(&self, seg: &Segment<'_>, path: &str) -> (u64, u64) {
        let scope = seg.dirs.subtree(path);
        let mut disk = 0;
        let mut files = 0;
        let mut add = |from: usize, to: usize| {
            if let (Some(a), Some(b)) = (self.disk.get(from), self.disk.get(to)) {
                disk += b.saturating_sub(*a);
            }
            if let (Some(a), Some(b)) = (self.files.get(from), self.files.get(to)) {
                files += b.saturating_sub(*a);
            }
        };
        // The directory's own row is not adjacent to its descendants, so it is
        // a run of one and has to be added separately. Forgetting it loses
        // whatever sits *directly* in the folder — which on a folder holding
        // only files is the entire answer, and reads as zero.
        if let Some(own) = scope.own {
            add(own as usize, own as usize + 1);
        }
        add(scope.below.start as usize, scope.below.end as usize);
        (disk, files)
    }
}

/// The prefix sums for every segment, kept until the segment changes.
#[derive(Debug, Default)]
pub struct Cache {
    /// By segment number, which is stable — a segment is never renumbered,
    /// only folded away and replaced by a new one.
    per_segment: HashMap<u64, Prefix>,
}

impl Cache {
    /// Bring the cache up to date with these segments, then answer for each
    /// path: bytes on disk, and how many files.
    ///
    /// Both in one call, because the expensive half is bringing the cache up
    /// to date and a page asks about thirty folders at once.
    pub fn subtrees(&mut self, segments: &[Live], paths: &[String]) -> Vec<(u64, u64)> {
        let mut out = vec![(0u64, 0u64); paths.len()];
        for live in segments {
            let Ok(seg) = live.view() else { continue };
            let deaths = live.deaths();
            let entry = self.per_segment.entry(live.number);
            let prefix = match entry {
                std::collections::hash_map::Entry::Occupied(mut o) => {
                    if o.get().deaths != deaths {
                        o.insert(Prefix::of(&seg, deaths));
                    }
                    o.into_mut()
                }
                std::collections::hash_map::Entry::Vacant(v) => v.insert(Prefix::of(&seg, deaths)),
            };
            for (i, path) in paths.iter().enumerate() {
                let (disk, files) = prefix.subtree(&seg, path);
                out[i].0 += disk;
                out[i].1 += files;
            }
        }
        // Segments that have been folded away. Dropped here rather than at
        // compaction time so that nothing outside this file has to remember
        // the cache exists — the cost is one pass over a map of at most a few
        // dozen entries, against a leak that grows for the life of a process.
        if self.per_segment.len() > segments.len() {
            let alive: std::collections::HashSet<u64> = segments.iter().map(|l| l.number).collect();
            self.per_segment.retain(|n, _| alive.contains(n));
        }
        out
    }

    /// What the cache is holding, for the memory line in `stats`.
    pub fn bytes(&self) -> u64 {
        self.per_segment
            .values()
            .map(|p| ((p.disk.len() + p.files.len()) * 8) as u64)
            .sum()
    }
}
