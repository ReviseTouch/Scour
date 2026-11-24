//! A page across segments: each segment's best rows, merged in the order asked.

use super::*;
use crate::columns::Field;
use crate::rank::{LiveRank, first_at_or_below};
use crate::search::Segment;
use crate::segment::Live;
use scour_core::{Entry, Hit, SearchResponse};
use std::collections::HashMap;
use std::time::Instant;

/// A row that might be on the page, ordered but not built: four numbers and a
/// key, against a `Hit`'s reconstructed path and eleven fields. A deep page holds
/// a hundred thousand of these on the way to sixty rows.
struct Candidate {
    key: crate::search::SortValue,
    /// What breaks a tie on the key: newest first, as `sort_hits` does it.
    mtime: i64,
    seg: u32,
    row: u32,
}

/// One segment's position in a merge.
struct Cursor {
    entry: Entry,
    seg: usize,
    row: usize,
}

// Ordered so that a max-heap yields the *first* row of the merged order:
// newest first, ties broken by path, which is the order `build` produces.
impl Ord for Cursor {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.entry
            .meta
            .mtime
            .cmp(&other.entry.meta.mtime)
            .then_with(|| other.entry.path.cmp(&self.entry.path))
    }
}

impl PartialOrd for Cursor {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl PartialEq for Cursor {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}

impl Eq for Cursor {}

/// Below this offset a page is walked to: the walk is a few milliseconds near
/// the top of a result, where the reach costs a rank table to be built.
const REACH_FROM: usize = 2_000;

/// Below this offset a numeric page is walked to. Gathering costs two passes
/// over every match, and the walk's zone map skips most of a segment near the
/// top: at 150,000 the walk was 102 ms and 81 MB, gathering 270 ms and 55 MB.
/// Past here what the walk keeps is what costs. `SCOUR_GATHER_FROM`
/// overrides, which is how the tests put small indexes through it.
fn numeric_from() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("SCOUR_GATHER_FROM")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(300_000)
    })
}

/// How far the merge will step through a group of rows that share a second
/// before giving up and letting the walk do it. There is no rank *within* a tie,
/// and the bound keeps the reach from ever being the slower path.
const TIE_STEPS: usize = 100_000;

/// Whether to keep every page on the walk, for measuring the reach against it.
/// `SCOUR_NO_REACH=1`, read once.
fn walk_only() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SCOUR_NO_REACH").is_some())
}

