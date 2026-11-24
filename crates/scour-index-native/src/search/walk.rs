//! Walking a segment's rows for a page: in parts, on threads, by the order
//! asked for.

use super::*;
use crate::columns::{BLOCK, Field};
use scour_core::{Hit, SortKey};
use std::cell::Cell;
use std::sync::atomic::Ordering;

/// What a search asks for, and what it is allowed to spend. `count_cap` bounds the
/// walk **only once the page has been decided**, which a keyed text order never
/// reaches — there it bounds the total alone. Letting it stop the selection
/// returned "the largest forty among the five hundred newest".
#[derive(Debug, Clone, Copy)]
pub struct Wanted {
    pub sort: SortKey,
    pub descending: bool,
    pub offset: usize,
    pub limit: usize,
    /// Stop counting matches here.
    pub count_cap: usize,
    /// Answer with [`Found::ranked`] instead of [`Found::hits`], for a caller with
    /// several segments to merge: it decides the page and builds only that.
    pub rank_only: bool,
}

/// A candidate row, ordered but not built.
#[derive(Debug, Clone)]
pub struct Ranked {
    pub key: SortValue,
    /// What breaks a tie on the key, the same way [`sort_hits`] breaks it.
    pub mtime: i64,
    pub row: u32,
}

#[derive(Debug, Default)]
pub struct Found {
    pub hits: Vec<Hit>,
    /// Set instead of `hits` when [`Wanted::rank_only`] was asked for.
    pub ranked: Vec<Ranked>,
    pub total: u64,
    pub capped: bool,
    /// Whether the walk was able to stop early. Two ways and no others: the
    /// stored order has a page and a believable total, or a numeric order's
    /// unopened blocks cannot reach the page.
    pub early_exit: bool,
    pub rows_visited: u64,
    /// Rows built into a `Hit` — the expensive part, since each reconstructs a
    /// front-coded path.
    pub rows_built: u64,
}

/// Walk the segment and answer.
pub fn run(seg: &Segment<'_>, plan: &Plan, want: Wanted) -> Found {
    run_with(seg, plan, want, None, &[])
}

/// The same walk, with a veto over rows that match but must not be shown: a
/// removal disappears from searches at once, while erasing it from a segment
/// waits for the next commit. The veto runs *after* the query accepts a row,
/// being the dearer of the two.
pub fn run_with(
    seg: &Segment<'_>,
    plan: &Plan,
    want: Wanted,
    mut conceals: Option<&mut dyn FnMut(&Segment<'_>, usize, &[u8]) -> bool>,
    // What each directory row has under it, by row, sorted — see `sizes.rs`.
    // **Only `sort:size` reads it**, and only for directory rows. Empty means the
    // caller has not built it, and a folder then sorts by its own `Size` column.
    folders: &[(u32, i64)],
) -> Found {
    /* **A stored text or path order under a query that reads names**, tried
     * first where it can pay: each position costs a name read from its block's
     * start, so it wins only when matches are common, and is given up for the
     * ordinary walk after a budget. `e` sorted by name was 223 ms of reading
     * every name on 4.5 M rows to keep two hundred. */
    let need = want.offset + want.limit;
    let budget = need.saturating_mul(NAMED_STREAM_TRIES);
    if plan.needs_name()
        && need > 0
        && matches!(want.sort, SortKey::Name | SortKey::Path | SortKey::Ext)
        && budget <= seg.rows() / NAMED_STREAM_SHARE
        && let Some(found) = walk_segment(
            seg,
            plan,
            want,
            conceals.as_deref_mut(),
            folders,
            Some(budget),
            None,
            None,
        )
    {
        return found;
    }
    /* **A walk of most of a large segment is made in parts, a thread each.**
     * Nothing but the walk itself knows it is one, so it is asked to hand its
     * blocks back rather than walk them. Not under a veto, which is one
     * closure and not shareable; not for hits, which the merge does not take. */
    let mut split: Option<Vec<u32>> = None;
    let may_split = want.rank_only && conceals.is_none() && need > 0 && search_threads() > 1;
    if let Some(found) = walk_segment(
        seg,
        plan,
        want,
        conceals,
        folders,
        None,
        None,
        may_split.then_some(&mut split),
    ) {
        return found;
    }
    let blocks = split.expect("a walk with no budget answers or splits");
    in_parts(seg, plan, want, folders, &blocks)
}

/// Candidate blocks below which a walk stays on one thread: half a million
/// rows, about ten milliseconds of the dearest walk. `SCOUR_SPLIT_BLOCKS`
/// overrides, which is how the tests put every walk they make through parts.
fn split_blocks() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("SCOUR_SPLIT_BLOCKS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(16_384)
    })
}

