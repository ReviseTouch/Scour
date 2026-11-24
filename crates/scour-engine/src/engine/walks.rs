//! Walking the sources: one at a time, or a lane a disk, first rows shown as
//! they are found.

use super::*;
use crate::reconcile::Pulses;
use scour_core::{Result, ScanOptions};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Reduce a set of requested walks to the ones not already covered: walking `/a`
/// walks `/a/b`, and a walk flushes a segment, takes a generation and sweeps.
pub(super) fn coalesce(paths: Vec<String>) -> Vec<String> {
    scour_core::PrefixSet::new(paths).into_paths()
}

/// Walks asked for and not run yet, with when each was first and last asked for.
/// Each waits [`EngineOptions::walk_debounce`] for its neighbours; none is dropped.
#[derive(Default)]
pub(super) struct PendingWalks {
    /// Path, first asked, last asked. A `Vec` and a linear scan: the earliest
    /// deadline needs a full pass anyway, and a burst reduces as it arrives.
    pub(super) at: Vec<(String, Instant, Instant)>,
}

impl PendingWalks {
    /// Note that `path` wants walking, merging it with what is already waiting.
    pub(super) fn add(&mut self, path: &str, now: Instant) {
        // Normalised as `PrefixSet` does: `/a/` and `/a` are one entry, `/` is empty.
        let path = path.trim_end_matches('/');
        if path.is_empty() {
            // A watcher that lost track and cannot say where: walking every source
            // covers everything waiting here, and it does not wait.
            self.at.clear();
            self.at.push((String::new(), now, now));
            return;
        }
        if let Some(e) = self
            .at
            .iter_mut()
            .find(|(p, _, _)| scour_core::under(path, p))
        {
            // Already covered by a waiting walk. Its clock is refreshed rather than
            // a second entry made; the cap is measured from its own first arrival.
            e.2 = now;
            return;
        }
        // The other direction: this path covers some of what is waiting. The
        // earliest first-asked comes with it, so nothing's deadline is postponed.
        let mut first = now;
        self.at.retain(|(p, f, _)| {
            if scour_core::under(p, path) {
                first = first.min(*f);
                false
            } else {
                true
            }
        });
        self.at.push((path.to_owned(), first, now));
    }

    /// When the earliest of these has waited long enough.
    pub(super) fn next_due(&self, debounce: Duration, cap: Duration) -> Option<Instant> {
        self.at
            .iter()
            .map(|(p, f, l)| Self::due_at(p, *f, *l, debounce, cap))
            .min()
    }

    /// Take the walks that have waited long enough, reduced. Whatever a taken path
    /// covers goes with it, or the same walk runs again a moment later.
    pub(super) fn take_due(
        &mut self,
        now: Instant,
        debounce: Duration,
        cap: Duration,
    ) -> Vec<String> {
        let mut due: Vec<String> = Vec::new();
        self.at.retain(|(p, f, l)| {
            if Self::due_at(p, *f, *l, debounce, cap) <= now {
                due.push(p.clone());
                false
            } else {
                true
            }
        });
        if due.is_empty() {
            return due;
        }
        self.at
            .retain(|(p, _, _)| !due.iter().any(|d| scour_core::under(p, d)));
        coalesce(due)
    }

    /// Quiet for `debounce`, or waiting since `cap` ago, whichever comes first.
    fn due_at(
        path: &str,
        first: Instant,
        last: Instant,
        debounce: Duration,
        cap: Duration,
    ) -> Instant {
        if path.is_empty() {
            // "I lost track" does not wait.
            return first;
        }
        (last + debounce).min(first + cap)
    }
}

