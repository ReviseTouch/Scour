//! The orchestrator.

mod questions;
mod reading;
mod search;
mod sources;
pub use reading::*;
pub use search::*;

mod options;
mod prepare;
mod run;
mod sink;
mod walks;
pub use options::*;
use prepare::*;
pub use run::*;
use sink::*;
use walks::*;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Sender, unbounded};
use parking_lot::{Condvar, Mutex, RwLock};

use scour_core::{
    Change, Error, Index, IndexStats, MaintReport, Maintenance, Result, ScanOptions, Source,
    Status, WatchHandle,
};

struct Shared {
    sources: Vec<Arc<dyn Source>>,
    index: Arc<dyn Index>,
    opts: EngineOptions,
    /// What the walk and the watchers skip. Behind a lock because a person edits it
    /// while the service runs; an `Arc` inside so a walk copies it and lets go.
    scan: RwLock<Arc<ScanOptions>>,
    status: RwLock<Status>,
    /// Each source's last whole walk, by its name. See [`Engine::walks`].
    walks: RwLock<std::collections::HashMap<String, scour_core::Walked>>,
    pending: AtomicU64,
    scanning: AtomicBool,
    /// Per source: a whole-source walk is already waiting to run. Cleared as the
    /// walk begins, so a rule saved mid-walk still queues one behind it.
    queued: Vec<AtomicBool>,
    /// Per source: when a change in it last reached the index, unix seconds; 0 never.
    changed: Vec<std::sync::atomic::AtomicI64>,
    /// Per source: a whole walk of it is under way.
    walking: Vec<AtomicBool>,
    stop: AtomicBool,
    /// The live watches, each beside the source it belongs to: the worker runs the
    /// walks, and a subtree walk is when its watcher is told the subtree exists.
    watches: Mutex<Vec<(usize, Box<dyn WatchHandle>)>>,
    /// See [`Status::revision`].
    revision: AtomicU64,
    /// Whoever is blocked in [`Engine::await_change`]. The mutex closes the gap
    /// between a client reading the revision and going to sleep on it.
    waiters: (Mutex<()>, Condvar),
    /// When they were last woken, and whether a bump has been held since — a held
    /// bump is owed, not dropped. See [`EngineOptions::await_hold`].
    wake_at: Mutex<Instant>,
    wake_owed: AtomicBool,
    /// [`Reading`], as its bits.
    read_as: AtomicU8,
    /// The last query that found nothing, when it was counted, and what it
    /// nearly found. See [`SIMILAR_STANDS`].
    similar: Mutex<Option<(String, Instant, Option<scour_core::Similar>)>>,
    /// The ordered hits of whichever query was asked for last.
    prepared: RwLock<Option<Prepared>>,
    /// When one was last asked for, so misses cannot queue one each.
    prepared_at: Mutex<Instant>,
    /// How long that one has to stand before another may be asked for, in
    /// microseconds — the preparing thread's last walk times [`PREPARE_COST`].
    prepare_floor: AtomicU64,
    /// Asks the preparing thread for a query's full ordered page. Bounded and
    /// tiny: only the newest request matters.
    prepare: Sender<Prepare>,
    /// When a query last read the index, and whether one has since memory was
    /// last released: the mapped pages a search reads are released a minute
    /// after the last one, rather than kept resident by one burst of typing.
    last_read: Mutex<Instant>,
    read_since_release: std::sync::atomic::AtomicBool,
    /// How many of them there are. Read by the commit clock: zero means nobody is
    /// looking and the batching stands.
    watchers: AtomicU32,
    /// The reading behind a query with `content:` in it.
    contents: Arc<crate::content::Contents>,
    /// What a thread reading files does first. See [`Engine::read_files_as`].
    enter: RwLock<Option<scour_grep::Enter>>,
}

impl Shared {
    fn read_as(&self) -> Reading {
        Reading::of(self.read_as.load(Ordering::Relaxed))
    }

    /// A query as it is answered: parsed, and read the way a person chose.
    fn ast(&self, query: &str) -> scour_core::Ast {
        let read = self.read_as();
        let mut ast = scour_query::parse(query);
        ast.exact_accents = read.exact_accents;
        ast.strict_typos = read.strict_typos;
        match read.typos {
            Typos::Always => ast.forgiving().unwrap_or(ast),
            _ => ast,
        }
    }

    /// A query is reading the index: the release clock starts again.
    fn reading(&self) {
        *self.last_read.lock() = Instant::now();
        self.read_since_release.store(true, Ordering::Relaxed);
    }

    /// What the walk skips, right now. A pointer copy under the read lock, so a
    /// caller that walks for a minute holds nothing a saved rule waits behind.
    fn scan(&self) -> Arc<ScanOptions> {
        Arc::clone(&self.scan.read())
    }

