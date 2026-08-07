//! What a subtree weighs.
//!
//! The question TreeSize takes minutes to answer, because it walks the
//! filesystem. Everything it needs is already here, and two properties of the
//! layout make the answer nearly free:
//!
//! * every row carries the number of the directory it sits in, as a column;
//! * directory numbers are handed out in **sorted path order**, so the table
//!   itself is a depth-first walk of the tree and a stack of open ancestors is
//!   the whole of the hierarchy.
//!
//! So: one pass over the rows gives every directory what sits *directly* in it,
//! and one pass over the directory table rolls those into subtree totals. No
//! path is reconstructed to add up a number, and nothing is stored.
//!
//! ## Why this is not one pass per segment
//!
//! A directory has a different number in every segment that holds rows in it,
//! and the same path can be in all of them. Rolling up per segment and adding
//! the answers gives the right total for a leaf and the wrong one for anything
//! above it, because each segment's stack closes ancestors the others also
//! close. So the per-directory *own* totals are merged by path first, and the
//! rollup runs once over the merged, sorted list.
//!
//! ## Hard links, and the premise that stopped being true
//!
//! This used to say that no work was needed: a file with four names had one
//! inode, an inode was an identity, and the index held one row for it. Identity
//! became the **path**, because an inode is a promise about an object and a row
//! is a name — and the moment it did, a four-name file became four rows and
//! this file started counting its blocks four times.
//!
//! Each row carries its share instead: `disk / links`, with `links` straight
//! from `st_nlink`. The total over a tree is then the space the tree really
//! occupies, whatever order the walk took. It is not what `du` does — `du`
//! charges the whole file to whichever name it meets first, so its per-folder
//! numbers depend on traversal order — but the two agree on the total, and this
//! one does not change when a directory is renamed.
//!
//! The division truncates, so a file with an odd allocation and two names
//! loses a byte across the pair. Against block sizes of 4,096 that is not a
//! number anybody can see.

use std::collections::HashMap;
use std::time::Instant;

use scour_core::{AGE_BANDS, DirUsage, Result, UsageRequest, UsageResponse};

use crate::columns::Field;
use crate::search::Segment;

const DAY: i64 = 86_400;

/// Which age band a file modified `ago` seconds back belongs to.
fn band(ago: i64) -> usize {
    match ago {
        a if a < DAY => 0,
        a if a < 7 * DAY => 1,
        a if a < 30 * DAY => 2,
        a if a < 180 * DAY => 3,
        a if a < 365 * DAY => 4,
        _ => 5,
    }
}

/// What sits directly in one directory.
#[derive(Default, Clone, Copy)]
struct Own {
    bytes: u64,
    disk: u64,
    files: u64,
    age: [u64; AGE_BANDS],
}

impl Own {
    fn add(&mut self, other: &Own) {
        self.bytes += other.bytes;
        self.disk += other.disk;
        self.files += other.files;
        for (slot, v) in self.age.iter_mut().zip(other.age) {
            *slot += v;
        }
    }
}

/// The accumulator a caller drives one segment at a time.
///
/// Split from [`NativeIndex`] so that the merge across segments is visible:
/// [`Rollup::add_segment`] is the linear pass over rows, and
/// [`Rollup::finish`] is the one that sorts paths and walks the tree.
///
/// [`NativeIndex`]: crate::NativeIndex
pub struct Rollup<'a> {
    req: &'a UsageRequest,
    now: i64,
    scope: String,
    /// Own totals by directory path, merged across every segment.
    own: HashMap<String, Own>,
}

