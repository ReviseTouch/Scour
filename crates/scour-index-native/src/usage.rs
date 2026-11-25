//! What a subtree weighs, without walking the filesystem.
//!
//! Directory numbers are handed out in sorted path order, so the table is a
//! depth-first walk and a stack of open ancestors is the hierarchy. Own totals
//! merge by path across segments first: a directory is numbered per segment.

use std::collections::HashMap;
use std::time::Instant;

use scour_core::{AGE_BANDS, DirUsage, Result, UsageRequest, UsageResponse};

use crate::columns::Field;
use crate::search::{Plan, Segment, walk_matches};

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

/// The accumulator a caller drives one segment at a time:
/// [`Rollup::add_segment`] is the linear pass over rows, and [`Rollup::finish`]
/// sorts the paths and walks the tree.
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
    /// Directory numbers are resolved to paths once each, not once per row, and
    /// a directory is entered whether or not it holds a match: dropping the
    /// empty ones rolls a match up into a grandparent. The directory loop is
    /// the cost, not the row walk — 376 ms unfiltered over 2,228,623 rows and
    /// 243 ms for a query matching nothing; scoped to a folder, 57 and 13.9 ms.
    pub fn add_segment(&mut self, seg: &Segment<'_>) -> Result<()> {
        let n_dirs = seg.dirs.len();
        let mut per_dir = vec![Own::default(); n_dirs];
        let mut wanted = vec![true; n_dirs];
        if !self.scope.is_empty() {
            let scope = seg.dirs.subtree(&self.scope);
            for (id, w) in wanted.iter_mut().enumerate() {
                *w = scope.contains(id as u32);
            }
        }

        let plan = Plan::compile(&self.req.query, seg)?;
        if plan.is_empty() {
            for row in 0..seg.rows() {
                if seg.is_alive(row) {
                    charge(seg, row, &mut per_dir, &wanted, self.now);
                }
            }
        } else {
            walk_matches(seg, &plan, |row| {
                charge(seg, row, &mut per_dir, &wanted, self.now);
                true
            });
        }

        for (id, o) in per_dir.iter().enumerate() {
            // A directory with nothing directly in it is still an ancestor the
            // rollup needs, and its path is what the stack closes against.
            if !wanted[id] {
                continue;
            }
            let Some(path) = seg.dirs.get(id as u32) else {
                continue;
            };
            self.own.entry(path).or_default().add(o);
        }
        Ok(())
    }

    /// Roll the own-totals up the tree and answer.
    pub fn finish(mut self, started: Instant) -> Result<UsageResponse> {
        let mut dirs: Vec<(String, Own)> = std::mem::take(&mut self.own).into_iter().collect();
        dirs.sort_unstable_by(|a, b| a.0.cmp(&b.0));

        // Sorted by path: a directory not under the top of the stack closes it,
        // and closing adds its total to whatever is below.
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

        // With no scope there is no single root: taking the first row gives
        // `/home` alone on an index of `/home/hasan` and `/mnt/depo`. An
        // unscoped roll-up sums the *maximal* directories — those with no
        // ancestor in the set — under a root named by the empty path.
        let (root, mut children) = if self.scope.is_empty() {
            // Every maximal so far, not just the last: `-` is 0x2D and `/` is
            // 0x2F, so `/a-b` sorts between `/a` and `/a/x`. The list holds the
            // source roots, so this stays linear in the rows.
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

/// A scoped report, which is what every caller but `scour du` with no path
/// asks for: the scope's total and its children's. **Kept per child, not per
/// directory.** The general roll-up gave every directory of the scope a path
/// and a total — 700,000 strings in a map under a home folder, a 280 MB peak
/// and 670 ms — and then read two levels of it. Here a directory is sent to
/// the child it lies under as the table is read in order, and a row is
/// charged to that child: a few dozen totals, merged by name across segments.
pub struct Children<'a> {
    req: &'a UsageRequest,
    now: i64,
    scope: String,
    /// The scope's own row, wherever any segment holds a directory of it.
    root: Option<Own>,
    /// By child name: what lies at or below `scope/name`.
    kids: HashMap<String, Own>,
}

/// Where a directory's rows are charged: not wanted, the scope itself, or the
/// child with this number in the segment's list.
const OUT: u32 = u32::MAX;
const ROOT: u32 = u32::MAX - 1;

impl<'a> Children<'a> {
    /// `None` for a request this cannot answer: one without a scope, whose
    /// root is every source's and whose children are the maximal directories.
    pub fn new(req: &'a UsageRequest, now: i64) -> Option<Children<'a>> {
        let scope = scope_of(&req.path);
        (!scope.is_empty()).then(|| Children {
            req,
            now,
            scope,
            root: None,
            kids: HashMap::new(),
        })
    }

    pub fn add_segment(&mut self, seg: &Segment<'_>) -> Result<()> {
        let n_dirs = seg.dirs.len();
        let within = seg.dirs.subtree(&self.scope);
        if within.own.is_none() && within.below.is_empty() {
            return Ok(());
        }
        let mut bucket = vec![OUT; n_dirs];
        if let Some(own) = within.own
            && let Some(slot) = bucket.get_mut(own as usize)
        {
            *slot = ROOT;
        }
        // The children this segment knows, in the order first met. A
        // directory's path is `scope/child[/…]`; runs of one child are long,
        // so the last child is compared before the map is asked.
        let mut names: Vec<Box<[u8]>> = Vec::new();
        let mut number: HashMap<Box<[u8]>, u32> = HashMap::new();
        let head = self.scope.trim_end_matches('/').len() + 1;
        let below = within.below.start as usize..within.below.end as usize;
        seg.dirs.each_bytes_in(below, |id, path| {
            let Some(rest) = path.get(head..) else {
                return;
            };
            let child = rest.split(|&b| b == b'/').next().unwrap_or(rest);
            let at = match names.last() {
                Some(last) if &**last == child => names.len() as u32 - 1,
                _ => match number.get(child) {
                    Some(&at) => at,
                    None => {
                        let at = names.len() as u32;
                        names.push(child.into());
                        number.insert(child.into(), at);
                        at
                    }
                },
            };
            if let Some(slot) = bucket.get_mut(id) {
                *slot = at;
            }
        });

        let mut root = Own::default();
        let mut kids = vec![Own::default(); names.len()];
        let mut charge_row = |row: usize| {
            let Some(own) = cost(seg, row, self.now) else {
                return;
            };
            match bucket.get(seg.dir_id(row) as usize).copied() {
                Some(ROOT) => root.add(&own),
                Some(OUT) | None => {}
                Some(at) => kids[at as usize].add(&own),
            }
        };
        let plan = Plan::compile(&self.req.query, seg)?;
        if plan.is_empty() {
            for row in 0..seg.rows() {
                if seg.is_alive(row) {
                    charge_row(row);
                }
            }
        } else {
            walk_matches(seg, &plan, |row| {
                charge_row(row);
                true
            });
        }

        // A directory the scope holds, even an empty one, is a child the old
        // roll-up listed, and the scope is a root once any segment holds it.
        self.root.get_or_insert_with(Own::default).add(&root);
        for (name, own) in names.iter().zip(kids) {
            self.kids
                .entry(String::from_utf8_lossy(name).into_owned())
                .or_default()
                .add(&own);
        }
        Ok(())
    }

    pub fn finish(self, started: Instant) -> Result<UsageResponse> {
        let Some(mut total) = self.root else {
            return Ok(UsageResponse {
                took_us: started.elapsed().as_micros() as u64,
                ..UsageResponse::default()
            });
        };
        let usage = |path: String, o: &Own| DirUsage {
            path,
            bytes: o.bytes,
            disk: o.disk,
            files: o.files,
            age: o.age,
        };
        for own in self.kids.values() {
            total.add(own);
        }
        let mut children: Vec<DirUsage> = self
            .kids
            .iter()
            .map(|(name, own)| {
                let base = self.scope.trim_end_matches('/');
                usage(format!("{base}/{name}"), own)
            })
            .collect();
        let child_count = children.len() as u32;
        children.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes).then(a.path.cmp(&b.path)));
        children.truncate(self.req.top.max(1) as usize);
        Ok(UsageResponse {
            root: usage(self.scope.clone(), &total),
            children,
            child_count,
            took_us: started.elapsed().as_micros() as u64,
        })
    }
}