/// Walk one source and reconcile what it holds, saying whether the walk could see
/// what it came for. `false` means the roots were not there to be read.
pub(super) fn scan(
    shared: &Arc<Shared>,
    pulses: &mut Pulses,
    source: usize,
    subtree: Option<String>,
    gentle: bool,
) -> bool {
    let Some(src) = shared.sources.get(source).cloned() else {
        return true;
    };
    let began = Instant::now();
    let read_before = disk_read_bytes();
    // A generation the index could not open cannot reconcile: its rows would carry
    // the previous one, and the sweep would judge them by it.
    let generation = match shared.index.begin_generation() {
        Ok(g) => g,
        Err(e) => {
            scour_core::note!(
                "scourd: a scan of {} could not start: {e}",
                src.describe().name
            );
            return false;
        }
    };
    shared.scanning.store(true, Ordering::Relaxed);
    {
        let mut st = shared.status.write();
        st.scanning_source = Some(src.id());
        st.scanned = 0;
    }
    if subtree.is_none() {
        shared.walking[source].store(true, Ordering::Relaxed);
    }

    // A disk the index holds nothing of shows its rows as they are found: nothing
    // else can answer for it until this walk does. Any other walk only corrects
    // what is already there. Per disk, not per index: the second disk of a first
    // run was minutes of nothing (eight on a Windows machine, 17 November).
    let first = subtree.is_none() && holds_nothing(&*shared.index, &src.describe().roots);
    let mut sink = ToIndex {
        index: Arc::clone(&shared.index),
        seen: 0,
        buffer: Vec::with_capacity(BATCH),
        stop: Arc::clone(shared),
        failed: false,
        show: first.then(|| Show {
            at: Instant::now() + SHOW_FIRST,
            every: SHOW_FIRST,
        }),
        counter: Arc::new(AtomicU64::new(0)),
    };
    // Read once, here: a walk skips by one set of rules from beginning to end, and
    // a rule saved halfway through takes effect on the scan that follows.
    let mut opts = ScanOptions {
        subtree: subtree.clone(),
        ..(*shared.scan()).clone()
    };
    // A check is nobody's wait: one thread, however many a walk is allowed.
    if gentle {
        opts.threads = 1;
    }
    let report = src.scan(&opts, &mut sink);
    if let Err(e) = &report {
        scour_core::note!("scourd: a scan of {} failed: {e}", src.describe().name);
    }
    sink.flush();
    let seen = sink.seen;

    // Anything under the walked subtree this pass did not stamp is gone. Never run
    // where the walk could not look or the index refused: both delete living files.
    let vouched: Vec<String> = report
        .as_ref()
        .map(|r| r.vouched.clone())
        .unwrap_or_default();
    let spare =
        scour_core::PrefixSet::new(report.as_ref().map(|r| r.blind.clone()).unwrap_or_default());
    let could_look = !vouched.is_empty();
    let trustworthy = report.as_ref().is_ok_and(|r| !r.cancelled) && could_look && !sink.failed;
    let mut ended = true;
    let mut gone = 0;
    if trustworthy {
        // One call for every root the walk vouched for: the pass's notes of unchanged
        // rows are consumed by the first call, leaving later roots reconciled to nothing.
        gone = match shared.index.sweep(src.id(), &vouched, generation, &spare) {
            Ok(n) => n,
            // Half a reconciliation. The retry is the caller's; the rows that should
            // have gone are found again by the next full scan.
            Err(e) => {
                scour_core::note!("scourd: {vouched:?} could not be reconciled: {e}");
                ended = false;
                if let Err(e) = shared.index.abandon_generation(generation) {
                    scour_core::note!("scourd: an incomplete pass could not be ended: {e}");
                }
                0
            }
        };
        // A sweep takes effect at once, like any other removal; the walk's own
        // upserts are staged and announce themselves at the commit.
        if gone > 0 {
            shared.touched();
        }
    } else if let Err(e) = shared.index.abandon_generation(generation) {
        ended = false;
        // A pass that will not be swept still has to end: a segment carrying its notes
        // cannot be folded, and one unended pass measured 241 of them.
        scour_core::note!(
            "scourd: a pass of {} could not be ended: {e}",
            src.describe().name
        );
    }
    if subtree.is_none() {
        // A whole source only: subtree walks come dozens a minute during a build.
        // Without this line the journal could not say when a walk after a
        // reboot had finished, or what it cost.
        let read = match (read_before, disk_read_bytes()) {
            (Some(a), Some(b)) => {
                format!(", {} MB read from disk", b.saturating_sub(a) / 1_000_000)
            }
            _ => String::new(),
        };
        let how = match (trustworthy, gentle) {
            (false, _) => "stopped walking",
            (true, true) => "checked",
            (true, false) => "walked",
        };
        // Named: what is under an unreadable place is neither seen nor swept,
        // and the first path says where to look.
        let blind = match report.as_ref() {
            Ok(r) if r.unreadable > 0 || !r.blind.is_empty() => format!(
                ", {} unreadable, first {}",
                r.unreadable.max(r.blind.len() as u64),
                r.blind.first().map(String::as_str).unwrap_or("unnamed")
            ),
            _ => String::new(),
        };
        // Beside `gone`, what the walk found that the watcher had not already
        // said: the measure of whether this walk was needed at all.
        let fresh = shared.index.fresh(generation);
        // A walk that found something is a change reaching the index too: the
        // only way one does for a source nothing watches.
        if fresh > 0 || gone > 0 {
            shared.changed[source].store(now_secs(), Ordering::Relaxed);
        }
        scour_core::note!(
            "scourd: {how} {} in {:.1} s — {seen} entries, {fresh} new or changed, \
             {gone} gone{read}{blind}",
            src.describe().name,
            began.elapsed().as_secs_f64()
        );
        pulses.scanned(source, began.elapsed());
        let problem = match report.as_ref() {
            Err(e) => e.to_string(),
            Ok(_) if !could_look => "could not be read".to_owned(),
            Ok(_) => String::new(),
        };
        shared.walks.write().insert(
            src.describe().name,
            scour_core::Walked {
                at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs() as i64),
                entries: seen,
                unreadable: report.as_ref().map_or(0, |r| r.unreadable),
                problem,
            },
        );
        // Recognising a whole source's unchanged rows read the index end to end:
        // 162 MB left resident after a startup that nothing looked at again.
        shared.index.release_memory();
    }

    // Refusals and vanished directories leave a walk finished: a retry would
    // find them the same, and their rows are spared either way.
    let finished = report
        .as_ref()
        .is_ok_and(|r| !r.cancelled && r.unreadable == r.lasting);
    let covered = scour_core::PrefixSet::new(vouched);
    let all_roots = src.describe().roots.iter().all(|root| covered.covers(root));

    shared.scanning.store(false, Ordering::Relaxed);
    shared.walking[source].store(false, Ordering::Relaxed);
    let mut st = shared.status.write();
    st.scanning_source = None;
    st.scanned = seen;
    st.last_scan_ms = report.map(|r| r.took_ms).unwrap_or(0);
    // A missing subtree alone is not a missing source. Any incomplete pass or
    // failed write needs a retry of the source to recover what it could not see.
    !sink.failed && ended && finished && (subtree.is_some() || (could_look && all_roots))
}

