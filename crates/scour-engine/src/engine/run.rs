//! The engine's own thread: changes in, walks scheduled and retried, the index
//! committed and kept.

use super::*;
use crate::reconcile::{Nudge, Pulses};
use crossbeam_channel::{Receiver, select};
use scour_core::{Change, Maintenance};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// Work the background thread does.
pub(super) enum Job {
    /// Walk a source, or one subtree of it, and reconcile.
    Scan {
        source: usize,
        subtree: Option<String>,
    },
    /// Walk these sources whole: disks side by side, each disk's in turn.
    ScanAll(Vec<usize>),
    Maintain(Maintenance),
    Stop,
}

/// How one source is kept up to date. See [`Engine::keeping`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Keeping {
    /// A watcher covers all of it.
    pub live: bool,
    /// When a change in it last reached the index, unix seconds.
    pub changed: Option<i64>,
    /// Being walked, or waiting to be.
    pub walking: bool,
}

pub(super) fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// The background thread: one loop for jobs, changes and the commit clock.
pub(super) fn run(shared: Arc<Shared>, jobs: Receiver<Job>, changes: Receiver<Box<Change>>) {
    let mut dirty = false;
    // Set when a walk finishes with changes already queued behind it.
    let mut overdue = false;
    // Housekeeping runs once per quiet period, not once per tick.
    let mut idle_done = false;
    // Set by a commit, cleared by the check after: the one moment merging is free.
    let mut dirty_settled = false;
    let mut last_commit = Instant::now();
    // When something last arrived, as opposed to when this loop last wrote.
    let mut last_busy = Instant::now();
    let mut last_compact = Instant::now();
    // Published and not yet written down, and since when the clock runs.
    let mut published = false;
    let mut last_persist = Instant::now();
    // Roots that were not there to be read: a session starts before its mounts do.
    let mut retries: Vec<(usize, Instant, usize)> = Vec::new();
    // Subtree walks asked for and waiting for their neighbours.
    let mut pending_walks = PendingWalks::default();
    let mut pulses = Pulses::new(
        shared.sources.len(),
        shared.opts.poll_interval,
        shared.opts.reconcile_interval,
        shared.opts.watched_reconcile,
    );
    for (i, src) in shared.sources.iter().enumerate() {
        pulses.be_patient(i, src.patience());
    }

    loop {
        // Wait for the next thing that has to happen, not a tick: each deadline below
        // must be a moment the body would act, or the wait is zero and the loop spins.
        let now = Instant::now();
        if shared.wake_due().is_some_and(|at| at <= now) {
            shared.announce(now);
        }
        let left = |at: Instant| at.saturating_duration_since(now);
        let mut wake = Duration::from_secs(10);
        // Which deadline set the wake-up: one collapsed onto the floor is invisible.
        let trace = std::env::var_os("SCOUR_WAKE_TRACE").is_some();
        let mut who = "floor";
        // Every deadline that has already passed, not only the first: `left` saturates
        // at zero and the comparison is strict, so the first zero would hide the rest.
        let mut floored: Vec<&str> = Vec::new();
        macro_rules! deadline {
            ($name:literal, $at:expr) => {
                let d = left($at);
                if trace && d <= WAKE_FLOOR {
                    floored.push($name);
                }
                if d < wake {
                    wake = d;
                    who = $name;
                }
            };
        }
        deadline!("pulse", pulses.next_due());
        if dirty {
            // A full batch is held only by the interval floor, an unfull one by patience.
            let watched = shared.watchers.load(Ordering::Relaxed) > 0;
            let patience = if watched {
                shared.opts.commit_watched
            } else {
                commit_idle(&shared.opts)
            };
            let batch = if watched {
                shared.opts.commit_batch
            } else {
                shared.opts.commit_batch.max(IDLE_BATCH)
            };
            let at = if shared.pending.load(Ordering::Relaxed) >= batch {
                last_commit + shared.opts.commit_interval
            } else {
                last_commit + patience.max(shared.opts.commit_interval)
            };
            deadline!("commit", at);
        }
        // `&& !dirty` must match the guard on the work below: with only one, the
        // deadline stays past and the loop spins at 0.85% of a core doing nothing.
        if dirty_settled && !dirty {
            deadline!("compact", last_compact + COMPACT_EVERY);
        }
        if !dirty && !idle_done {
            deadline!("idle", last_busy + shared.opts.idle_after);
        }
        if let Some(at) = retries.iter().map(|(_, at, _)| *at).min() {
            deadline!("retry", at);
        }
        if published {
            deadline!("persist", last_persist + shared.opts.persist_every);
        }
        if shared.read_since_release.load(Ordering::Relaxed) {
            deadline!("release", *shared.last_read.lock() + release_after());
        }
        // The held walks, flushed below in the same turn, so this cannot spin.
        if let Some(at) =
            pending_walks.next_due(shared.opts.walk_debounce, shared.opts.walk_debounce_cap)
        {
            deadline!("walk", at);
        }
        // The held wake-up, already paid above if due, so this one is in the future.
        if let Some(at) = shared.wake_due() {
            deadline!("announce", at);
        }
        // A backstop: a deadline computed wrong costs turns a second, not a core.
        wake = wake.max(WAKE_FLOOR);
        if trace && wake <= WAKE_FLOOR {
            // All of them: the case worth finding is more than one having passed.
            let who = if floored.is_empty() {
                who.to_owned()
            } else {
                floored.join(", ")
            };
            scour_core::note!(
                "scourd: wake floored by {who} (dirty={dirty} settled={dirty_settled} idle_done={idle_done})"
            );
        }

        select! {
            recv(jobs) -> job => match job {
                Ok(Job::Stop) | Err(_) => break,
                Ok(Job::Scan { source, subtree }) => {
                    let whole = subtree.is_none();
                    if whole && let Some(flag) = shared.queued.get(source) {
                        // From here a fresh request is about what this walk has passed.
                        flag.store(false, Ordering::Release);
                    }
                    if !scan(&shared, &mut pulses, source, subtree, false) {
                        schedule_retry(&shared, &mut retries, &pulses, source);
                    } else if whole {
                        retries.retain(|(s, _, _)| *s != source);
                    }
                    dirty = true;
                    idle_done = false;
                    last_busy = Instant::now();
                    // Whatever waited out the walk has waited long enough: a walk does
                    // not drain this channel, and `commit_idle` would charge it twice.
                    overdue = !changes.is_empty();
                }
                Ok(Job::ScanAll(list)) => {
                    for &i in &list {
                        if let Some(flag) = shared.queued.get(i) {
                            flag.store(false, Ordering::Release);
                        }
                    }
                    for (source, ok) in scan_all(&shared, &mut pulses, list) {
                        if ok {
                            retries.retain(|(s, _, _)| *s != source);
                        } else {
                            schedule_retry(&shared, &mut retries, &pulses, source);
                        }
                    }
                    dirty = true;
                    idle_done = false;
                    last_busy = Instant::now();
                    overdue = !changes.is_empty();
                }
                Ok(Job::Maintain(level)) => {
                    match shared.index.maintain(level) {
                        Ok(_) => {
                            shared.touched();
                            shared.pending.store(0, Ordering::Relaxed);
                            dirty = false;
                        }
                        Err(e) => {
                            scour_core::note!("scourd: maintenance failed: {e}");
                            dirty = true;
                        }
                    }
                    last_commit = Instant::now();
                }
            },
            recv(changes) -> change => match change {
                Ok(c) => {
                    last_busy = Instant::now();
                    // A burst becomes one batch: a hundred changes cost barely one.
                    let mut batch = vec![*c];
                    while let Ok(more) = changes.try_recv() {
                        batch.push(*more);
                        if batch.len() >= 4_096 {
                            break;
                        }
                    }
                    // A handful is enough to prove the watcher is awake.
                    let now = now_secs();
                    for change in batch.iter().take(64) {
                        if let Some(i) = owner_of(&shared, change.path()) {
                            pulses.saw_event(i);
                            shared.changed[i].store(now, Ordering::Relaxed);
                        }
                    }
                    // Walks come out first: a thousand from a `git clone` coalesce to one.
                    let mut walks: Vec<String> = Vec::new();
                    batch.retain(|c| match c {
                        Change::Rescan { path } => {
                            walks.push(path.clone());
                            false
                        }
                        _ => true,
                    });
                    if !batch.is_empty() {
                        shared.pending.fetch_add(batch.len() as u64, Ordering::Relaxed);
                        let report = shared.index.apply(&mut batch.into_iter());
                        // Removals count now, upserts at the commit: `apply` hides a
                        // deletion at once while a creation stays staged until written.
                        match report {
                            Ok(r) if r.removed + r.subtrees_removed > 0 => shared.touched(),
                            Ok(_) => {},
                            Err(e) => {
                                scour_core::note!("scourd: changes could not be indexed: {e}");
                                // `apply` may consume only part of the iterator.
                                for i in 0..shared.sources.len() {
                                    schedule_retry(&shared, &mut retries, &pulses, i);
                                }
                            }
                        }
                        dirty = true;
                        idle_done = false;
                        // The ones that waited out a walk; see the scan arm.
                        if overdue {
                            overdue = false;
                            last_commit = Instant::now() - commit_idle(&shared.opts);
                        }
                    }
                    // A watcher that lost track becomes a walk of the subtree it lost,
                    // held rather than run: `coalesce` merges only what shares a batch.
                    let now = Instant::now();
                    for path in coalesce(walks) {
                        pending_walks.add(&path, now);
                    }
                }
                Err(_) => break,
            },
            default(wake) => {}
        }

        // The walks whose neighbours have stopped arriving — which is when nothing is.
        for path in pending_walks.take_due(
            Instant::now(),
            shared.opts.walk_debounce,
            shared.opts.walk_debounce_cap,
        ) {
            // An empty path means "I lost track and cannot say where" — an
            // overflowed kernel queue — so every source is walked.
            if path.is_empty() {
                let all = (0..shared.sources.len()).collect();
                for (i, ok) in scan_all(&shared, &mut pulses, all) {
                    if !ok {
                        schedule_retry(&shared, &mut retries, &pulses, i);
                    }
                }
            } else if let Some(i) = owner_of(&shared, &path) {
                // Watched before it is walked: a walk is a snapshot and a watch is
                // everything after it. The other way round lost 1,260 files of 5,000.
                for (src, h) in shared.watches.lock().iter() {
                    if *src == i {
                        h.cover(&path);
                    }
                }
                if !scan(&shared, &mut pulses, i, Some(path.clone()), false) {
                    schedule_retry(&shared, &mut retries, &pulses, i);
                }
            }
            dirty = true;
            idle_done = false;
            last_busy = Instant::now();
        }

        // The pulses, read outside the wait, where a steady stream of changes would
        // starve them. `is_due` carries the two-second floor, so asking is a compare.
        // A source watched but for some places — a `tmpfs` root, a disk mounted
        // inside — is walked there and nowhere else when its pulse moves: the rest
        // is heard, and walking "sistem" whole for `/tmp` was what unwatched cost.
        let mut partly: std::collections::HashMap<usize, Vec<String>> =
            std::collections::HashMap::new();
        let nudges = if pulses.is_due() {
            let watched: Vec<usize> = {
                let watches = shared.watches.lock();
                for (i, handle) in watches.iter() {
                    let places = handle.unwatched();
                    if !places.is_empty() {
                        partly.insert(*i, places);
                    }
                }
                watches
                    .iter()
                    .filter(|(i, _)| !partly.contains_key(i))
                    .map(|(i, _)| *i)
                    .collect()
            };
            let readings: Vec<Option<u64>> = shared.sources.iter().map(|s| s.pulse()).collect();
            pulses.decide(&readings, &watched, &crate::reconcile::machine_quiet)
        } else {
            Vec::new()
        };
        for (source, job) in nudges {
            // Failed passes have their own widening retry clock.
            if retries.iter().any(|(s, _, _)| *s == source) {
                continue;
            }
            if let Some(places) = partly.get(&source)
                && matches!(job, Nudge::Reconcile | Nudge::Check)
            {
                let gentle = matches!(job, Nudge::Check);
                for place in places {
                    if !scan(&shared, &mut pulses, source, Some(place.clone()), gentle) {
                        schedule_retry(&shared, &mut retries, &pulses, source);
                    }
                }
                dirty = true;
                idle_done = false;
                last_busy = Instant::now();
                continue;
            }
            match job {
                Nudge::Reconcile => {
                    if !scan(&shared, &mut pulses, source, None, false) {
                        schedule_retry(&shared, &mut retries, &pulses, source);
                    }
                    dirty = true;
                    idle_done = false;
                    last_busy = Instant::now();
                }
                Nudge::Check => {
                    if !scan(&shared, &mut pulses, source, None, true) {
                        schedule_retry(&shared, &mut retries, &pulses, source);
                    }
                    dirty = true;
                    idle_done = false;
                    last_busy = Instant::now();
                }
                Nudge::Blind => {
                    // A watched source whose pulse has moved for minutes with nothing
                    // arriving: a quiet watcher and a quiet disk look alike.
                    scour_core::note!(
                        "scourd: source {source} has changed repeatedly with no events \
                         arriving — the watch is not covering it; rescanning"
                    );
                    if !scan(&shared, &mut pulses, source, None, false) {
                        schedule_retry(&shared, &mut retries, &pulses, source);
                    }
                    dirty = true;
                    idle_done = false;
                    last_busy = Instant::now();
                }
            }
        }

        // A commit writes a segment, so a trickle waits and a burst does not.
        let waited = last_commit.elapsed();
        let watched = shared.watchers.load(Ordering::Relaxed) > 0;
        // The batch is a latency rule, so it applies only when latency has somebody to
        // matter to: unwatched it cost 0.30% of a core on fourteen commits a minute.
        let batch = if watched {
            shared.opts.commit_batch
        } else {
            shared.opts.commit_batch.max(IDLE_BATCH)
        };
        let enough = shared.pending.load(Ordering::Relaxed) >= batch;
        // How long a trickle may wait: unwatched, one segment a minute rather than one
        // a second; watched, as soon as the burst clock allows — 0.90 s to findable.
        let patience = if watched {
            shared.opts.commit_watched
        } else {
            commit_idle(&shared.opts)
        };
        if dirty && waited >= shared.opts.commit_interval && (enough || waited >= patience) {
            match shared.index.publish() {
                Ok(()) => {
                    // Whatever was staged is now in a segment and findable, and
                    // on disk once the persist below comes round.
                    published = true;
                    shared.touched();
                    shared.pending.store(0, Ordering::Relaxed);
                    shared.status.write().unwritten = 0;
                    dirty = false;
                    dirty_settled = true;
                    idle_done = false;
                }
                // Still dirty, and nobody is told: the rows are back in the staging
                // buffer, and a revision would re-read an index that did not move.
                Err(e) => {
                    let n = {
                        let mut st = shared.status.write();
                        st.unwritten += 1;
                        st.unwritten
                    };
                    // Once, then every thirty: a full disk should be said, not repeated.
                    if n == 1 || n % 30 == 0 {
                        scour_core::note!(
                            "scourd: the index could not be written ({n} attempts): {e}"
                        );
                    }
                }
            }
            last_commit = Instant::now();
        }

        // What publishing kept in memory, written down in one go. A failure keeps
        // it in memory and says so the way a failed commit does.
        if published && last_persist.elapsed() >= shared.opts.persist_every {
            match shared.index.commit() {
                Ok(()) => {
                    published = false;
                    shared.status.write().unwritten = 0;
                }
                // Counted with the failed commits, so the faces say it: this is
                // the write a full disk refuses, and the one above rarely is.
                Err(e) => {
                    let n = {
                        let mut st = shared.status.write();
                        st.unwritten += 1;
                        st.unwritten
                    };
                    if n == 1 || n % 30 == 0 {
                        scour_core::note!(
                            "scourd: the index could not be written ({n} attempts): {e}"
                        );
                    }
                }
            }
            last_persist = Instant::now();
        }

        // A minute after the last query, the pages it read go back to the kernel.
        if shared.read_since_release.load(Ordering::Relaxed)
            && shared.last_read.lock().elapsed() >= release_after()
        {
            shared.read_since_release.store(false, Ordering::Relaxed);
            shared.index.release_memory();
        }

        // Volumes that were not there when they were last asked about.
        if !retries.is_empty() {
            let now = Instant::now();
            let due: Vec<(usize, usize)> = retries
                .iter()
                .filter(|(_, at, _)| *at <= now)
                .map(|(source, _, attempt)| (*source, *attempt))
                .collect();
            retries.retain(|(_, at, _)| *at > now);
            for (source, attempt) in due {
                if scan(&shared, &mut pulses, source, None, false) {
                    scour_core::note!(
                        "scourd: {} is readable again",
                        shared.sources[source].describe().name
                    );
                    dirty = true;
                    last_busy = Instant::now();
                    idle_done = false;
                } else {
                    retries.push((
                        source,
                        pulses.retry_at(source, retry_delay(attempt + 1)),
                        attempt + 1,
                    ));
                }
            }
        }

        // Merging does not wait for quiet, which never comes: a fold builds under the
        // read lock and costs a search one `Vec` swap. What is left is a floor.
        if dirty_settled && !dirty {
            if let Ok(stats) = shared.index.stats()
                && (stats.segments > shared.opts.compact_urgent
                    || (stats.segments > shared.opts.compact_segments
                        && last_compact.elapsed() >= COMPACT_EVERY))
            {
                let _ = shared.index.maintain(Maintenance::Compact);
                last_compact = Instant::now();
                last_commit = Instant::now();
            }
            dirty_settled = false;
        }

        // Housekeeping, once the machine has stopped asking for anything, counted from
        // the last change rather than the last commit — this loop's own footprint.
        if !dirty && !idle_done && last_busy.elapsed() >= shared.opts.idle_after {
            // Stop holding a write buffer: on an idle machine that is hundreds of
            // megabytes of a service left running.
            // Which writes down whatever publishing kept in memory.
            if shared.index.maintain(Maintenance::Idle).is_ok() {
                published = false;
                last_persist = Instant::now();
            }
            // And while nobody is waiting, work out what the folders weigh: 90 ms of
            // prefix sums otherwise paid by the first list a window shows.
            let _ = shared.index.subtree_sizes(&[]);
            idle_done = true;
        }
        if shared.stop.load(Ordering::Relaxed) && jobs.is_empty() && changes.is_empty() {
            break;
        }
    }
    let _ = shared.index.commit();
    shared.pending.store(0, Ordering::Relaxed);
}