/// The share of its blocks a stored-order walk reads before it asks to be
/// split: where matches are common the page and the count are both found
/// there, and where they are rare a sixteenth is what was wasted.
const SPLIT_AFTER: usize = 16;

/// Threads one search may use. `SCOUR_SEARCH_THREADS` overrides, for measuring;
/// 1 keeps every walk whole.
fn search_threads() -> usize {
    static ENV: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    let env = *ENV.get_or_init(|| {
        std::env::var("SCOUR_SEARCH_THREADS")
            .ok()
            .and_then(|v| v.parse().ok())
    });
    let chosen = env.unwrap_or_else(|| match SEARCH_THREADS.load(Ordering::Relaxed) {
        0 => std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .min(8),
        n => n,
    });
    chosen.max(1)
}

/// How many threads one large walk is split over, as the settings say; zero
/// is the measured default, the cores up to eight. `SCOUR_SEARCH_THREADS`
/// still wins, which is how the measurements pin it.
static SEARCH_THREADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Split large walks over this many threads from now on; zero for the default.
pub fn set_search_threads(n: usize) {
    SEARCH_THREADS.store(n, Ordering::Relaxed);
}

/// One walk made as several, over equal shares of its candidate blocks. Each
/// part keeps its own page of candidates and its own count; the caller's merge
/// orders what they hand back, as it orders the segments.
fn in_parts(
    seg: &Segment<'_>,
    plan: &Plan,
    want: Wanted,
    folders: &[(u32, i64)],
    blocks: &[u32],
) -> Found {
    let per = blocks.len().div_ceil(search_threads()).max(1);
    let parts: Vec<Found> = std::thread::scope(|scope| {
        let walks: Vec<_> = blocks
            .chunks(per)
            .map(|part| {
                scope.spawn(move || {
                    walk_segment(seg, plan, want, None, folders, None, Some(part), None)
                        .expect("a part of a walk always answers")
                })
            })
            .collect();
        walks
            .into_iter()
            .map(|walk| match walk.join() {
                Ok(found) => found,
                Err(panic) => std::panic::resume_unwind(panic),
            })
            .collect()
    });
    let counted: u64 = parts.iter().map(|p| p.total).sum();
    Found {
        total: counted.min(want.count_cap as u64),
        capped: counted >= want.count_cap as u64,
        early_exit: parts.iter().all(|p| p.early_exit),
        rows_visited: parts.iter().map(|p| p.rows_visited).sum(),
        ranked: parts.into_iter().flat_map(|p| p.ranked).collect(),
        ..Found::default()
    }
}

/// Positions a name-reading stream may look at per row the page needs: it
/// succeeds when one name in sixteen matches. The count is not its to make.
const NAMED_STREAM_TRIES: usize = 16;

/// The most of a segment such a stream may read before giving up, as a share:
/// a name read at random costs about four read in order, so a thirty-second of
/// the rows wasted is an eighth of the walk it falls back to.
const NAMED_STREAM_SHARE: usize = 32;

