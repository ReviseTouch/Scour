//! Queries asked often and answered slowly, answered again in the background
//! after the index moves, so the next asking finds them ready.

use super::*;
use crossbeam_channel::Receiver;
use scour_core::{Hit, Page, SearchRequest, SortKey};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// What the preparing thread is asked to have ready.
pub(super) struct Prepare {
    pub(super) query: String,
    pub(super) sort: SortKey,
    pub(super) descending: bool,
    pub(super) count_cap: u32,
}

/// One query's ordered hits, kept so that paging through them is free: a page
/// otherwise costs `offset + limit` per segment, 16 ms at the top and 114 deep.
pub(super) struct Prepared {
    pub(super) query: String,
    pub(super) parsed: scour_core::Ast,
    pub(super) sort: SortKey,
    pub(super) descending: bool,
    pub(super) count_cap: u32,
    /// The index revision this was built from; anything else makes it wrong.
    pub(super) revision: u64,
    pub(super) hits: Vec<Hit>,
    pub(super) total: u64,
    pub(super) capped: bool,
}

/// How many ordered hits are kept hot — the ceiling on what any page may ask for.
/// Roughly a hundred bytes a hit, and there is only ever one of these.
pub(super) const PREPARE: u32 = 20_000;

/// How often an ordering may be built, at the very least: a floor, not the whole
/// rule — one window of two hundred rows spans 3.2 ms to 2,463.6 ms by sort key.
pub(super) const PREPARE_EVERY: Duration = Duration::from_secs(2);

/// Cheap pages already meet the interactive budget. Building a much larger
/// window for them spends memory and competes with the next keystroke.
pub(super) const PREPARE_MIN_COST: Duration = Duration::from_millis(20);

/// What a walk buys the next one: it waits at least this many times what the last
/// took, so speculation costs at most a tenth of a machine. Deliberately uncapped.
const PREPARE_COST: u32 = 10;

/// How many of the biggest files are considered for duplication. A generous
/// ceiling: on 1,474,650 files, everything over a megabyte is 18,723 of them.
pub(super) const CANDIDATES: u32 = 200_000;

/// Keeps the ordering of whatever query was asked for last. The channel holds one,
/// and the result is dropped if the index moved while it was built.
pub(super) fn prepare_loop(shared: Arc<Shared>, jobs: Receiver<Prepare>, stop: Receiver<()>) {
    loop {
        // Shared owns the sender, so waiting on disconnection outlives `Engine::drop`.
        let job = crossbeam_channel::select_biased! {
            recv(stop) -> _ => return,
            recv(jobs) -> job => match job {
                Ok(job) => job,
                Err(_) => return,
            },
        };
        if shared.stop.load(Ordering::Acquire) {
            return;
        }
        let at = shared.revision.load(Ordering::Acquire);
        let began = Instant::now();
        let parsed = shared.ast(&job.query);
        let found = shared.index.search(&SearchRequest {
            query: parsed.clone(),
            sort: job.sort,
            descending: job.descending,
            page: Page {
                offset: 0,
                limit: PREPARE,
                count_cap: job.count_cap,
            },
        });
        // Charged whether kept or not: a discarded walk cost as much as a used one.
        let floor = PREPARE_EVERY.max(began.elapsed() * PREPARE_COST);
        shared
            .prepare_floor
            .store(floor.as_micros() as u64, Ordering::Relaxed);
        let Ok(found) = found else { continue };
        if shared.revision.load(Ordering::Acquire) != at {
            continue;
        }
        *shared.prepared.write() = Some(Prepared {
            query: job.query,
            parsed,
            sort: job.sort,
            descending: job.descending,
            count_cap: job.count_cap,
            revision: at,
            hits: found.hits,
            total: found.total,
            capped: found.capped,
        });
    }
}