/// Which source owns this path? A prefix **and a separator**, not a prefix:
/// `/home/hasan` does not own `/home/hasanX`.
pub(super) fn owner_of(shared: &Shared, path: &str) -> Option<usize> {
    // The longest root wins, not the first configured: a project directory that is
    // its own source lives inside a home directory that is also one.
    shared
        .sources
        .iter()
        .enumerate()
        .filter_map(|(i, s)| {
            s.describe()
                .roots
                .iter()
                .filter(|r| {
                    let r = r.trim_end_matches('/');
                    path == r
                        || (path.starts_with(r) && path.as_bytes().get(r.len()) == Some(&b'/'))
                })
                .map(|r| r.trim_end_matches('/').len())
                .max()
                .map(|len| (i, len))
        })
        .max_by_key(|&(_, len)| len)
        .map(|(i, _)| i)
}

/// How long to wait before looking again at a source whose roots were not there:
/// catches a disk mounted after start-up, and costs an unplugged one a listing an hour.
const RETRY_AFTER: [Duration; 5] = [
    Duration::from_secs(10),
    Duration::from_secs(30),
    Duration::from_secs(120),
    Duration::from_secs(600),
    Duration::from_secs(3_600),
];

fn retry_delay(attempt: usize) -> Duration {
    RETRY_AFTER[attempt.min(RETRY_AFTER.len() - 1)]
}

/// Note that a source has to be looked at again, without queueing a second one.
/// One refused in too many places waits ten minutes, not ten seconds: refusals
/// are met again, and a disk walked into `/proc` rewrote a million rows a minute.
fn schedule_retry(
    shared: &Shared,
    retries: &mut Vec<(usize, Instant, usize)>,
    pulses: &Pulses,
    source: usize,
) {
    if !retries.iter().any(|(s, _, _)| *s == source) {
        let attempt = if shared.blind[source].load(Ordering::Relaxed) {
            3
        } else {
            0
        };
        retries.push((
            source,
            pulses.retry_at(source, retry_delay(attempt)),
            attempt,
        ));
    }
}

/// The floor on how often segments are merged without being asked. Not a cost of
/// merging but of deciding: `stats()` reads every segment's live count.
const COMPACT_EVERY: Duration = Duration::from_secs(60);

/// The shortest the worker will ever sleep: a backstop, so a deadline computed
/// wrong costs fifty turns a second rather than a spun core.
const WAKE_FLOOR: Duration = Duration::from_millis(20);