/// [`run_with`]'s walk. With `named`, a stored order is streamed even though the
/// query reads names, for at most that many positions; `None` if they run out.
fn walk_segment<'v>(
    seg: &Segment<'_>,
    plan: &Plan,
    want: Wanted,
    // Its own lifetime for the closure, so that a first try can borrow it briefly.
    mut conceals: Option<&mut (dyn FnMut(&Segment<'_>, usize, &[u8]) -> bool + 'v)>,
    folders: &[(u32, i64)],
    named: Option<usize>,
    // The candidate blocks to walk instead of the plan's own: one share of a
    // walk being made in parts. Never streamed, a stream reading no blocks.
    part: Option<&[u32]>,
    // Where to leave the candidate blocks, returning `None`, if this turns out
    // to be a walk worth making in parts. See [`in_parts`].
    split: Option<&mut Option<Vec<u32>>>,
) -> Option<Found> {
    let has_veto = conceals.is_some();
    // A name-reading query streams only when asked to, and then reads the name.
    let streams_names = named.is_some();
    let reads_names = has_veto || plan.needs_name();
    // A `Cell` rather than a plain counter because the block loop below reads it
    // while the closure that increments it is alive.
    let counted = Cell::new(0usize);
    let mut visited = 0u64;
    // Row numbers, not rows: four bytes each until the page is decided.
    let mut kept: Vec<u32> = Vec::new();
    /* **A page's worth of candidates, not a corpus's.** Materialising each match
     * to sort it measured 16.5 ms, and one `SortValue` per match at 2,234,583
     * entries was ~90 MB — 559 MB of peak RSS sorted by path. */
    let mut keyed: Vec<(SortValue, u32)> = Vec::new();
    /* The worst row the page holds, in key space, as of the last trim: no stored
     * number bounds a name, so no *block* can be skipped, but the row in hand can.
     * `None` under [`zone_order`], where `worst` carries the same boundary. */
    let mut bar: Option<(SortValue, u32)> = None;
    /* The key of the row being looked at, refilled rather than rebuilt: ordering
     * by path builds a string a row, and only a row that beats `bar` is copied
     * out of this buffer. */
    let mut scratch = SortValue::Num(0);
    /* Directories already rebuilt, for the one ordering that asks per row.
    See `DirPaths`. */
    let mut dir_paths = DirPaths::default();

    // The terms relevance scores against, and they have to be the merge's terms:
    // each segment picks the best `need` of its own rows and `sort_hits` orders
    // what they hand over. `filterable` cannot serve — for `ext:rs` it answers
    // `.rs`, which every match contains by definition.
    let score_terms: Vec<Vec<u8>> = if want.sort == SortKey::Relevance {
        plan.clauses
            .iter()
            .filter_map(|c| match &c.alts[..] {
                [(false, Test::NameHas(n))] if !n.folded().is_empty() => Some(n.folded().to_vec()),
                [(false, Test::NameHasMarked(m))] => Some(m.folded.folded().to_vec()),
                _ => None,
            })
            .collect()
    } else {
        Vec::new()
    };

    // The one order the row layout already satisfies; everything else has to see
    // every match first. Relevance with nothing to score is that same order, and
    // it is what a window shows on opening: 121 ms of full scan before this line.
    let stored_forward = (want.sort == SortKey::Modified && want.descending)
        // With nothing to score, both relevance directions are one equal-key
        // group: direction reverses only the primary key, and the tie order stays
        // newest-first then path-first, which is the stored row order.
        || (want.sort == SortKey::Relevance && score_terms.is_empty());

    // How many rows the page can possibly reach. Zero is a count and nothing
    // else, which several decisions below turn on.
    let need = want.offset + want.limit;

    // Which blocks are worth opening at all — the trigram index and the zone map,
    // both of which can only remove. Hoisted above the walk because how much of
    // the segment survived decides whether the path order can be streamed.
    let blocks = match part {
        Some(part) => part.to_vec(),
        None => blocks_worth_opening(seg, plan),
    };

    /* **Ordering by path is a stored order too, when the segment has one**: the
     * page is the first `need` positions that are live and match. Two conditions —
     * the walk must not need names, the folded arena having no offsets, and the
     * filters must not have narrowed much, half the blocks surviving bounding the
     * worst case at twice the walk. */
    let n_blocks = seg.rows().div_ceil(BLOCK);
    let path_stream = want.sort == SortKey::Path
        && need > 0
        && (!plan.needs_name() || streams_names)
        && seg.porder.is_some_and(|o| o.rows() == seg.rows())
        && part.is_none()
        && blocks.len().saturating_mul(2) >= n_blocks;
    // The name equivalent of `path_stream`, over an order folded at build time as
    // the keyed walk and the merge compare it. A query that reads names stays on
    // the sequential arena walk, whose locality this would discard.
    let name_stream = want.sort == SortKey::Name
        && need > 0
        && (!plan.needs_name() || streams_names)
        && seg.norder.is_some_and(|o| o.rows() == seg.rows())
        && part.is_none()
        && blocks.len().saturating_mul(2) >= n_blocks;
    // Extensions have the same grouped tie semantics as names with far fewer
    // primary values; the boundary map keeps descending order from reversing the
    // large ties.
    let extension_stream = want.sort == SortKey::Ext
        && need > 0
        && (!plan.needs_name() || streams_names)
        && seg.eorder.is_some_and(|o| o.rows() == seg.rows())
        && part.is_none()
        && blocks.len().saturating_mul(2) >= n_blocks;
    let text_stream = name_stream || extension_stream;
    let position_stream = path_stream || text_stream;
    /* **A streamed order stops at the page and the total is counted apart.**
     * Counting on in stream order is a name read at random per position: `a` by
     * name with a cap of a hundred thousand read every row, 125 ms, where the
     * same count in row order is under a millisecond. Only where names are
     * read: a column read at random is cheap enough to count with. */
    let count_apart = position_stream && reads_names && want.count_cap > need;
    // Asked to stream and unable to: the walk is the caller's to make, who may
    // make it in parts.
    if named.is_some() && !position_stream {
        return None;
    }

    // Whether the walk has to read names at all — decided here because the
    // direction depends on it. A streamed text order reads none: a name is wanted
    // only for rows that end up as merge candidates.
    let sort_reads_name = !stored_forward
        && !position_stream
        && matches!(want.sort, SortKey::Name | SortKey::Ext | SortKey::Path);
    let by_row = !plan.needs_name() && !has_veto && !sort_reads_name;

    /* **Oldest-first is the same walk backwards**: 60–82 ms against 1,321–2,168 on
     * 2.24 M rows. Only when the walk does not read names — the folded arena
     * stores no offsets, so there is no reading it backwards. */
    let backwards = want.sort == SortKey::Modified && !want.descending && by_row;
    // A streamed path order belongs here: the rows arrive already in the order
    // asked for, so there is nothing to select and no boundary to keep.
    let stored_order = stored_forward || backwards || position_stream;
    /* The length that triggers the next trim of `keyed`: a page's worth, then
     * twice whatever the trim left, so the amortised cost per row stays
     * constant. */
    let mut trim_at = need;
    /* Is the sort key the whole answer, or only the start of one? Asked here so
     * that the trim and the final selection cannot answer it differently. See
     * [`key_is_exact`]. */
    let exact = key_is_exact(want.sort);
    let mut done = false;

    /* **A number the rows are not stored in can still stop early**: every block
     * records the minimum and maximum of every column, so the blocks are opened
     * best first. `need == 0` is a count, with no page to bound. */
    let bounded = !stored_order && need > 0 && sort_field(want.sort).is_some();
    /* The worst row the page currently holds, once it holds a page of them, in
     * the block order's key space where smaller is better — see [`zone_key`].
     * `None` until the page fills, because until then nothing is out of reach. */
    let worst: Cell<Option<(i64, u32)>> = Cell::new(None);
    /* Set when no unopened block can reach the page: the walk can no longer change
     * *which* rows are returned, so the only thing left to walk for is the count,
     * which may stop at `count_cap`. Before it is set, the cap must not. */
    let closed = Cell::new(false);
    /* The date the page ends on, once there is a page. A backwards walk yields
     * dates in order but paths *reversed* within a date, and the merge sorts ties
     * by path, so the walk runs to the end of that group and hands all of it over. */
    let mut edge: Option<i64> = None;

    let mut visit = |row: usize, name: &[u8]| -> bool {
        visited += 1;
        if !seg.is_alive(row) || !plan.accepts(seg, row, name) {
            return true;
        }
        if let Some(veto) = conceals.as_deref_mut()
            && veto(seg, row, name)
        {
            return true;
        }
        counted.set(counted.get() + 1);
        if backwards {
            let when = seg.num(Field::Mtime, row);
            if kept.len() < need {
                kept.push(row as u32);
                if kept.len() == need {
                    edge = Some(when);
                }
            } else if edge == Some(when) {
                // Still inside the group the page ends on. See `edge`.
                kept.push(row as u32);
            } else if counted.get() >= want.count_cap {
                // Past that group, and enough counted for the total to be
                // honest. Both, for the reason the forward walk gives below.
                done = true;
                return false;
            }
        } else if stored_order {
            if kept.len() < need {
                kept.push(row as u32);
            }
            // Both conditions must hold: enough rows for the page, and enough
            // counted for the total to be honest. The first alone is what made
            // the engine this replaces report a page and a lie.
            if kept.len() >= need && (count_apart || counted.get() >= want.count_cap) {
                done = true;
                return false;
            }
        } else if !closed.get() {
            // Any other order has to see every match before it knows which forty
            // win, so the count cap must not stop the walk here: it turned "the
            // largest forty" into "the largest forty among the five hundred
            // newest".
            if need == 0 {
                return true;
            }
            sort_value_into(
                &mut scratch,
                seg,
                row,
                name,
                want.sort,
                &score_terms,
                folders,
                &mut dir_paths,
            );
            // Rejected before it is kept, which for a path is before its
            // string is copied out of the buffer it was built in.
            if out_of_page(&bar, &scratch, row as u32, want.descending, exact) {
                return true;
            }
            keyed.push((scratch.clone(), row as u32));
            // **A page's worth, not a corpus's.** Throwing rows away is only safe
            // when the key is the whole answer, and an abbreviated name key ties
            // rows that are not equal — `narrow`'s job. One selection per `need`
            // pushes is amortised constant and leaves the boundary at `need - 1`.
            if keyed.len() >= trim_at {
                narrow(&mut keyed, need, want.descending, exact);
                trim_at = keyed.len().saturating_mul(2);
                if bounded {
                    if let (SortValue::Num(v), at) = &keyed[need - 1] {
                        worst.set(Some((zone_key(*v, want.descending), *at)));
                    }
                } else {
                    // **The row gate, and only where the block gate cannot run.**
                    // Under `zone_order` the boundary is refreshed every `need`
                    // *matches*, and a staler `worst` skips fewer blocks.
                    bar = Some(keyed[need - 1].clone());
                }
            }
        } else if counted.get() >= want.count_cap {
            // The page can no longer change and the total is believed: `closed`
            // is the block loop's statement that nothing left can reach the page,
            // and the cap makes the number beside it honest.
            done = true;
            return false;
        }
        true
    };

    // Nothing here reads a name, and the *sort* has to be asked too: `sort_value`
    // reading the empty name gave every row sorted by name the same key, at a cost
    // of 1,117,687 rows built to return forty.
    let mut walk = |from: usize, to: usize, rev: bool| -> bool {
        if by_row {
            if rev {
                for row in (from..to).rev() {
                    if !visit(row, b"") {
                        return false;
                    }
                }
            } else {
                for row in from..to {
                    if !visit(row, b"") {
                        return false;
                    }
                }
            }
            return true;
        }
        let mut go = true;
        // The **folded** arena: a search matches folded text, and folding
        // it here instead was 24.4 ns of the 40.5 a row used to cost.
        seg.folded.walk_range(from, to, |row, name| {
            go = visit(row, name);
            go
        });
        go
    };

    // Whether any candidate block was left unopened, which is what
    // [`Found::early_exit`] reports; `visit` sets `done` for the other way out.
    let mut skipped = false;
    // What is left of a name-reading stream's budget; unlimited otherwise.
    let mut tries = named.unwrap_or(usize::MAX);
    let out_of_tries = |tries: &mut usize| {
        if *tries == 0 {
            return true;
        }
        *tries -= 1;
        false
    };
    let grouped_positions = seg
        .norder
        .filter(|_| name_stream)
        .map(GroupedPositions::Name)
        .or_else(|| {
            seg.eorder
                .filter(|_| extension_stream)
                .map(|order| GroupedPositions::Extension(order, Default::default()))
        });
    if let Some(mut positions) = grouped_positions {
        /* Ascending text keys are the position list as written; descending reads
         * primary-key groups from the end but each group forwards. Ties stay
         * newest-first then path-first in both directions. */
        if want.descending {
            let mut end = positions.rows();
            while end > 0 {
                let Some(start) = positions.group_at_or_before(end - 1) else {
                    break;
                };
                for i in start..end {
                    if out_of_tries(&mut tries) {
                        return None;
                    }
                    let Some(row) = positions.at(i) else {
                        continue;
                    };
                    let name = if reads_names {
                        seg.folded.get(row as usize).unwrap_or_default().as_bytes()
                    } else {
                        b""
                    };
                    if !visit(row as usize, name) {
                        end = 0;
                        break;
                    }
                }
                if end > 0 {
                    end = start;
                }
            }
        } else {
            for i in 0..positions.rows() {
                if out_of_tries(&mut tries) {
                    return None;
                }
                let Some(row) = positions.at(i) else {
                    continue;
                };
                let name = if reads_names {
                    seg.folded.get(row as usize).unwrap_or_default().as_bytes()
                } else {
                    b""
                };
                if !visit(row as usize, name) {
                    break;
                }
            }
        }
    } else if let Some(positions) = seg.porder.filter(|_| path_stream) {
        /* **The page is the first `need` positions that survive.** What it gives
         * up is locality — consecutive positions are scattered rows — against not
         * making the read at all: two hundred rows against 2,235,402. The block
         * filters are ignored, soundly: both only remove rows `accepts` rejects. */
        let n = positions.rows();
        for i in 0..n {
            let at = if want.descending { n - 1 - i } else { i };
            // A position naming a row this segment does not hold is damage the
            // reader has already refused to pass on.
            if out_of_tries(&mut tries) {
                return None;
            }
            let Some(row) = positions.at(at) else {
                continue;
            };
            let name = if reads_names {
                seg.folded.get(row as usize).unwrap_or_default().as_bytes()
            } else {
                b""
            };
            if !visit(row as usize, name) {
                break;
            }
        }
    }
    // **Best block first.** The blocks are opened in the order of what their
    // stored maximum — or minimum, ascending — could contribute, and the walk ends
    // when the page's worst row beats everything the next could hold.
    else if let Some(field) = sort_field(want.sort).filter(|_| bounded) {
        /* **The ordering is built when it can pay for itself, and not before**: it
         * saves nothing until there is a page for a block to be out of reach *of*,
         * and building it anyway measured 9.6–10.9 ms against 8.6–9.0 on an
         * already-narrow `rapor` sorted by size. */
        let mut order: Vec<(i64, u32)> = Vec::new();
        let mut ordered = false;
        let mut at = 0usize;
        let mut stopped = false;
        loop {
            if !ordered && worst.get().is_some() {
                order = zone_order(seg, field, want.descending, &blocks[at..], folders);
                ordered = true;
                at = 0;
            }
            let block = if ordered {
                let Some(&(reach, block)) = order.get(at) else {
                    break;
                };
                if out_of_reach(worst.get(), reach, block) {
                    break;
                }
                block
            } else {
                let Some(&block) = blocks.get(at) else {
                    break;
                };
                block
            };
            at += 1;
            let from = block as usize * BLOCK;
            let to = ((block as usize + 1) * BLOCK).min(seg.rows());
            stopped = !walk(from, to, false);
            if stopped {
                break;
            }
        }
        let mut left: Vec<u32> = if ordered {
            order[at..].iter().map(|&(_, b)| b).collect()
        } else {
            blocks[at..].to_vec()
        };
        // **The count is a separate obligation, and this is where it is paid.**
        // The page is settled and the total is not, so what is left of the blocks
        // is walked for the count alone. `closed` is what lets `count_cap` stop.
        if !stopped && counted.get() < want.count_cap {
            closed.set(true);
            left.sort_unstable();
            for (from, to) in runs_of(&left, seg.rows()) {
                if !walk(from, to, false) {
                    break;
                }
            }
        } else {
            skipped = !left.is_empty();
        }
    }
    // Every other order walks the candidates in row order, or from the far end
    // when the stored order is being read backwards.
    else {
        let large = blocks.len() >= split_blocks() && !backwards;
        let mut from_block = 0usize;
        // Whether the walk said it needs nothing more.
        let mut finished = false;
        if let Some(split) = split.filter(|_| large) {
            // A keyed walk sees every match whatever it finds; the stored order
            // may be done within its first blocks.
            if stored_order {
                from_block = blocks.len() / SPLIT_AFTER;
                for (from, to) in runs_of(&blocks[..from_block], seg.rows()) {
                    if !walk(from, to, false) {
                        finished = true;
                        break;
                    }
                }
            }
            if !finished {
                *split = Some(blocks);
                return None;
            }
        }
        let mut runs = runs_of(&blocks[from_block..], seg.rows());
        if backwards {
            runs.reverse();
        }
        for (from, to) in runs {
            if finished || !walk(from, to, backwards) {
                break;
            }
        }
    }

    let done = done || skipped;
    // A stream that ran out has counted every match; one that stopped at the
    // page has not, and a walk with no page is the count.
    let (total, capped) = if count_apart && done {
        let count = walk_segment(
            seg,
            plan,
            // In the stored order, the one a count may stop at its cap in.
            Wanted {
                sort: SortKey::Modified,
                descending: true,
                offset: 0,
                limit: 0,
                ..want
            },
            conceals,
            folders,
            None,
            None,
            None,
        )
        .expect("a walk with no budget always answers");
        visited += count.rows_visited;
        (count.total, count.capped)
    } else {
        (
            counted.get().min(want.count_cap) as u64,
            counted.get() >= want.count_cap,
        )
    };

    // The candidates, each with what it sorts by. The stored order needs no
    // selection, but still has to say what it sorts by: whoever merges this
    // segment cannot see the row order. Built for the rows *kept* and no others.
    let ranked: Vec<(SortValue, u32)> = if stored_order {
        kept.iter()
            .map(|&row| {
                // A streamed text order needs a merge key only for the page, so
                // these are random arena reads for `need` rows.
                let name = if text_stream {
                    seg.folded.get(row as usize).unwrap_or_default().as_bytes()
                } else {
                    b""
                };
                (
                    sort_value(
                        seg,
                        row as usize,
                        name,
                        want.sort,
                        &score_terms,
                        folders,
                        &mut dir_paths,
                    ),
                    row,
                )
            })
            .collect()
    } else {
        // The last of the trims the walk has been making all along, and the
        // only one on a buffer that never reached `trim_at`.
        narrow(&mut keyed, need, want.descending, exact);
        keyed
    };

    // **Keys and row numbers, for a caller with other segments to merge this
    // with.** Building a row reconstructs its front-coded path, and the merge
    // throws away all but the page: 401,438 paths for sixty, 1.43 s.
    if want.rank_only {
        return Some(Found {
            ranked: ranked
                .into_iter()
                .map(|(key, row)| Ranked {
                    key,
                    // The merge's second key, matching `sort_hits`. One column
                    // read a candidate, and only for candidates.
                    mtime: seg.num(Field::Mtime, row as usize),
                    row,
                })
                .collect(),
            total,
            capped,
            early_exit: done,
            rows_visited: visited,
            ..Found::default()
        });
    }
    kept = ranked.into_iter().map(|(_, row)| row).collect();

    // Materialise. Only now, and only what can appear: reading a row means
    // building its path, which is the expensive part of the whole operation.
    let rows_built = kept.len() as u64;
    let mut hits: Vec<Hit> = kept
        .into_iter()
        .filter_map(|row| {
            let row = row as usize;
            seg.names.get(row).map(|n| seg.hit(row, &n))
        })
        .collect();
    // **And the backwards walk sorts too**: its rows arrive in date order with the
    // paths reversed inside a date, and `kept` is a page and a tie group. **So
    // does a streamed path order**, where two rows can spell the same path.
    if !stored_order || backwards || position_stream {
        // Within one segment the rows already carry their score in `keyed`;
        // this path is the one that did not sort, so it scores from scratch.
        let owned: Vec<String> = score_terms
            .iter()
            .map(|t| String::from_utf8_lossy(t).into_owned())
            .collect();
        let terms: Vec<&str> = owned.iter().map(String::as_str).collect();
        sort_hits(&mut hits, want.sort, want.descending, &terms);
    }
    let hits: Vec<Hit> = hits
        .into_iter()
        .skip(want.offset)
        .take(want.limit)
        .collect();

    Some(Found {
        hits,
        total,
        capped,
        early_exit: done,
        rows_visited: visited,
        rows_built,
        ..Found::default()
    })
}

