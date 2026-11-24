//! What an engine is built with, and the few numbers a test may tune.

use super::*;
use scour_core::ScanOptions;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// What is done about a word that may be mistyped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Typos {
    /// Nothing: a word finds what it spells.
    Off,
    /// A search that finds nothing says how many files it nearly found.
    #[default]
    Suggest,
    /// Every word long enough is forgiven its mistakes, in every search.
    Always,
}

#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// What the walk and the watchers skip, to begin with. Taken by [`Engine::new`]
    /// and never read again; [`Engine::set_scan_options`] replaces the live copy.
    pub scan: ScanOptions,
    /// How long changes accumulate before a commit.
    pub commit_interval: Duration,
    /// How large the unsorted tail may grow before a rebuild is advised.
    pub rebuild_threshold: u64,
    /// Rows a page may hold, whatever a caller asks for.
    pub result_limit: u32,
    /// How words are read, to begin with; [`Engine::set_reading`] replaces it.
    pub reading: Reading,
    /// How many unordered segments may accumulate before they are merged. Every
    /// query pays a fixed cost per segment; only a rebuild shrinks the tail.
    pub compact_segments: u32,
    /// How many changes make a commit worth a segment of its own; below this a change
    /// waits for [`EngineOptions::commit_idle`]. The cost guarded is the segment.
    pub commit_batch: u64,
    /// How long a handful of changes may wait before being written anyway. Bounds
    /// batching once a change has reached the worker, not detection or queued scans.
    pub commit_idle: Duration,
    /// The same bound, while somebody is waiting to be told about changes: a segment
    /// per commit that would have batched, paid only while a window waits.
    pub commit_watched: Duration,
    /// The point at which segments are merged **without** waiting for idle, since a
    /// build leaves no idle moment: 222 segments, and an 8 ms query took 13 s.
    pub compact_urgent: u32,
    /// How long the index may sit untouched before it is asked to give back
    /// whatever it was holding for writes.
    pub idle_after: Duration,
    /// Full reconciliation when neither a watch nor a pulse is available.
    pub poll_interval: Duration,
    /// Safety pass even when a source appears quiet. Pulses are only hints.
    /// Only for a source no watcher covers; see `watched_reconcile`.
    pub reconcile_interval: Duration,
    /// The same for a watched source: once the machine is quiet, one thread.
    /// Every thirty minutes it read 5 GB a round here and found nothing but
    /// files written through a mapping, which no watcher sees.
    pub watched_reconcile: Duration,
    /// How long a [`Change::Rescan`](scour_core::Change::Rescan) waits for its
    /// neighbours: 157 walks in twelve minutes of a build, 39.4 MB of writes.
    pub walk_debounce: Duration,
    /// The ceiling on that wait, however long the cluster keeps arriving. The
    /// whole of the latency the debounce can cost.
    pub walk_debounce_cap: Duration,
    /// How often [`Engine::await_change`] may wake the clients waiting in it; zero
    /// wakes on every bump. 3.5–6.2 bumps a second at 20.5–21 ms of CPU each.
    pub await_hold: Duration,
    /// How long published changes may stay only in memory before they are
    /// written down. A commit was a segment and fourteen syncs a second while a
    /// window watched; a crash now costs this much, and the next walk finds it.
    pub persist_every: Duration,
    /// Where [`Status::revision`] starts. An engine that replaces another over
    /// the same index continues its count, or a client that saw the old one's
    /// number could wait on the new one until that number comes round again.
    pub first_revision: u64,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            scan: ScanOptions::default(),
            commit_interval: Duration::from_millis(1_000),
            rebuild_threshold: 200_000,
            result_limit: 1_000,
            reading: Reading::default(),
            commit_batch: 64,
            commit_idle: Duration::from_secs(15),
            // The burst clock exactly: `commit_interval` floors how often a segment is written.
            commit_watched: Duration::from_millis(1_000),
            compact_segments: 8,
            compact_urgent: 64,
            idle_after: Duration::from_secs(20),
            poll_interval: Duration::from_secs(60),
            reconcile_interval: Duration::from_secs(1_800),
            watched_reconcile: Duration::from_secs(86_400),
            // Walks arrive at 13.3 a minute in bursts of 22 in five seconds: half a second is a real gap.
            walk_debounce: Duration::from_millis(500),
            // The worst a file in a brand-new directory waits; deliberately under `commit_idle`.
            walk_debounce_cap: Duration::from_secs(3),
            // `commit_watched`, exactly: a waiter cannot be shown what has not been written.
            await_hold: Duration::from_millis(1_000),
            persist_every: Duration::from_secs(30),
            first_revision: 0,
        }
    }
}

/// What the person tuned, for the whole process: an engine is replaced when a
/// disk comes or goes, and what was chosen in the settings has to outlive it.
/// See [`tune`].
static RELEASE_AFTER_MS: AtomicU64 = AtomicU64::new(60_000);

/// Zero is the configuration's own `commit_idle`.
static FRESH_WITHIN_MS: AtomicU64 = AtomicU64::new(0);

/// How the service trades memory and CPU for speed, as the settings say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tuning {
    /// How long after the last query the index's mapped pages and the folder
    /// table are released. A minute by default: typing never pays for it, and
    /// a burst of searching is not kept resident for the rest of the day.
    pub release_after: Duration,
    /// How stale the index may get while nothing is watching it; `None` keeps
    /// the configuration's `commit_idle`.
    pub fresh_within: Option<Duration>,
}

/// Take these from now on, in every engine of the process.
pub fn tune(t: &Tuning) {
    RELEASE_AFTER_MS.store(t.release_after.as_millis() as u64, Ordering::Relaxed);
    FRESH_WITHIN_MS.store(
        t.fresh_within.map_or(0, |d| d.as_millis().max(1) as u64),
        Ordering::Relaxed,
    );
}

pub(super) fn release_after() -> Duration {
    Duration::from_millis(RELEASE_AFTER_MS.load(Ordering::Relaxed))
}

/// How long an unwatched trickle may wait: the setting, or the configuration's.
pub(super) fn commit_idle(opts: &EngineOptions) -> Duration {
    match FRESH_WITHIN_MS.load(Ordering::Relaxed) {
        0 => opts.commit_idle,
        ms => Duration::from_millis(ms),
    }
}