    /// A search run again could now answer differently. Bumped under the waiters'
    /// lock, so a client between reading and sleeping on it is not overtaken.
    fn touched(&self) {
        {
            let _held = self.waiters.0.lock();
            self.revision.fetch_add(1, Ordering::Release);
        }
        self.announce(Instant::now());
    }

    /// The same, for a change asked for by name: already bounded by how fast a
    /// person can ask, so it skips the hold and resets it.
    fn touched_now(&self) {
        {
            let _held = self.waiters.0.lock();
            self.revision.fetch_add(1, Ordering::Release);
        }
        *self.wake_at.lock() = Instant::now();
        self.wake_owed.store(false, Ordering::Relaxed);
        self.waiters.1.notify_all();
    }

    /// Wake the waiters unless one was woken less than a hold ago, and say whether it
    /// did. A held bump sets `wake_owed`, so the announcement is late, never missing.
    fn announce(&self, now: Instant) -> bool {
        let hold = self.opts.await_hold;
        let mut at = self.wake_at.lock();
        if hold.is_zero() || now.saturating_duration_since(*at) >= hold {
            *at = now;
            self.wake_owed.store(false, Ordering::Relaxed);
            drop(at);
            self.waiters.1.notify_all();
            true
        } else {
            self.wake_owed.store(true, Ordering::Relaxed);
            false
        }
    }

    /// When a held-back wake-up comes due, if one is owed.
    fn wake_due(&self) -> Option<Instant> {
        self.wake_owed
            .load(Ordering::Relaxed)
            .then(|| *self.wake_at.lock() + self.opts.await_hold)
    }
}

pub struct Engine {
    shared: Arc<Shared>,
    jobs: Sender<Job>,
    changes: Sender<Box<Change>>,
    worker: Mutex<Option<JoinHandle<()>>>,
    prepare_stop: Sender<()>,
    preparer: Mutex<Option<JoinHandle<()>>>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("sources", &self.shared.sources.len())
            .field("index", &self.shared.index)
            .finish()
    }
}