/// Every matching row of one segment, narrowed the same way a search is, for the
/// questions about the whole matching set rather than a page of it. `f` is given
/// the row; a caller that wants the name reads it. False if `f` stopped it.
pub fn walk_matches(seg: &Segment<'_>, plan: &Plan, mut f: impl FnMut(usize) -> bool) -> bool {
    let blocks = blocks_worth_opening(seg, plan);
    let by_row = !plan.needs_name();
    let mut i = 0usize;
    while i < blocks.len() {
        let mut j = i;
        while j + 1 < blocks.len() && blocks[j + 1] == blocks[j] + 1 {
            j += 1;
        }
        let from = blocks[i] as usize * BLOCK;
        let to = ((blocks[j] as usize + 1) * BLOCK).min(seg.rows());
        if by_row {
            for row in from..to {
                if seg.is_alive(row) && plan.accepts(seg, row, b"") && !f(row) {
                    return false;
                }
            }
        } else {
            let mut go = true;
            // The **folded** arena, exactly as a search walks it: the spelled
            // name means `RAPOR.pdf` quietly does not match `rapor`.
            seg.folded.walk_range(from, to, |row, name| {
                if seg.is_alive(row) && plan.accepts(seg, row, name) {
                    go = f(row);
                }
                go
            });
            if !go {
                return false;
            }
        }
        i = j + 1;
    }
    true
}