/// A report with no scope, which `scour du` and a report opened on everything
/// ask for: every source's total, and its maximal directories — those with no
/// ancestor anywhere in the index, here one, `/`. The general roll-up named
/// and totalled every directory to find them, a 324 MB peak. A directory is
/// sent to the topmost of its ancestors this segment's table holds instead,
/// and the few tops are merged across segments by path.
pub struct Everything<'a> {
    req: &'a UsageRequest,
    now: i64,
    /// Every segment's tops, by path, with what lies at or below them there.
    tops: HashMap<String, Own>,
}

impl<'a> Everything<'a> {
    pub fn new(req: &'a UsageRequest, now: i64) -> Option<Everything<'a>> {
        scope_of(&req.path).is_empty().then(|| Everything {
            req,
            now,
            tops: HashMap::new(),
        })
    }

    /// `false` for a segment the table cannot be read through, which the caller
    /// then answers the general way.
    pub fn add_segment(&mut self, seg: &Segment<'_>) -> Result<bool> {
        let Some(ancestry) = seg.dirs.ancestry() else {
            return Ok(false);
        };
        let n = seg.dirs.len();
        // Ancestors have smaller numbers, so one pass down the table hands each
        // directory its top's bucket.
        let mut tops: Vec<u32> = Vec::new();
        let mut bucket = vec![0u32; n];
        for id in 0..n {
            match ancestry.up[id] {
                crate::dirs::NO_DIR => {
                    bucket[id] = tops.len() as u32;
                    tops.push(id as u32);
                }
                up => bucket[id] = bucket[up as usize],
            }
        }
        let mut owns = vec![Own::default(); tops.len()];
        let mut charge_row = |row: usize| {
            if let Some(own) = cost(seg, row, self.now)
                && let Some(&at) = bucket.get(seg.dir_id(row) as usize)
            {
                owns[at as usize].add(&own);
            }
        };
        let plan = Plan::compile(&self.req.query, seg)?;
        if plan.is_empty() {
            for row in 0..seg.rows() {
                if seg.is_alive(row) {
                    charge_row(row);
                }
            }
        } else {
            walk_matches(seg, &plan, |row| {
                charge_row(row);
                true
            });
        }
        for (&id, own) in tops.iter().zip(&owns) {
            let path = seg.dirs.get(id).unwrap_or_default();
            self.tops.entry(path).or_default().add(own);
        }
        Ok(true)
    }

    pub fn finish(self, started: Instant) -> Result<UsageResponse> {
        let mut tops: Vec<(String, Own)> = self.tops.into_iter().collect();
        tops.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        // One segment's top may sit below another's: the maximal ones, in the
        // general roll-up's own pass, each taking what is below it.
        let mut maximal: Vec<(String, Own)> = Vec::new();
        for (path, own) in tops {
            match maximal
                .iter_mut()
                .find(|(top, _)| strictly_below(&path, top))
            {
                Some((_, total)) => total.add(&own),
                None => maximal.push((path, own)),
            }
        }
        let usage = |path: String, o: &Own| DirUsage {
            path,
            bytes: o.bytes,
            disk: o.disk,
            files: o.files,
            age: o.age,
        };
        let mut all = Own::default();
        for (_, own) in &maximal {
            all.add(own);
        }
        let child_count = maximal.len() as u32;
        let mut children: Vec<DirUsage> = maximal
            .iter()
            .map(|(path, own)| usage(path.clone(), own))
            .collect();
        children.sort_unstable_by(|a, b| b.bytes.cmp(&a.bytes).then(a.path.cmp(&b.path)));
        children.truncate(self.req.top.max(1) as usize);
        Ok(UsageResponse {
            root: usage(String::new(), &all),
            children,
            child_count,
            took_us: started.elapsed().as_micros() as u64,
        })
    }
}

/// What one row adds to the directory it sits in, or nothing for a directory
/// row: its own `size` is its entry table, not part of what it contains.
fn cost(seg: &Segment<'_>, row: usize, now: i64) -> Option<Own> {
    if seg.num_of(Field::IsDir, row) != 0 {
        return None;
    }
    // One name's share of a file that may have several.
    let links = seg.num_of(Field::Links, row).max(1) as u64;
    let bytes = seg.num_of(Field::Size, row).max(0) as u64 / links;
    let mut age = [0; AGE_BANDS];
    age[band(now - seg.num_of(Field::Mtime, row))] = bytes;
    Some(Own {
        bytes,
        disk: seg.num_of(Field::Disk, row).max(0) as u64 / links,
        files: 1,
        age,
    })
}

/// Add one row to the directory it sits in. Shared by both walks, so a filtered
/// report and an unfiltered one cannot drift apart; what a row weighs is
/// [`cost`]'s, shared with [`Children`].
fn charge(seg: &Segment<'_>, row: usize, per_dir: &mut [Own], wanted: &[bool], now: i64) {
    let d = seg.dir_id(row) as usize;
    if d >= per_dir.len() || !wanted[d] {
        return;
    }
    if let Some(own) = cost(seg, row, now) {
        per_dir[d].add(&own);
    }
}

/// The directory a request names, without a trailing slash; `/` stays `/`, the
/// filesystem's root, and only the empty path means every source at once.
fn scope_of(path: &str) -> String {
    match path.trim_end_matches('/') {
        "" if !path.is_empty() => "/".to_owned(),
        trimmed => trimmed.to_owned(),
    }
}

/// Is `path` **strictly** below `prefix`? Not [`scour_core::under`]: a rollup
/// asks what a directory *contains*, so it is not one of its own children.
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
mod tests;