/// Whole walks of these sources, each disk's one after another and the disks
/// side by side: PC2's seven volumes on two SSDs and a spinning disk had each
/// waited for the one before. Says for each source whether its walk finished.
pub(super) fn scan_all(
    shared: &Arc<Shared>,
    pulses: &mut Pulses,
    list: Vec<usize>,
) -> Vec<(usize, bool)> {
    let mut lanes: Vec<(Option<String>, Vec<usize>)> = Vec::new();
    for i in list {
        let Some(src) = shared.sources.get(i) else {
            continue;
        };
        let disk = src.disk();
        match lanes.iter_mut().find(|(d, _)| *d == disk) {
            Some((_, lane)) => lane.push(i),
            None => lanes.push((disk, vec![i])),
        }
    }
    if lanes.len() <= 1 {
        return lanes
            .into_iter()
            .flat_map(|(_, lane)| lane)
            .map(|i| (i, scan(shared, pulses, i, None, false)))
            .collect();
    }
    scan_lanes(shared, pulses, lanes.into_iter().map(|(_, l)| l).collect())
}

/// What one walk of a shared pass brought back.
struct LaneWalk {
    source: usize,
    report: Result<scour_core::ScanReport>,
    seen: u64,
    failed: bool,
    took: Duration,
}

/// The walks of several disks at once, in one generation: the index keeps one
/// pass open at a time, and a pass's notes of unchanged rows are swept in one
/// call for every source it walked ([`Index::sweep_many`]).
fn scan_lanes(
    shared: &Arc<Shared>,
    pulses: &mut Pulses,
    lanes: Vec<Vec<usize>>,
) -> Vec<(usize, bool)> {
    let began = Instant::now();
    let generation = match shared.index.begin_generation() {
        Ok(g) => g,
        Err(e) => {
            scour_core::note!("scourd: a walk of every disk could not start: {e}");
            return lanes.into_iter().flatten().map(|i| (i, false)).collect();
        }
    };
    shared.scanning.store(true, Ordering::Relaxed);
    {
        let mut st = shared.status.write();
        st.scanning_source = lanes
            .first()
            .and_then(|l| l.first())
            .and_then(|&i| shared.sources.get(i))
            .map(|s| s.id());
        st.scanned = 0;
    }
    let counter = Arc::new(AtomicU64::new(0));
    let opts = ScanOptions {
        subtree: None,
        ..(*shared.scan()).clone()
    };
    let walks: Vec<LaneWalk> = std::thread::scope(|scope| {
        let handles: Vec<_> = lanes
            .iter()
            .map(|lane| {
                let (counter, opts) = (Arc::clone(&counter), &opts);
                scope.spawn(move || {
                    lane.iter()
                        .map(|&i| {
                            let src = Arc::clone(&shared.sources[i]);
                            let started = Instant::now();
                            shared.walking[i].store(true, Ordering::Relaxed);
                            let first = holds_nothing(&*shared.index, &src.describe().roots);
                            let mut sink = ToIndex {
                                index: Arc::clone(&shared.index),
                                seen: 0,
                                buffer: Vec::with_capacity(BATCH),
                                stop: Arc::clone(shared),
                                failed: false,
                                show: first.then(|| Show {
                                    at: Instant::now() + SHOW_FIRST,
                                    every: SHOW_FIRST,
                                }),
                                counter: Arc::clone(&counter),
                            };
                            let report = src.scan(opts, &mut sink);
                            sink.flush();
                            shared.walking[i].store(false, Ordering::Relaxed);
                            LaneWalk {
                                source: i,
                                report,
                                seen: sink.seen,
                                failed: sink.failed,
                                took: started.elapsed(),
                            }
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().unwrap_or_default())
            .collect()
    });

    // One sweep for every walk that may judge what it did not see.
    let trusted = |w: &LaneWalk| {
        !w.failed
            && w.report
                .as_ref()
                .is_ok_and(|r| !r.cancelled && !r.vouched.is_empty())
    };
    let parts: Vec<(scour_core::SourceId, Vec<String>)> = walks
        .iter()
        .filter(|w| trusted(w))
        .filter_map(|w| {
            let r = w.report.as_ref().ok()?;
            Some((shared.sources[w.source].id(), r.vouched.clone()))
        })
        .collect();
    let blind: Vec<String> = walks
        .iter()
        .filter(|w| trusted(w))
        .flat_map(|w| {
            w.report
                .as_ref()
                .map(|r| r.blind.clone())
                .unwrap_or_default()
        })
        .collect();
    let spare = scour_core::PrefixSet::new(blind);
    let mut ended = true;
    let mut gone = 0;
    if parts.is_empty() {
        if let Err(e) = shared.index.abandon_generation(generation) {
            ended = false;
            scour_core::note!("scourd: a pass of every disk could not be ended: {e}");
        }
    } else {
        match shared.index.sweep_many(&parts, generation, &spare) {
            Ok(n) => gone = n,
            Err(e) => {
                scour_core::note!("scourd: a walk of every disk could not be reconciled: {e}");
                ended = false;
                if let Err(e) = shared.index.abandon_generation(generation) {
                    scour_core::note!("scourd: an incomplete pass could not be ended: {e}");
                }
            }
        }
        if gone > 0 {
            shared.touched();
        }
    }
    let fresh = shared.index.fresh(generation);
    let total: u64 = walks.iter().map(|w| w.seen).sum();
    scour_core::note!(
        "scourd: walked {} disks at once in {:.1} s — {total} entries, {fresh} new or changed, {gone} gone",
        lanes.len(),
        began.elapsed().as_secs_f64()
    );

    let mut out = Vec::new();
    for w in &walks {
        let src = &shared.sources[w.source];
        let trustworthy = trusted(w);
        let could_look = w.report.as_ref().is_ok_and(|r| !r.vouched.is_empty());
        scour_core::note!(
            "scourd: {} {} in {:.1} s — {} entries",
            if trustworthy {
                "walked"
            } else {
                "stopped walking"
            },
            src.describe().name,
            w.took.as_secs_f64(),
            w.seen
        );
        if let Err(e) = &w.report {
            scour_core::note!("scourd: a scan of {} failed: {e}", src.describe().name);
        }
        pulses.scanned(w.source, w.took);
        let problem = match w.report.as_ref() {
            Err(e) => e.to_string(),
            Ok(_) if !could_look => "could not be read".to_owned(),
            Ok(_) => String::new(),
        };
        shared.walks.write().insert(
            src.describe().name,
            scour_core::Walked {
                at: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_secs() as i64),
                entries: w.seen,
                unreadable: w.report.as_ref().map_or(0, |r| r.unreadable),
                problem,
            },
        );
        let finished = w
            .report
            .as_ref()
            .is_ok_and(|r| !r.cancelled && r.unreadable == r.lasting);
        let covered = scour_core::PrefixSet::new(
            w.report
                .as_ref()
                .map(|r| r.vouched.clone())
                .unwrap_or_default(),
        );
        let all_roots = src.describe().roots.iter().all(|root| covered.covers(root));
        out.push((
            w.source,
            !w.failed && ended && finished && could_look && all_roots,
        ));
    }
    // A lane that panicked brought nothing back: its sources are walked again.
    for i in lanes.iter().flatten() {
        if !out.iter().any(|(s, _)| s == i) {
            out.push((*i, false));
        }
    }
    shared.index.release_memory();
    shared.scanning.store(false, Ordering::Relaxed);
    let mut st = shared.status.write();
    st.scanning_source = None;
    st.scanned = total;
    st.last_scan_ms = began.elapsed().as_millis() as u64;
    out
}

/// What this process has had read from the disk so far — the page cache's misses,
/// not its reads. Linux only; a walk's cost is the difference across it.
fn disk_read_bytes() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let io = std::fs::read_to_string("/proc/self/io").ok()?;
        io.lines()
            .find_map(|l| l.strip_prefix("read_bytes:"))?
            .trim()
            .parse()
            .ok()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}
