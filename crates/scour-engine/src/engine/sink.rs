//! Where a walk's rows go: into the index in batches, and the first of them
//! onto the list before the walk is done.

use super::*;
use scour_core::{Change, Entry, EntrySink, Flow, Index, Page, SearchRequest};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// The batch that stands in for `commit_batch` when nobody is watching: with
/// nothing open there is no latency to protect, so this is a memory ceiling.
pub(super) const IDLE_BATCH: u64 = 4_096;

pub(super) const BATCH: usize = 4_096;

/// Turns a walk into index updates, in batches.
pub(super) struct ToIndex {
    pub(super) index: Arc<dyn Index>,
    pub(super) seen: u64,
    pub(super) buffer: Vec<Change>,
    pub(super) stop: Arc<Shared>,
    /// A batch the index refused. The sweep removes everything the walk did not
    /// stamp, so one failed write would otherwise turn a scan into a deletion.
    pub(super) failed: bool,
    /// When a first walk next shows what it has found. `None` on any other walk.
    pub(super) show: Option<Show>,
    /// Shared by the walks of one pass, so the count shown is all of them.
    pub(super) counter: Arc<AtomicU64>,
}

/// Whether the index holds nothing under these roots: an empty index, or a disk
/// walked for the first time. One row asked for, so a disk that has rows answers
/// at the first block it reads.
pub(super) fn holds_nothing(index: &dyn Index, roots: &[String]) -> bool {
    if index.stats().is_ok_and(|s| s.entries == 0) {
        return true;
    }
    roots.iter().all(|root| {
        let request = SearchRequest {
            query: scour_query::parse(&format!("under:\"{root}\"")),
            page: Page {
                offset: 0,
                limit: 1,
                count_cap: 1,
            },
            ..SearchRequest::default()
        };
        index
            .search(&request)
            .is_ok_and(|found| found.hits.is_empty())
    })
}

/// A first walk's clock for making its rows searchable while it goes on.
pub(super) struct Show {
    pub(super) at: Instant,
    pub(super) every: Duration,
}

/// How soon a first walk's rows are searchable, and the most the wait between two
/// showings grows to. A walk into an empty index otherwise told a waiting window
/// nothing until it ended — minutes of an empty list on a fresh install.
pub(super) const SHOW_FIRST: Duration = Duration::from_secs(1);

const SHOW_MOST: Duration = Duration::from_secs(16);

impl ToIndex {
    /// Make what the walk has found so far searchable and say so. The waits double,
    /// so a long walk shows itself a dozen times rather than once a second.
    fn show(&mut self) {
        if let Err(e) = self.index.publish() {
            // The rows stay staged and land with the walk; only the showing stops.
            scour_core::note!("scourd: what the walk has found so far could not be shown: {e}");
            self.show = None;
            return;
        }
        self.stop.touched_now();
        if let Some(s) = self.show.as_mut() {
            s.every = (s.every * 2).min(SHOW_MOST);
            s.at = Instant::now() + s.every;
        }
    }

    pub(super) fn flush(&mut self) {
        if self.buffer.is_empty() {
            return;
        }
        let batch = std::mem::replace(&mut self.buffer, Vec::with_capacity(BATCH));
        if let Err(e) = self.index.apply(&mut batch.into_iter()) {
            scour_core::note!("scourd: a batch of the scan was not indexed: {e}");
            self.failed = true;
        }
    }
}

impl EntrySink for ToIndex {
    fn push(&mut self, entry: Entry) -> Flow {
        self.seen += 1;
        self.buffer.push(Change::Upsert(entry));
        if self.buffer.len() >= BATCH {
            self.flush();
            // How far this walk has got, while it is still walking: written once a
            // batch, not once a row — a write lock every four thousand entries.
            let total = self.counter.fetch_add(BATCH as u64, Ordering::Relaxed) + BATCH as u64;
            self.stop.status.write().scanned = total;
            if self.show.as_ref().is_some_and(|s| Instant::now() >= s.at) {
                self.show();
            }
        }
        if self.stop.stop.load(Ordering::Relaxed) {
            Flow::Stop
        } else {
            Flow::Continue
        }
    }

    fn stopping(&self) -> bool {
        self.stop.stop.load(Ordering::Relaxed)
    }
}