/// A page **reached** rather than walked to. The walk's cost is everything above
/// the page — 105 ms and 2,080,974 rows visited for the two hundred at offset two
/// million — where the start is a binary search plus a rank over a bitmap. `None`
/// for any shape this cannot answer, and the caller then walks.
#[allow(clippy::too_many_arguments)]
fn reach(
    segments: &[Live],
    views: &[Segment<'_>],
    descending: bool,
    offset: usize,
    limit: usize,
    cap: usize,
    started: Instant,
) -> Option<SearchResponse> {
    // Live rows before any given row, per segment. Not kept: a rank one deletion
    // stale returns a page from the wrong place.
    let ranks: Vec<LiveRank> = segments
        .iter()
        .zip(views)
        .map(|(live, seg)| LiveRank::build(seg.alive, live.rows()))
        .collect();
    let counted: usize = ranks
        .iter()
        .zip(views)
        .map(|(rank, seg)| rank.total(seg.alive))
        .sum();
    let empty = |visited: u64| SearchResponse {
        hits: Vec::new(),
        total: (counted as u64).min(cap as u64),
        capped: counted >= cap,
        took_us: started.elapsed().as_micros() as u64,
        fast_path: true,
        rows_visited: visited,
        rows_built: 0,
        misread: Vec::new(),
        similar: None,
        reading: None,
    };
    if offset >= counted {
        return Some(empty(0));
    }

    // How many live rows are newer than `t`. Two reads a segment: where the
    // column crosses `t`, and how many rows before that are live.
    let newer = |t: i64| -> usize {
        segments
            .iter()
            .zip(views)
            .zip(&ranks)
            .map(|((live, seg), rank)| {
                let at = first_at_or_below(live.rows(), t, |row| seg.num_of(Field::Mtime, row));
                rank.upto(seg.alive, at)
            })
            .sum()
    };
    // And older, for oldest first: every live row not newer than `t - 1`.
    let older = |t: i64| -> usize { counted - newer(t.saturating_sub(1)) };

    // The date of the row at `offset`, by bisection: newest first, the smallest
    // `t` with no more than `offset` rows above it; oldest first, the largest.
    // The bracket is the newest date in **any** segment and the oldest in any,
    // not the newest they share.
    let mut low = i64::MAX;
    let mut high = i64::MIN;
    for (live, seg) in segments.iter().zip(views) {
        if live.rows() > 0 {
            high = high.max(seg.num_of(Field::Mtime, 0));
            low = low.min(seg.num_of(Field::Mtime, live.rows() - 1));
        }
    }
    if low > high {
        return Some(empty(0));
    }
    while low < high {
        if descending {
            let mid = low + ((i128::from(high) - i128::from(low)) / 2) as i64;
            if newer(mid) <= offset {
                high = mid;
            } else {
                low = mid + 1;
            }
        } else {
            let mid = low + ((i128::from(high) - i128::from(low) + 1) / 2) as i64;
            if older(mid) <= offset {
                low = mid;
            } else {
                high = mid - 1;
            }
        }
    }
    let when = if descending { high } else { low };
    let above = if descending { newer(when) } else { older(when) };
    // Rows of that same date that still precede the page. There is no rank
    // inside a tie, so these are stepped through — see [`TIE_STEPS`].
    let mut skip = offset.checked_sub(above)?;
    if skip > TIE_STEPS {
        return None;
    }

    // Each segment's part of the page: the rows of one date at a time, read
    // forwards from its first row, so the path breaks a tie forwards either
    // way. Newest first the next date is the next row; oldest first it is the
    // group stored just before, found by a bisection below it.
    let mut at: Vec<Span> = segments
        .iter()
        .zip(views)
        .map(|(live, seg)| {
            let start = first_at_or_below(live.rows(), when, |row| seg.num_of(Field::Mtime, row));
            let end = if descending {
                live.rows()
            } else {
                first_at_or_below(live.rows(), when.saturating_sub(1), |row| {
                    seg.num_of(Field::Mtime, row)
                })
            };
            Span {
                row: start,
                start,
                end,
            }
        })
        .collect();
    for (i, span) in at.iter_mut().enumerate() {
        span.settle(views, i, descending);
    }

    // The merge holds one row a segment, ordered as `sort_hits` orders it: by
    // date, ties broken by paths compared as if joined.
    let mut dirs: HashMap<(usize, u32), String> = HashMap::new();
    let mut hits: Vec<Hit> = Vec::with_capacity(limit.min(counted));
    let mut visited = 0u64;
    while hits.len() < limit {
        let mut best: Option<usize> = None;
        for i in 0..views.len() {
            if at[i].row >= at[i].end {
                continue;
            }
            let Some(b) = best else {
                best = Some(i);
                continue;
            };
            if first_of(views, &mut dirs, descending, (i, at[i].row), (b, at[b].row)) {
                best = Some(i);
            }
        }
        let Some(i) = best else { break };
        let row = at[i].row;
        visited += 1;
        if skip > 0 {
            skip -= 1;
        } else {
            let seg = &views[i];
            if let Some(name) = seg.names.get(row) {
                hits.push(seg.hit(row, &name));
            }
        }
        at[i].row = row + 1;
        at[i].settle(views, i, descending);
    }

    Some(SearchResponse {
        rows_built: hits.len() as u64,
        hits,
        rows_visited: visited,
        ..empty(0)
    })
}

/// Where one segment's part of a reached page has got to: `row`, inside the
/// rows `start..end` it is reading forwards.
struct Span {
    row: usize,
    start: usize,
    end: usize,
}

impl Span {
    /// On to the next live row, and when this run is spent, newest first, the
    /// rest of the segment is the run; oldest first, the date stored before.
    /// `row == end` once nothing is left.
    pub(super) fn settle(&mut self, views: &[Segment<'_>], i: usize, descending: bool) {
        let seg = &views[i];
        loop {
            while self.row < self.end && !seg.is_alive(self.row) {
                self.row += 1;
            }
            if self.row < self.end || descending || self.start == 0 {
                return;
            }
            let date = seg.num_of(Field::Mtime, self.start - 1);
            let start =
                first_at_or_below(self.start - 1, date, |row| seg.num_of(Field::Mtime, row));
            self.end = self.start;
            self.start = start;
            self.row = start;
        }
    }
}

/// Whether `a` comes before `b` in the merged stored order, newest first or
/// oldest. The path breaks a tie forwards in both.
fn first_of(
    views: &[Segment<'_>],
    dirs: &mut HashMap<(usize, u32), String>,
    descending: bool,
    a: (usize, usize),
    b: (usize, usize),
) -> bool {
    let mtime = |(seg, row): (usize, usize)| views[seg].num_of(Field::Mtime, row);
    let by_date = mtime(a).cmp(&mtime(b));
    match if descending {
        by_date
    } else {
        by_date.reverse()
    } {
        std::cmp::Ordering::Greater => return true,
        std::cmp::Ordering::Less => return false,
        std::cmp::Ordering::Equal => {}
    }
    // The same date. Inside one segment the row number is the path order, so
    // there is nothing to compare.
    if a.0 == b.0 {
        return a.1 < b.1;
    }
    for (seg, row) in [a, b] {
        let key = (seg, views[seg].dir_id(row));
        dirs.entry(key)
            .or_insert_with(|| views[seg].dirs.get(key.1).unwrap_or_default());
    }
    let parts = |(seg, row): (usize, usize)| {
        let dir = dirs[&(seg, views[seg].dir_id(row))].as_str();
        (dir, views[seg].names.get(row).unwrap_or_default())
    };
    let ((a_dir, a_name), (b_dir, b_name)) = (parts(a), parts(b));
    joined_path(a_dir, &a_name).lt(joined_path(b_dir, &b_name))
}

/// Emit every live row of every segment, in the merged stored order. Each
/// segment is already in it, so the heap holds one row a segment.
pub(super) fn merge_rows(segs: &[&Live], views: &[Segment<'_>], emit: &mut dyn FnMut(&Entry)) {
    use std::collections::BinaryHeap;

    // Each segment is read in row order, so its names are read on from the
    // last one rather than from their block's start.
    let mut names: Vec<crate::spelling::Reader> = Vec::new();
    names.resize_with(segs.len(), Default::default);
    let mut next = |i: usize, from: usize| -> Option<(Entry, usize)> {
        let row = (from..segs[i].rows()).find(|&r| segs[i].is_alive(r))?;
        let name = names[i].read(&views[i].names, row)?;
        Some((views[i].entry_named(row, name), row))
    };

    let mut heap: BinaryHeap<Cursor> = BinaryHeap::with_capacity(segs.len());
    for i in 0..segs.len() {
        if let Some((entry, row)) = next(i, 0) {
            heap.push(Cursor { entry, seg: i, row });
        }
    }
    while let Some(c) = heap.pop() {
        emit(&c.entry);
        if let Some((entry, row)) = next(c.seg, c.row + 1) {
            heap.push(Cursor {
                entry,
                seg: c.seg,
                row,
            });
        }
    }
}

impl NativeIndex {
    /// The body of [`Index::search`].
    pub(super) fn search_segments(&self, req: &SearchRequest) -> Result<SearchResponse> {
        let started = Instant::now();
        let inner = self.inner.read();
        let offset = req.page.offset as usize;
        let limit = req.page.limit as usize;
        let need = offset + limit;
        let cap = req.page.count_cap.max(1) as usize;

        // **Candidates, not rows.** Building `offset + limit` rows a segment
        // reconstructed 401,438 front-coded paths to return sixty at offset
        // 100,000, and took 1.43 s.
        let mut pool: Vec<Candidate> = Vec::new();
        // Opened before the walk so the comparator can hold them while the pool
        // is still filling, which is what lets the pool be trimmed as it grows.
        let views: Vec<Segment<'_>> = inner
            .segments
            .iter()
            .map(|live| live.view())
            .collect::<Result<Vec<_>>>()?;

        // **A page reached instead of walked to**, for the one shape that allows
        // it: the stored order, nothing filtering, nothing concealed, deep enough
        // to be worth it. Everything else falls through to the walk unchanged.
        // Oldest first is the same order read by date the other way, with the
        // path still breaking a tie forwards: reached too.
        let stored_order = matches!(
            req.sort,
            scour_core::SortKey::Modified | scour_core::SortKey::Relevance
        ) && req.descending
            || req.sort == scour_core::SortKey::Modified;
        if offset >= REACH_FROM
            && !walk_only()
            && stored_order
            && req.query.groups.is_empty()
            && inner.hidden_prefixes.is_empty()
            && let Some(found) = reach(
                &inner.segments,
                &views,
                req.descending,
                offset,
                limit,
                cap,
                started,
            )
        {
            return Ok(found);
        }

        // The same for the three stored text orders, where only the place is
        // found: the rows around it join the pool and are ordered below.
        let deep = (offset >= REACH_FROM
            && limit > 0
            && !walk_only()
            && matches!(
                req.sort,
                scour_core::SortKey::Name | scour_core::SortKey::Path | scour_core::SortKey::Ext
            )
            && req.query.groups.is_empty())
        .then(|| {
            let dooms: Vec<Doomed<'_>> = views
                .iter()
                .map(|seg| Doomed::new(seg, &inner.hidden_prefixes))
                .collect();
            let hides: Vec<bool> = dooms.iter().map(|d| !d.is_empty()).collect();
            let hide = |s: usize, row: usize| {
                let seg = &views[s];
                dooms[s].takes(seg, row, seg.dir_id(row))
            };
            crate::deep::reach(
                &views,
                req.sort,
                req.descending,
                offset,
                limit,
                &hide,
                &hides,
            )
        })
        .flatten();
        // Where the page sits in the pool: the request's own offset after a
        // walk, a few rows in after a reach.
        let (mut offset, mut need) = match &deep {
            Some(deep) => (deep.skip, deep.skip + limit),
            None => (offset, need),
        };

        let mut counted = 0u64;
        let mut budget = cap;
        let mut visited = 0u64;
        // Paths reconstructed, page and discarded prefix alike — see
        // `SearchResponse::rows_built`.
        let mut rows = 0u64;
        // The order the merge imposes, and it is `sort_hits`'s, moved to where the
        // rows have not been built yet: text keys resolve out of the arenas where
        // their sixteen bytes agree, and the last tie is the path, compared *as if
        // joined* with a directory decoded once for every row in it.
        let exact = crate::search::key_is_exact(req.sort);
        let desc = req.descending;
        let mut dirs: HashMap<(u32, u32), String> = HashMap::new();
        let mut cmp = |a: &Candidate, b: &Candidate| {
            let mut o = a.key.cmp(&b.key);
            // `exact` says the key is an abbreviation; the sort says what it
            // abbreviates. A second inexact key added without a case here orders
            // its whole tie group by file name.
            if o.is_eq() && !exact {
                o = match req.sort {
                    scour_core::SortKey::Name => {
                        let folded = |c: &Candidate| {
                            views
                                .get(c.seg as usize)
                                .and_then(|s| s.folded.get(c.row as usize))
                                .unwrap_or_default()
                        };
                        folded(a).cmp(folded(b))
                    }
                    scour_core::SortKey::Ext => {
                        match (views.get(a.seg as usize), views.get(b.seg as usize)) {
                            (Some(a_seg), Some(b_seg)) => crate::search::compare_extensions(
                                a_seg,
                                a.row as usize,
                                b_seg,
                                b.row as usize,
                            ),
                            _ => std::cmp::Ordering::Equal,
                        }
                    }
                    _ => std::cmp::Ordering::Equal,
                };
            }
            let o = if desc { o.reverse() } else { o };
            o.then_with(|| b.mtime.cmp(&a.mtime)).then_with(|| {
                // **Inside one segment the row number is the path order.** Rows
                // are stored newest-first with the path breaking that, and the
                // dates have just tied.
                if a.seg == b.seg {
                    return a.row.cmp(&b.row);
                }
                for c in [a, b] {
                    let Some(seg) = views.get(c.seg as usize) else {
                        continue;
                    };
                    let at = (c.seg, seg.dir_id(c.row as usize));
                    dirs.entry(at)
                        .or_insert_with(|| seg.dirs.get(at.1).unwrap_or_default());
                }
                let parts = |c: &Candidate| {
                    let seg = views.get(c.seg as usize);
                    let dir = seg
                        .map(|s| dirs[&(c.seg, s.dir_id(c.row as usize))].as_str())
                        .unwrap_or_default();
                    let name = seg
                        .and_then(|s| s.names.get(c.row as usize))
                        .unwrap_or_default();
                    (dir, name)
                };
                let ((a_dir, a_name), (b_dir, b_name)) = (parts(a), parts(b));
                joined_path(a_dir, &a_name).cmp(joined_path(b_dir, &b_name))
            })
        };

        // Held for the whole loop, and only when the order depends on it.
        // A read guard, because nothing here builds: see the note at the call.
        let folders = (req.sort == scour_core::SortKey::Size).then(|| {
            // Catching up is a pass over the live bits, never a build.
            let mut cache = self.sizes.write();
            cache.fresh_rows(&inner.segments);
            parking_lot::RwLockWriteGuard::downgrade(cache)
        });
        // **A deep page in a numeric order, gathered rather than kept.** See
        // `deep::reach_numeric`. Not the date order, which is stored and keeps
        // four bytes a row even walked backwards.
        let mut gathered = false;
        if deep.is_none()
            && offset >= numeric_from()
            && limit > 0
            && !walk_only()
            && req.sort != scour_core::SortKey::Modified
            && crate::search::is_numeric(req.sort)
        {
            let plans = views
                .iter()
                .map(|seg| Plan::compile(&req.query, seg))
                .collect::<Result<Vec<_>>>()?;
            let dooms: Vec<Doomed<'_>> = views
                .iter()
                .map(|seg| Doomed::new(seg, &inner.hidden_prefixes))
                .collect();
            let under: Vec<&[(u32, i64)]> = inner
                .segments
                .iter()
                .map(|live| {
                    folders
                        .as_ref()
                        .and_then(|c| c.rows_of(live.number))
                        .unwrap_or(&[])
                })
                .collect();
            let sort = req.sort;
            let found = crate::deep::reach_numeric(req.descending, offset, limit, |f| {
                for (which, seg) in views.iter().enumerate() {
                    let doomed = &dooms[which];
                    crate::search::walk_matches(seg, &plans[which], |row| {
                        if !doomed.is_empty() && doomed.takes(seg, row, seg.dir_id(row)) {
                            return true;
                        }
                        if let Some(v) = crate::search::numeric_key(seg, row, sort, under[which]) {
                            f(which as u32, row as u32, v, seg.num_of(Field::Mtime, row));
                        }
                        true
                    });
                }
            });
            if let Some(found) = found {
                counted = found.total as u64;
                rows = inner.segments.iter().map(|live| live.rows() as u64).sum();
                visited = rows;
                pool.extend(
                    found
                        .rows
                        .iter()
                        .map(|&(seg, row, value, mtime)| Candidate {
                            key: crate::search::SortValue::Num(value),
                            mtime,
                            seg,
                            row,
                        }),
                );
                offset = found.skip;
                need = found.skip + limit;
                gathered = true;
            }
        }
        if let Some(deep) = &deep {
            counted = deep.total as u64;
            visited = deep.read;
            rows = inner.segments.iter().map(|live| live.rows() as u64).sum();
            pool.extend(deep.rows.iter().filter_map(|&(which, row)| {
                let seg = views.get(which as usize)?;
                Some(Candidate {
                    key: crate::search::merge_key(seg, row as usize, req.sort),
                    mtime: seg.num_of(Field::Mtime, row as usize),
                    seg: which,
                    row,
                })
            }));
        }
        for (which, live) in inner.segments.iter().enumerate() {
            if deep.is_some() || gathered {
                break;
            }
            rows += live.rows() as u64;
            let seg = &views[which];
            let plan = Plan::compile(&req.query, seg)?;
            let doomed = Doomed::new(seg, &inner.hidden_prefixes);
            let conceals = !doomed.is_empty();
            let mut veto = |seg: &Segment<'_>, row: usize, _name: &[u8]| {
                doomed.takes(seg, row, seg.dir_id(row))
            };
            // **A query with no conditions matches every live row**, and the
            // segment already keeps that number. Walking for the total took
            // **1.233 s** across 2,094,185 rows against 5.5 ms for the page.
            let matches_all = !conceals && plan.is_empty();
            if matches_all && need == 0 {
                // A count and nothing else. There is no page to build, so
                // there is nothing left to walk for.
                let live_rows = live.live_rows();
                counted += live_rows;
                budget = budget.saturating_sub(live_rows as usize);
                continue;
            }
            let found = run_with(
                seg,
                &plan,
                Wanted {
                    sort: req.sort,
                    descending: req.descending,
                    // Nothing is skipped per segment: the row a global offset
                    // skips may live in any of them.
                    offset: 0,
                    limit: need,
                    // What is left of the *global* count, not a fresh copy: the
                    // whole cap per segment measured thirteen times the cost of
                    // one segment on a fragmented index.
                    count_cap: if matches_all { need } else { budget },
                    rank_only: true,
                },
                conceals.then_some(&mut veto as &mut dyn FnMut(&Segment<'_>, usize, &[u8]) -> bool),
                // Only `sort:size` reads this and it never *builds* it: a cold
                // cache means a folder sorts by its own column, and building here
                // would put ninety milliseconds inside a keystroke.
                folders
                    .as_ref()
                    .and_then(|c| c.rows_of(live.number))
                    .unwrap_or(&[]),
            );
            let total = if matches_all {
                live.live_rows()
            } else {
                found.total
            };
            counted += total;
            budget = budget.saturating_sub(total as usize);
            visited += found.rows_visited;
            pool.extend(found.ranked.into_iter().map(|r| Candidate {
                key: r.key,
                mtime: r.mtime,
                seg: which as u32,
                row: r.row,
            }));
        }

        // Selection, not a sort. Two partitions put the window where it belongs
        // without ordering the hundred thousand rows in front of it.
        let end = need.min(pool.len());
        if pool.len() > end && end > 0 {
            pool.select_nth_unstable_by(end - 1, &mut cmp);
            pool.truncate(end);
        }
        let start = offset.min(pool.len());
        if start > 0 && start < pool.len() {
            pool.select_nth_unstable_by(start - 1, &mut cmp);
        }
        let window = &mut pool[start..];
        window.sort_unstable_by(&mut cmp);

        let built = window.len() as u64;
        let hits: Vec<Hit> = window
            .iter()
            .take(limit)
            .filter_map(|c| {
                let seg = views.get(c.seg as usize)?;
                let row = c.row as usize;
                seg.names.get(row).map(|n| seg.hit(row, &n))
            })
            .collect();
        Ok(SearchResponse {
            hits,
            total: counted.min(cap as u64),
            capped: counted >= cap as u64,
            took_us: started.elapsed().as_micros() as u64,
            // Did the walk get away with looking at less than everything? Not
            // "did every segment stop": a segment holding fewer matches than a
            // page runs to its own end and reports no early exit.
            fast_path: rows > 0 && visited < rows,
            rows_visited: visited,
            rows_built: built,
            // An index is handed a parsed query and never sees the text. The
            // engine stamps this on the way out.
            misread: Vec::new(),
            similar: None,
            reading: None,
        })
    }
}