impl<'a> Rollup<'a> {
    pub fn new(req: &'a UsageRequest, now: i64) -> Rollup<'a> {
        Rollup {
            req,
            now,
            scope: req.path.trim_end_matches('/').to_owned(),
            own: HashMap::new(),
        }
    }

    /// Is this path the scope or below it?
    fn in_scope(&self, path: &str) -> bool {
        if self.scope.is_empty() {
            return true;
        }
        path == self.scope
            || (path.len() > self.scope.len()
                && path.starts_with(&self.scope)
                && path.as_bytes()[self.scope.len()] == b'/')
    }

    /// Accumulate one segment's rows.
    ///
    /// The directory numbers are resolved to paths once each rather than once
    /// per row — there are two orders of magnitude more rows than directories,
    /// and a path is a decode and an allocation.
    pub fn add_segment(&mut self, seg: &Segment<'_>) {
        let n_dirs = seg.dirs.len();
        let mut per_dir = vec![Own::default(); n_dirs];
        let mut wanted = vec![true; n_dirs];
        if !self.scope.is_empty() {
            let scope = seg.dirs.subtree(&self.scope);
            for (id, w) in wanted.iter_mut().enumerate() {
                *w = scope.contains(id as u32);
            }
        }

        for row in 0..seg.rows() {
            if !seg.is_alive(row) || seg.num_of(Field::IsDir, row) != 0 {
                continue;
            }
            let d = seg.dir_id(row) as usize;
            if d >= n_dirs || !wanted[d] {
                continue;
            }
            // One name's share of a file that may have several.
            let links = seg.num_of(Field::Links, row).max(1) as u64;
            let bytes = seg.num_of(Field::Size, row).max(0) as u64 / links;
            let o = &mut per_dir[d];
            o.bytes += bytes;
            o.disk += seg.num_of(Field::Disk, row).max(0) as u64 / links;
            o.files += 1;
            o.age[band(self.now - seg.num_of(Field::Mtime, row))] += bytes;
        }

        for (id, o) in per_dir.iter().enumerate() {
            // A directory with nothing directly in it still has to be here:
            // it may be an ancestor the rollup needs, and its path is what the
            // stack closes against.
            if !wanted[id] {
                continue;
            }
            let Some(path) = seg.dirs.get(id as u32) else {
                continue;
            };
            self.own.entry(path).or_default().add(o);
        }
    }

    /// Roll the own-totals up the tree and answer.
    pub fn finish(mut self, started: Instant) -> Result<UsageResponse> {
        let mut dirs: Vec<(String, Own)> = std::mem::take(&mut self.own).into_iter().collect();
        dirs.sort_unstable_by(|a, b| a.0.cmp(&b.0));

        // Sorted by path, so a stack of open ancestors is the hierarchy: a
        // directory that is not under the top of the stack closes it, and
        // closing adds its total to whatever is below.
        let mut total: Vec<Own> = dirs.iter().map(|(_, o)| *o).collect();
        let mut stack: Vec<usize> = Vec::new();
        for id in 0..dirs.len() {
            while let Some(&top) = stack.last() {
                if strictly_below(&dirs[id].0, &dirs[top].0) {
                    break;
                }
                stack.pop();
                if let Some(&parent) = stack.last() {
                    let child = total[top];
                    total[parent].add(&child);
                }
            }
            stack.push(id);
        }
        while let Some(top) = stack.pop() {
            if let Some(&parent) = stack.last() {
                let child = total[top];
                total[parent].add(&child);
            }
        }

        let usage = |i: usize| DirUsage {
            path: dirs[i].0.clone(),
            bytes: total[i].bytes,
            disk: total[i].disk,
            files: total[i].files,
            age: total[i].age,
        };

        // **With no scope there is no single root**, and taking the first row
        // as one was wrong in a way that looked right. Rows are sorted by path,
        // so on an index of `/home/hasan` and `/mnt/depo` the first row is
        // under `/home` — and the answer came back naming `/home`, totalling
        // only `/home`, and looking complete. Measured on this machine when
        // the report tab was first wired to it: 93.8 GiB reported against
        // 384.8 GiB indexed, with the larger source silently absent.
        //
        // So an unscoped roll-up sums the *maximal* directories — those with no
        // ancestor in the set — and reports them as the children of a root that
        // stands for everything. One source gives the same answer as before;
        // two give both. The empty path is what names it, because that is what
        // was asked for and any real path here would be a claim about the
        // filesystem that the index cannot make.
        let (root, mut children) = if self.scope.is_empty() {
            // **Checked against every maximal so far, not just the last one.**
            // The obvious version keeps one open directory and asks whether the
            // row is below it, which assumes a directory's descendants follow
            // it without interruption. They do not: `-` is 0x2D and `/` is
            // 0x2F, so `/a-b` sorts *between* `/a` and `/a/x`, becomes the open
            // directory, and `/a/x` is then not below it and is counted twice.
            // The test below is that exact list, and it failed on the first
            // version of this loop.
            //
            // The list it scans is the source roots — two here, and a machine
            // with a hundred separate mount points is not the shape this is
            // for — so this is linear in the rows and constant in practice.
            let mut tops: Vec<usize> = Vec::new();
            for i in 0..dirs.len() {
                if tops.iter().any(|&t| strictly_below(&dirs[i].0, &dirs[t].0)) {
                    continue;
                }
                tops.push(i);
            }
            let mut all = Own::default();
            for &i in &tops {
                all.add(&total[i]);
            }
            (
                DirUsage {
                    path: String::new(),
                    bytes: all.bytes,
                    disk: all.disk,
                    files: all.files,
                    age: all.age,
                },
                tops.into_iter().map(usage).collect::<Vec<DirUsage>>(),
            )
        } else {
            let root = (0..dirs.len())
                .find(|&i| self.in_scope(&dirs[i].0))
                .map(usage)
                .unwrap_or_default();
            let kids = (0..dirs.len())
                .filter(|&i| is_child(&dirs[i].0, &root.path))
                .map(usage)
                .collect::<Vec<DirUsage>>();
            (root, kids)
        };
        let child_count = children.len() as u32;
        children.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes).then(a.path.cmp(&b.path)));
        children.truncate(self.req.top.max(1) as usize);