impl Engine {
    pub fn new(
        sources: Vec<Arc<dyn Source>>,
        index: Arc<dyn Index>,
        mut opts: EngineOptions,
    ) -> Engine {
        // One slot: a request that has been overtaken is work nobody wants.
        let (prepare_tx, prepare_rx) = crossbeam_channel::bounded::<Prepare>(1);
        let (prepare_stop, prepare_stopped) = crossbeam_channel::bounded(1);
        // Moved, not copied: one answer to "what does the walk skip", never two.
        let scan = RwLock::new(Arc::new(std::mem::take(&mut opts.scan)));
        let source_count = sources.len();
        let await_hold = opts.await_hold;
        let reading = opts.reading;
        let first_revision = opts.first_revision;
        let shared = Arc::new(Shared {
            status: RwLock::new(Status {
                sources: sources.len() as u32,
                ..Default::default()
            }),
            sources,
            index,
            opts,
            scan,
            walks: RwLock::new(std::collections::HashMap::new()),
            pending: AtomicU64::new(0),
            scanning: AtomicBool::new(false),
            queued: (0..source_count).map(|_| AtomicBool::new(false)).collect(),
            changed: (0..source_count)
                .map(|_| std::sync::atomic::AtomicI64::new(0))
                .collect(),
            walking: (0..source_count).map(|_| AtomicBool::new(false)).collect(),
            stop: AtomicBool::new(false),
            watches: Mutex::new(Vec::new()),
            revision: AtomicU64::new(first_revision),
            waiters: (Mutex::new(()), Condvar::new()),
            // A hold ago, so the session's first change is announced as it happens.
            wake_at: Mutex::new(
                Instant::now()
                    .checked_sub(await_hold)
                    .unwrap_or_else(Instant::now),
            ),
            wake_owed: AtomicBool::new(false),
            watchers: AtomicU32::new(0),
            contents: Arc::default(),
            enter: RwLock::new(None),
            read_as: AtomicU8::new(reading.bits()),
            similar: Mutex::new(None),
            prepared: RwLock::new(None),
            prepared_at: Mutex::new(Instant::now() - PREPARE_EVERY),
            prepare_floor: AtomicU64::new(PREPARE_EVERY.as_micros() as u64),
            prepare: prepare_tx,
            last_read: Mutex::new(Instant::now()),
            read_since_release: std::sync::atomic::AtomicBool::new(false),
        });
        let (jobs_tx, jobs_rx) = unbounded::<Job>();
        // Bounded: a burst of events slows the watcher rather than filling memory.
        // Boxed: a bounded channel allocates every slot up front, and 65,536 of
        // an entry apiece was 10 MB held for a queue that is nearly always empty.
        let (changes_tx, changes_rx) = crossbeam_channel::bounded::<Box<Change>>(65_536);
        let worker = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("scour-worker".into())
                .spawn(move || run(shared, jobs_rx, changes_rx))
                .ok()
        };
        // Its own thread: a page has to be ready while a scan runs, not after it.
        let preparer = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("scour-prepare".into())
                .spawn(move || prepare_loop(shared, prepare_rx, prepare_stopped))
                .ok()
        };
        Engine {
            shared,
            jobs: jobs_tx,
            changes: changes_tx,
            worker: Mutex::new(worker),
            prepare_stop,
            preparer: Mutex::new(preparer),
        }
    }

    pub fn maintain(&self, level: Maintenance) -> Result<MaintReport> {
        // Flush is quick and its result is wanted; heavier levels go to the worker.
        if level == Maintenance::Flush {
            let report = self.shared.index.maintain(level)?;
            self.shared.touched_now();
            return Ok(report);
        }
        self.send(Job::Maintain(level))?;
        Ok(MaintReport {
            level,
            ..Default::default()
        })
    }

    fn send(&self, job: Job) -> Result<()> {
        self.jobs.send(job).map_err(|_| Error::Unreachable {
            detail: "the worker has stopped".into(),
        })
    }

    pub fn stats(&self) -> Result<IndexStats> {
        self.shared.index.stats()
    }

    /// Wait until the index would answer differently; `since` is the last
    /// [`Status::revision`] seen. Waiting puts commits on `commit_watched`.
    pub fn await_change(&self, since: u64, timeout: Duration) -> Status {
        let deadline = Instant::now() + timeout;
        // Asked of an engine already stopped, which never changes again:
        // answering at once had a face ask again at once, a spin for as long
        // as a swap's stop takes — and a disk that stopped answering makes that
        // long. A second, within the timeout. A waiter the stop wakes is not
        // held: that is the stop telling it to look elsewhere.
        if self.shared.stop.load(Ordering::Acquire) {
            std::thread::sleep(
                deadline
                    .saturating_duration_since(Instant::now())
                    .min(Duration::from_secs(1)),
            );
            return self.status();
        }
        self.shared.watchers.fetch_add(1, Ordering::Relaxed);
        {
            let mut held = self.shared.waiters.0.lock();
            while self.shared.revision.load(Ordering::Acquire) == since
                && !self.shared.stop.load(Ordering::Acquire)
            {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() || self.shared.waiters.1.wait_for(&mut held, left).timed_out() {
                    break;
                }
            }
        }
        self.shared.watchers.fetch_sub(1, Ordering::Relaxed);
        self.status()
    }

    pub fn status(&self) -> Status {
        let mut s = self.shared.status.read().clone();
        s.pending = self.shared.pending.load(Ordering::Relaxed);
        s.scanning = self.shared.scanning.load(Ordering::Relaxed);
        s.revision = self.shared.revision.load(Ordering::Acquire);
        if let Ok(stats) = self.shared.index.stats() {
            s.entries = stats.entries;
            s.index_bytes = stats.bytes_on_disk;
            s.unsorted = stats.unsorted_entries;
            s.rebuild_advised = stats.unsorted_entries >= self.shared.opts.rebuild_threshold;
            s.cold = stats.entries == 0;
        }
        s
    }

    /// Waiters are told to look again: something a face shows changed outside
    /// the index, such as a disk waiting for an answer.
    pub fn nudge(&self) {
        self.shared.touched_now();
    }

    /// A walk running, or one asked for that has not started: what an engine
    /// stopped now would leave undone.
    pub fn walking(&self) -> bool {
        self.shared.scanning.load(Ordering::Relaxed)
            || self.shared.queued.iter().any(|q| q.load(Ordering::Acquire))
    }

    /// Stop watching, finish what is queued, and commit. Once: the second call
    /// — `Drop`, from whichever request held the engine last — may come after
    /// another engine has taken the index over.
    pub fn shutdown(&self) {
        // Never hold the watch lock across a join: the consumer takes it too.
        {
            let _held = self.shared.waiters.0.lock();
            if self.shared.stop.swap(true, Ordering::AcqRel) {
                return;
            }
        }
        self.shared.waiters.1.notify_all();
        let handles = std::mem::take(&mut *self.shared.watches.lock());
        for (_, h) in handles {
            h.stop();
        }
        let _ = self.jobs.send(Job::Stop);
        let _ = self.prepare_stop.try_send(());
        if let Some(h) = self.worker.lock().take() {
            let _ = h.join();
        }
        if let Some(h) = self.preparer.lock().take() {
            let _ = h.join();
        }
        let _ = self.shared.index.commit();
    }
}

impl Drop for Engine {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Hands a watcher's events to the worker.
#[derive(Debug)]
struct Forward(Sender<Box<Change>>);

impl scour_core::ChangeSink for Forward {
    fn emit(&self, change: Change) {
        // A full channel slows the watcher rather than queueing the whole filesystem.
        let _ = self.0.send(Box::new(change));
    }
}

#[cfg(test)]
mod tests;