        Ok(UsageResponse {
            root,
            children,
            child_count,
            took_us: started.elapsed().as_micros() as u64,
        })
    }
}

/// Is `path` **strictly** below `prefix`?
///
/// Not [`scour_core::under`], and the difference is the whole point: a rollup
/// asks what a directory *contains*, so the directory is not one of its own
/// children. Named apart from the shared one because the two were briefly
/// confused for each other, which is a compile error here and would have been
/// an off-by-one row in a total.
fn strictly_below(path: &str, prefix: &str) -> bool {
    let p = prefix.trim_end_matches('/');
    if p.is_empty() {
        return true;
    }
    path.len() > p.len() && path.starts_with(p) && path.as_bytes()[p.len()] == b'/'
}

/// Is `path` one level below `parent`?
fn is_child(path: &str, parent: &str) -> bool {
    strictly_below(path, parent) && !path[parent.trim_end_matches('/').len() + 1..].contains('/')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_maximal_directories_of_a_sorted_list_are_found_in_one_pass() {
        // What an unscoped roll-up sums. The trap is the sibling that sorts
        // between a directory and its children — `-` is 0x2D, `/` is 0x2F — so
        // `/a-b` lands after `/a` and before `/a/x`, and a scan that only ever
        // looked at the previous row would call it a child of `/a`.
        let sorted = [
            "/home",
            "/home/hasan",
            "/home/hasan/Projeler",
            "/mnt",
            "/mnt/depo",
        ];
        assert_eq!(maximal(&sorted), vec!["/home", "/mnt"]);

        assert_eq!(maximal(&["/a", "/a-b", "/a/x"]), vec!["/a", "/a-b"]);
        assert_eq!(maximal(&["/only"]), vec!["/only"]);
        assert!(maximal(&[]).is_empty());
    }

    /// The loop `finish` runs, in isolation.
    fn maximal(sorted: &[&str]) -> Vec<String> {
        let mut tops: Vec<usize> = Vec::new();
        for i in 0..sorted.len() {
            if tops.iter().any(|&t| strictly_below(sorted[i], sorted[t])) {
                continue;
            }
            tops.push(i);
        }
        tops.into_iter().map(|i| sorted[i].to_owned()).collect()
    }

    #[test]
    fn a_child_is_one_level_down_and_a_grandchild_is_not() {
        assert!(is_child("/a/b", "/a"));
        assert!(!is_child("/a/b/c", "/a"));
        assert!(!is_child("/a", "/a"));
        // The sibling that sorts between a directory and its own children, and
        // which a naive range scan swallows: `-` is 0x2D and `/` is 0x2F.
        assert!(!is_child("/a-1/b", "/a"));
        assert!(!strictly_below("/a-1", "/a"));
    }

    #[test]
    fn the_bands_are_the_six_they_claim_to_be() {
        assert_eq!(band(0), 0);
        assert_eq!(band(DAY - 1), 0);
        assert_eq!(band(DAY), 1);
        assert_eq!(band(400 * DAY), 5);
        // A file with a timestamp in the future is not older than everything.
        assert_eq!(band(-1), 0);
    }
}
