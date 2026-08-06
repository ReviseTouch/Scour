//! The orchestrator.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, select, unbounded};
use parking_lot::{Condvar, Mutex, RwLock};
use scour_core::{
    Change, Completion, Entry, EntrySink, Error, FacetRequest, FacetResponse, Flow, Index,
    IndexStats, MaintReport, Maintenance, Page, Result, ScanOptions, SearchRequest, SearchResponse,
    SortKey, Source, SourceInfo, Span, Status, TreeNode, WatchHandle,
};

/// One reading of a query: what it means, what its pieces are, and what could
/// come next.
#[derive(Debug, Clone, Default)]
pub struct Explained {
    pub description: String,
    pub needs_content: bool,
    pub spans: Vec<Span>,
    pub completions: Vec<Completion>,
}

#[derive(Debug, Clone)]
pub struct EngineOptions {
    pub scan: ScanOptions,
    /// How long changes accumulate before a commit.
    pub commit_interval: Duration,
    /// How large the unsorted tail may grow before a rebuild is advised.
    pub rebuild_threshold: u64,
    /// Rows a page may hold, whatever a caller asks for.
    pub result_limit: u32,
    /// How many unordered segments may accumulate before they are merged.
    ///
    /// A stream of small commits leaves one segment each, and every query pays
    /// a fixed cost per segment — opening columns, building a scorer, holding a
    /// file handle. Merging them costs seconds and does not reduce how many
    /// documents the tail holds; only a rebuild does that.
    pub compact_segments: u32,
    /// How many changes make a commit worth a segment of its own.
    ///
    /// Below this a change waits for [`EngineOptions::commit_idle`] instead.
    /// The number is small because the cost it guards against is not the write
    /// — it is that every segment is one more thing every future query has to
    /// open and walk.
    pub commit_batch: u64,
    /// How long a handful of changes may wait before being written anyway.
    ///
    /// The bound on staleness. A file created now is findable within this at
    /// worst, and within [`EngineOptions::commit_interval`] when anything else
    /// is happening at the same time.
    pub commit_idle: Duration,
    /// The same bound, while somebody is waiting to be told about changes.
    ///
    /// A search window with live results open is watching, and what it is
    /// watching for is a file that was just created. Fifteen seconds is the
    /// right answer for nobody-is-looking and the wrong one for somebody-is:
    /// the point of this whole path is that a file saved a moment ago is
    /// findable, and a bound the user can count out loud is not that.
    ///
    /// It costs a segment per commit on a machine that would otherwise have
    /// batched, which is exactly what [`EngineOptions::commit_idle`] exists to
    /// avoid — so it is paid only while a window is open and waiting, and stops
    /// being paid the moment that window closes or is hidden.
    pub commit_watched: Duration,
    /// The point at which segments are merged **without** waiting for idle.
    ///
    /// Compaction normally waits for the machine to stop asking for things,
    /// because it costs seconds and nobody should pay them mid-search. That
    /// rests on churn arriving in bursts with gaps between — which a machine
    /// that is compiling breaks completely: a continuous stream of changes
    /// means the idle moment never comes, and the segments never stop
    /// arriving. Measured here during a build: **222 segments**, and a query
    /// that answers in 8 ms at one segment taking 13 seconds.
    ///
    /// Past this many, searching costs more than merging does, and waiting
    /// for a quiet moment is waiting for the wrong thing.
    pub compact_urgent: u32,
    /// How long the index may sit untouched before it is asked to give back
    /// whatever it was holding for writes.
    pub idle_after: Duration,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            scan: ScanOptions::default(),
            commit_interval: Duration::from_millis(1_000),
            rebuild_threshold: 200_000,
            result_limit: 1_000,
            commit_batch: 64,
            commit_idle: Duration::from_secs(15),
            // The burst clock, exactly. Below it nothing would happen sooner —
            // `commit_interval` is a floor on how often a segment is written at
            // all — so anything smaller would be a number that reads faster
            // than it behaves.
            commit_watched: Duration::from_millis(1_000),
            compact_segments: 8,
            compact_urgent: 64,
            idle_after: Duration::from_secs(20),
        }
    }
}

/// Work the background thread does.
enum Job {
    /// Walk a source, or one subtree of it, and reconcile.
    Scan {
        source: usize,
        subtree: Option<String>,
    },
    Maintain(Maintenance),
    Stop,
}

struct Shared {
    sources: Vec<Arc<dyn Source>>,
    index: Arc<dyn Index>,
    opts: EngineOptions,
    status: RwLock<Status>,
    pending: AtomicU64,
    scanning: AtomicBool,
    stop: AtomicBool,
    /// The live watches, each beside the source it belongs to.
    ///
    /// Shared rather than held by the `Engine` because the worker needs them:
    /// a walk of a subtree is the moment to tell the watcher that the subtree
    /// exists, and the worker is what runs the walk. Paired with the source
    /// index so that a second source's watcher is not asked to cover a path
    /// that is not its business.
    watches: Mutex<Vec<(usize, Box<dyn WatchHandle>)>>,
    /// See [`Status::revision`].
    revision: AtomicU64,
    /// Whoever is blocked in [`Engine::await_change`].
    ///
    /// A mutex and a condition variable rather than a channel per client: every
    /// waiter wants the same wake-up, and the mutex is what closes the gap
    /// between a client reading the revision and going to sleep on it. A change
    /// landing in that gap without it is a change nobody hears about until the
    /// timeout, which is the one failure a live list must not have.
    waiters: (Mutex<()>, Condvar),
    /// How many of them there are.
    ///
    /// Read by the commit clock, and that is the whole reason it is counted: a
    /// change is worth writing sooner when something is waiting to be told
    /// about it. Zero means nobody is looking and the batching stands.
    watchers: AtomicU32,
}

impl Shared {
    /// A search run again could now answer differently.
    ///
    /// Bumped under the waiters' lock, so a client that has read the revision
    /// and not yet gone to sleep on it is not overtaken.
    fn touched(&self) {
        {
            let _held = self.waiters.0.lock();
            self.revision.fetch_add(1, Ordering::Release);
        }
        self.waiters.1.notify_all();
    }
}

pub struct Engine {
    shared: Arc<Shared>,
    jobs: Sender<Job>,
    changes: Sender<Change>,
    worker: Mutex<Option<JoinHandle<()>>>,
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
        opts: EngineOptions,
    ) -> Engine {
        let shared = Arc::new(Shared {
            status: RwLock::new(Status {
                sources: sources.len() as u32,
                ..Default::default()
            }),
            sources,
            index,
            opts,
            pending: AtomicU64::new(0),
            scanning: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            watches: Mutex::new(Vec::new()),
            revision: AtomicU64::new(0),
            waiters: (Mutex::new(()), Condvar::new()),
            watchers: AtomicU32::new(0),
        });
        let (jobs_tx, jobs_rx) = unbounded::<Job>();
        // Bounded: a burst of filesystem events must slow the watcher down
        // rather than accumulate without limit in memory.
        let (changes_tx, changes_rx) = crossbeam_channel::bounded::<Change>(65_536);
        let worker = {
            let shared = Arc::clone(&shared);
            let changes_tx = changes_tx.clone();
            std::thread::Builder::new()
                .name("scour-worker".into())
                .spawn(move || run(shared, jobs_rx, changes_rx, changes_tx))
                .ok()
        };
        Engine {
            shared,
            jobs: jobs_tx,
            changes: changes_tx,
            worker: Mutex::new(worker),
        }
    }

    pub fn sources(&self) -> Vec<SourceInfo> {
        self.shared.sources.iter().map(|s| s.describe()).collect()
    }

    /// Start watching every source that can be watched.
    ///
    /// Sources that cannot are not an error: a cloud bucket has no change feed
    /// and is reconciled by rescanning instead. `Caps` says which is which, so
    /// nothing here has to know what it is talking to.
    pub fn start_watching(&self) -> Result<u32> {
        let mut started = 0;
        let mut handles = self.shared.watches.lock();
        for (i, src) in self.shared.sources.iter().enumerate() {
            if !src.caps().contains(scour_core::Caps::WATCH) {
                continue;
            }
            match src.watch(
                &self.shared.opts.scan,
                Box::new(Forward(self.changes.clone())),
            ) {
                Ok(h) => {
                    handles.push((i, h));
                    started += 1;
                }
                // One source that will not watch does not stop the others.
                Err(_) => continue,
            }
        }
        self.shared.status.write().watching = started;
        Ok(started)
    }

    /// Subtrees no watcher is covering, gathered from every handle.
    ///
    /// Empty is the ordinary answer. When it is not, live updates are partial
    /// and the paths are what makes that actionable — one root-owned directory
    /// under a home directory is a thing a person can look at, and "watching
    /// 0" is not.
    pub fn unwatched(&self) -> Vec<String> {
        self.shared
            .watches
            .lock()
            .iter()
            .flat_map(|(_, h)| h.unwatched())
            .collect()
    }

    /// Queue a full walk of every source, or of one subtree.
    pub fn rescan(&self, subtree: Option<String>) -> Result<()> {
        match &subtree {
            Some(path) => {
                let idx = self
                    .owner_of(path)
                    .ok_or_else(|| Error::NotFound { path: path.clone() })?;
                self.send(Job::Scan {
                    source: idx,
                    subtree,
                })
            }
            None => {
                for i in 0..self.shared.sources.len() {
                    self.send(Job::Scan {
                        source: i,
                        subtree: None,
                    })?;
                }
                Ok(())
            }
        }
    }

    /// Which source owns this path?
    fn owner_of(&self, path: &str) -> Option<usize> {
        owner_of(&self.shared, path)
    }

    pub fn maintain(&self, level: Maintenance) -> Result<MaintReport> {
        // Flush is quick and callers want its result; the heavy levels go to
        // the worker so a request never blocks for minutes.
        if level == Maintenance::Flush {
            return self.shared.index.maintain(level);
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

    pub fn search(
        &self,
        query: &str,
        sort: SortKey,
        descending: bool,
        page: Page,
    ) -> Result<SearchResponse> {
        let ast = scour_query::parse(query);
        let page = Page {
            limit: page.limit.min(self.shared.opts.result_limit),
            ..page
        };
        self.shared.index.search(&SearchRequest {
            query: ast,
            sort,
            descending,
            page,
        })
    }

    /// Every facet question about one query, answered from one walk.
    pub fn facets(&self, query: &str, by: Vec<scour_core::FacetBy>) -> Result<FacetResponse> {
        self.shared.index.facets(&FacetRequest {
            query: scour_query::parse(query),
            by,
        })
    }

    /// Read a query back without running it: what it means, what its pieces
    /// are, and what could follow the caret.
    ///
    /// One call rather than three because a search box wants all of it on the
    /// same keystroke, and because the three answers have to agree with each
    /// other — they are one reading of the query, not three.
    pub fn explain(&self, query: &str, cursor: Option<u32>) -> Explained {
        let ast = scour_query::parse(query);
        Explained {
            description: scour_query::describe(&ast),
            needs_content: ast.needs_content(),
            spans: scour_query::spans(query),
            completions: match cursor {
                Some(at) => scour_query::complete(query, at as usize),
                None => Vec::new(),
            },
        }
    }

    /// One entry, from the index if it is there and from the source if not.
    ///
    /// Falling back matters: a file created a moment ago is on disk before it
    /// is in the index, and answering "not found" for something the user can
    /// see would be indefensible.
    pub fn stat(&self, path: &str) -> Result<Entry> {
        // Only a source that owns the path may answer for it.
        //
        // This used to fall back to source zero when nobody owned it, and that
        // source's `stat` is a bare `symlink_metadata` with no root check — so
        // `stat /etc/shadow` returned its size, mode and owner. No contents
        // leaked, but the size, times and permissions of any path on the
        // machine did, and it answered "does this exist" for all of them. The
        // MCP server offers this to a model as read-only and scoped to what is
        // indexed, which was not true.
        let idx = self.owner_of(path).ok_or_else(|| Error::NotFound {
            path: path.to_owned(),
        })?;
        self.shared
            .sources
            .get(idx)
            .ok_or_else(|| Error::NotFound {
                path: path.to_owned(),
            })?
            .stat(path)
    }

    pub fn tree(&self, path: &str, depth: u32, limit: u32) -> Result<TreeNode> {
        crate::tree::build(
            self.shared.index.as_ref(),
            path,
            depth,
            limit.min(self.shared.opts.result_limit),
        )
    }

    /// What a subtree weighs.
    ///
    /// Straight through to the index: this is an aggregation over a layout,
    /// and the engine has nothing to add to it but the request.
    pub fn usage(&self, req: &scour_core::UsageRequest) -> Result<scour_core::UsageResponse> {
        self.shared.index.usage(req)
    }

    pub fn stats(&self) -> Result<IndexStats> {
        self.shared.index.stats()
    }

    /// Wait until the index would answer differently, then say where things
    /// stand.
    ///
    /// `since` is the [`Status::revision`] the caller last saw. It returns at
    /// once when that is already stale, and otherwise sleeps until something
    /// changes or `timeout` passes — so a client that calls this in a loop
    /// costs one blocked thread and no requests at all while nothing happens.
    /// Polling every second instead would be 86,400 searches a day to discover
    /// that a desktop was idle.
    ///
    /// A [`Status`] rather than the number, because every caller wants the
    /// counts beside it and asking twice would be two answers from two moments.
    ///
    /// **Waiting is what makes changes commit sooner**: while anyone is in
    /// here, the engine writes on [`EngineOptions::commit_watched`] rather than
    /// letting a trickle wait out [`EngineOptions::commit_idle`]. So the
    /// answer to "why is a file I just saved not in the list" is not "wait
    /// fifteen seconds" for as long as a window is open.
    pub fn await_change(&self, since: u64, timeout: Duration) -> Status {
        let deadline = Instant::now() + timeout;
        self.shared.watchers.fetch_add(1, Ordering::Relaxed);
        {
            let mut held = self.shared.waiters.0.lock();
            while self.shared.revision.load(Ordering::Acquire) == since {
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

    /// Stop watching, finish what is queued, and commit.
    pub fn shutdown(&self) {
        for (_, h) in self.shared.watches.lock().drain(..) {
            h.stop();
        }
        self.shared.stop.store(true, Ordering::Relaxed);
        let _ = self.jobs.send(Job::Stop);
        if let Some(h) = self.worker.lock().take() {
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
struct Forward(Sender<Change>);

impl scour_core::ChangeSink for Forward {
    fn emit(&self, change: Change) {
        // A full channel means the worker is behind. Blocking here is the
        // point: it slows the watcher instead of growing a queue that would
        // eventually be the whole filesystem.
        let _ = self.0.send(change);
    }
}

/// The background thread: one loop for jobs, changes and the commit clock.
fn run(
    shared: Arc<Shared>,
    jobs: Receiver<Job>,
    changes: Receiver<Change>,
    changes_tx: Sender<Change>,
) {
    let mut dirty = false;
    // Housekeeping runs once per quiet period, not once per tick.
    let mut idle_done = false;
    // Set by a commit, cleared by the check after it: the moment a batch of
    // changes has just landed is the only one where merging costs nothing that
    // was not already being paid.
    let mut dirty_settled = false;
    let mut last_commit = Instant::now();
    // When something last arrived, as opposed to when this loop last wrote.
    let mut last_busy = Instant::now();
    let mut last_compact = Instant::now();
    // Sources whose roots were not there to be read, and when to look again.
    //
    // A service that starts with the session starts before the volumes it is
    // configured to index: `/mnt/depo` is a readable, empty directory until
    // something mounts it. Without this, the first walk finds nothing, refuses
    // to reconcile — which is the right refusal — and then nobody ever asks
    // again, so the volume stays as stale as it was until a person types
    // `scour rescan`.
    let mut retries: Vec<(usize, Instant, usize)> = Vec::new();
    let tick = crossbeam_channel::tick(Duration::from_millis(100));

    loop {
        select! {
            recv(jobs) -> job => match job {
                Ok(Job::Stop) | Err(_) => break,
                Ok(Job::Scan { source, subtree }) => {
                    let whole = subtree.is_none();
                    if !scan(&shared, &changes_tx, source, subtree) && whole {
                        schedule_retry(&mut retries, source);
                    }
                    dirty = true;
                    idle_done = false;
                    last_busy = Instant::now();
                }
                Ok(Job::Maintain(level)) => {
                    let _ = shared.index.maintain(level);
                    dirty = false;
                    last_commit = Instant::now();
                }
            },
            recv(changes) -> change => match change {
                Ok(c) => {
                    last_busy = Instant::now();
                    // Drain what is already queued so a burst becomes one
                    // batch: applying a hundred changes together costs barely
                    // more than applying one.
                    let mut batch = vec![c];
                    while let Ok(more) = changes.try_recv() {
                        batch.push(more);
                        if batch.len() >= 4_096 {
                            break;
                        }
                    }
                    // The walks come out of the batch first, because they are
                    // the expensive kind and because they overlap. Every
                    // directory a `git clone` creates asks for one — see
                    // `translate` — and unwound, that is one walk per
                    // directory, each of which flushes a segment and sweeps.
                    // Coalesced, a thousand of them are one walk of the top.
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
                        // **Removals count now, upserts at the commit.** An
                        // `apply` hides what was deleted from every search
                        // immediately — that is a promise the `Index` trait
                        // makes — while what was created is staged and invisible
                        // until it is written. Announcing both here would wake
                        // every open window to show it exactly what it already
                        // had, once per batch, on a desktop that produces
                        // thirty to fifty changes a second.
                        if let Ok(r) = report
                            && r.removed + r.subtrees_removed > 0
                        {
                            shared.touched();
                        }
                        dirty = true;
                        idle_done = false;
                    }
                    // A watcher that lost track becomes a walk of the subtree
                    // it lost. Every platform loses track differently; this is
                    // the one place that has to care.
                    for path in coalesce(walks) {
                        // An empty path means "I lost track and cannot say
                        // where" — inotify exhausting its watches, a kernel
                        // buffer overflowing. It used to match no source and be
                        // dropped, which is the worst possible reading: the one
                        // message that exists to say the index is drifting was
                        // the one message thrown away, and the drift then
                        // continued silently until someone rescanned by hand.
                        if path.is_empty() {
                            for i in 0..shared.sources.len() {
                                if !scan(&shared, &changes_tx, i, None) {
                                    schedule_retry(&mut retries, i);
                                }
                            }
                        } else if let Some(i) = owner_of(&shared, &path) {
                            // **Watched before it is walked, and the order is
                            // the whole point.** A walk is a snapshot; a watch
                            // is everything after it. The other way round —
                            // walk it, then watch it, because now we know it is
                            // real — leaves the gap between them covered by
                            // neither, which is the same race the walk exists
                            // to close, moved rather than removed.
                            //
                            // Measured on the live index, having got it
                            // backwards first: five thousand files written into
                            // two hundred fresh directories left **1,260 of
                            // them missing**, and not scattered — packages 32
                            // to 82, one unbroken run, which is the window in
                            // which the shell loop was fastest. Watching first:
                            // 5,000 of 5,000.
                            //
                            // Watching something about to be walked costs a
                            // duplicate upsert at worst, and an upsert is by
                            // identity. Where the cover was rebuilt shallow
                            // this is also the only way anything below here is
                            // ever seen again; everywhere else it is a no-op.
                            for (src, h) in shared.watches.lock().iter() {
                                if *src == i {
                                    h.cover(&path);
                                }
                            }
                            scan(&shared, &changes_tx, i, Some(path.clone()));
                        }
                        dirty = true;
                        idle_done = false;
                    }
                }
                Err(_) => break,
            },
            recv(tick) -> _ => {}
        }

        // A commit writes a segment, so committing two files costs a segment
        // holding two rows — and a browser cache touching one file a second
        // produced one segment a second for as long as the machine was on.
        // Thirty-five of them in thirty-five seconds, on an idle desktop.
        //
        // So a trickle waits for the slower clock and a burst does not: enough
        // changes, or enough time, whichever comes first. What must not happen
        // is a change sitting unwritten indefinitely, which is why the second
        // half of that sentence exists.
        let waited = last_commit.elapsed();
        let enough = shared.pending.load(Ordering::Relaxed) >= shared.opts.commit_batch;
        // How long a trickle may wait, and it depends on whether anyone is
        // watching. Nobody is: fifteen seconds, and the machine writes one
        // segment a minute instead of one a second. Somebody is: as soon as the
        // burst clock allows, because what they are waiting for is a file they
        // just saved.
        //
        // This is not a small difference by luck. Measured here, a file created
        // in a watched directory became findable in **0.90 s** — which looked
        // like the design working and was not: this desktop happens to produce
        // 30–50 filesystem changes a second, so `commit_batch` was reached
        // before the trickle clock ever mattered. On a quiet machine the same
        // file waits the full fifteen.
        let patience = if shared.watchers.load(Ordering::Relaxed) > 0 {
            shared.opts.commit_watched
        } else {
            shared.opts.commit_idle
        };
        if dirty && waited >= shared.opts.commit_interval && (enough || waited >= patience) {
            match shared.index.commit() {
                Ok(()) => {
                    // Whatever was staged is now in a segment and findable.
                    shared.touched();
                    shared.pending.store(0, Ordering::Relaxed);
                    shared.status.write().unwritten = 0;
                    dirty = false;
                    dirty_settled = true;
                    idle_done = false;
                }
                // **Still dirty, and nobody is told it changed.** The rows are
                // back in the staging buffer; clearing `dirty` here would mean
                // nothing ever tried to write them again, and announcing a
                // revision would send every open window to re-read an index
                // that did not move. The clock is reset either way so a disk
                // that is full is retried on the same cadence rather than in a
                // loop.
                Err(e) => {
                    let n = {
                        let mut st = shared.status.write();
                        st.unwritten += 1;
                        st.unwritten
                    };
                    // Once, then every thirty tries: a service whose disk is
                    // full should say so, not fill the log with saying so.
                    if n == 1 || n % 30 == 0 {
                        eprintln!("scourd: the index could not be written ({n} attempts): {e}");
                    }
                }
            }
            last_commit = Instant::now();
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
                if scan(&shared, &changes_tx, source, None) {
                    eprintln!(
                        "scourd: {} is readable again",
                        shared.sources[source].describe().name
                    );
                    dirty = true;
                    last_busy = Instant::now();
                    idle_done = false;
                } else {
                    retries.push((source, now + retry_delay(attempt + 1), attempt + 1));
                }
            }
        }

        // Merging does not wait for a quiet moment, because the quiet moment
        // does not come.
        //
        // The old rule was `compact_segments` while idle, with `compact_urgent`
        // as an escape hatch for a machine that never goes idle. Both failed
        // together, and measurably: idleness was counted from the last
        // *commit*, and a desktop produces a filesystem change every few
        // seconds, so the window never opened — while `compact_urgent` at 64
        // was above where the index actually sat. Measured over a whole
        // session: **56 segments**, `compact_segments` at 8, and neither path
        // ran once.
        //
        // It does not need quiet any more. Since a fold builds under the read
        // lock and only swaps the list under the write one, merging costs a
        // search the time it takes to swap a `Vec` — the thing that made this
        // wait was removed and the waiting was left behind. What remains is a
        // floor on how often it is worth doing at all.
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

        // Housekeeping, once the machine has stopped asking for anything.
        //
        // Counted from the last change rather than the last commit: a commit is
        // this loop's own footprint, and measuring quiet by it meant every
        // commit reset the clock that was waiting for commits to stop.
        //
        // **No automatic rebuild here any more**, and that is a measurement
        // rather than a simplification. The index had drifted to 851,471
        // unsorted entries — four times `rebuild_threshold` — and rebuilding it
        // to a single segment moved `rapor` from 26 ms to 34 and `ext:pdf` from
        // 50 to 55, with `rows_visited` unchanged at 288,000. What a broad
        // query pays for is the number of candidate rows, not the number of
        // segments holding them. `scour maintain rebuild` still exists for
        // anyone who wants the space back; spending minutes of a core on it
        // unasked, for nothing, does not.
        if !dirty && !idle_done && last_busy.elapsed() >= shared.opts.idle_after {
            // Whatever else happened, stop holding a write buffer. On an idle
            // machine this is the difference between a service that costs
            // hundreds of megabytes to leave running and one that does not.
            let _ = shared.index.maintain(Maintenance::Idle);
            idle_done = true;
        }
        if shared.stop.load(Ordering::Relaxed) && jobs.is_empty() && changes.is_empty() {
            break;
        }
    }
    let _ = shared.index.commit();
    shared.pending.store(0, Ordering::Relaxed);
}

/// Which source owns this path?
///
/// A prefix **and a separator**, not a prefix. `/home/hasan` does not own
/// `/home/hasanX`, and the version of this that lived in the change loop
/// thought it did.
fn owner_of(shared: &Shared, path: &str) -> Option<usize> {
    // **The longest root wins**, not the first one configured. A project
    // directory configured as its own source lives inside the home directory
    // that is also one; asking which source a path belongs to and taking
    // whichever happened to be listed first sends the walk, and the sweep that
    // follows it, to the wrong one.
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

/// Reduce a set of requested walks to the ones that are not already covered.
///
/// Walking `/a` walks `/a/b`, so asking for both is asking twice. This matters
/// because the requests arrive in the thousands and each one costs far more
/// than the directory it names: a walk flushes a segment, takes a generation
/// and sweeps every older segment afterwards.
///
/// The reduction itself lives in `scour-core` because the index needs the same
/// one for removals, where getting it wrong was measured at 2.24 seconds of
/// held write lock.
fn coalesce(paths: Vec<String>) -> Vec<String> {
    scour_core::PrefixSet::new(paths).into_paths()
}

/// Walk one source and reconcile what it holds.
/// Walk one source, and say whether the walk could see what it came for.
///
/// `false` means the roots were not there to be read — a volume that has not
/// been mounted yet, a drive pulled out, a share that dropped. The caller is
/// expected to come back later rather than to treat it as an answer.
fn scan(
    shared: &Arc<Shared>,
    changes: &Sender<Change>,
    source: usize,
    subtree: Option<String>,
) -> bool {
    let Some(src) = shared.sources.get(source).cloned() else {
        return true;
    };
    // A generation the index could not open is a scan that cannot reconcile:
    // its rows would be stamped with the previous one and the sweep would then
    // judge them by it. Reported and retried rather than run blind.
    let generation = match shared.index.begin_generation() {
        Ok(g) => g,
        Err(e) => {
            eprintln!(
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

    let mut sink = ToIndex {
        index: Arc::clone(&shared.index),
        seen: 0,
        buffer: Vec::with_capacity(BATCH),
        stop: Arc::clone(shared),
        failed: false,
    };
    let opts = ScanOptions {
        subtree: subtree.clone(),
        ..shared.opts.scan.clone()
    };
    let report = src.scan(&opts, &mut sink);
    sink.flush();
    let seen = sink.seen;

    // Anything under the walked subtree that this pass did not stamp is gone
    // from the filesystem. A scan can only report what it found; this is how
    // what it did not find stops being in the index.
    //
    // Which is why it must not run when the walk could not look. A cancelled
    // walk is incomplete and a walk whose root was unreadable saw nothing at
    // all; sweeping on either deletes what is merely out of reach. The failure
    // is silent and total — the index empties, `rescan` reports success, and
    // the files come back only when the root does.
    //
    // And it must not run when the *index* could not take what the walk found,
    // for the same reason from the other end: a batch that failed to apply is
    // a set of files that exist and are unstamped, so a sweep would delete
    // exactly the rows the walk was there to keep.
    //
    // **Only the roots the walk vouched for**, and sparing what it could not
    // look into. One absent removable disk used to stop a home directory being
    // reconciled at all, because the evidence was one boolean for the source;
    // and a directory that lost its read permission after being indexed lost
    // its files from the index too, because a walk that could not look was
    // treated as a walk that found nothing.
    let vouched: Vec<String> = report
        .as_ref()
        .map(|r| r.vouched.clone())
        .unwrap_or_default();
    let spare =
        scour_core::PrefixSet::new(report.as_ref().map(|r| r.blind.clone()).unwrap_or_default());
    let could_look = !vouched.is_empty();
    let trustworthy = report.as_ref().is_ok_and(|r| !r.cancelled) && could_look && !sink.failed;
    if trustworthy {
        let mut gone = 0;
        for r in vouched {
            match shared.index.sweep(src.id(), &r, generation, &spare) {
                Ok(n) => gone += n,
                // Half a reconciliation. Saying so is all that can be done
                // here; the retry is the caller's, and the rows that should
                // have gone are found again by the next full scan.
                Err(e) => eprintln!("scourd: {r} could not be reconciled: {e}"),
            }
        }
        // A sweep takes effect at once, like any other removal, so anyone
        // watching should hear about it now rather than at the next commit.
        // The walk's own upserts are staged and announce themselves then.
        if gone > 0 {
            shared.touched();
        }
    }
    let _ = changes;

    shared.scanning.store(false, Ordering::Relaxed);
    let mut st = shared.status.write();
    st.scanning_source = None;
    st.scanned = seen;
    st.last_scan_ms = report.map(|r| r.took_ms).unwrap_or(0);
    // Only a whole-source walk is worth coming back for. A subtree that is not
    // there is a subtree that was deleted, and something has already said so.
    // A failed write is worth coming back for whatever was walked.
    !sink.failed && (could_look || subtree.is_some())
}

/// How long to wait before looking again at a source whose roots were not there.
///
/// The case this exists for is a machine that has just started: the service is
/// up before the volumes are, so the first walk of an external disk finds an
/// empty mount point. Short enough that a disk appearing a moment later is
/// picked up while the user is still logging in, long enough that a drive left
/// unplugged for a week costs one directory listing an hour.
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
fn schedule_retry(retries: &mut Vec<(usize, Instant, usize)>, source: usize) {
    if !retries.iter().any(|(s, _, _)| *s == source) {
        retries.push((source, Instant::now() + retry_delay(0), 0));
    }
}

/// The floor on how often segments are merged without being asked.
///
/// Not a cost of merging — a fold no longer holds anything a search needs —
/// but a cost of *deciding*: `stats()` reads every segment's live count, and
/// there is no point paying it on a machine whose segment count moves by one
/// every few seconds.
const COMPACT_EVERY: Duration = Duration::from_secs(60);

const BATCH: usize = 4_096;

/// Turns a walk into index updates, in batches.
struct ToIndex {
    index: Arc<dyn Index>,
    seen: u64,
    buffer: Vec<Change>,
    stop: Arc<Shared>,
    /// A batch the index refused.
    ///
    /// **The reason a scan has to know**: what makes a walk a reconciliation is
    /// the sweep at the end, which removes everything the walk did not stamp.
    /// A batch that never landed is a set of files the walk *did* find and the
    /// index does not have — so sweeping on that evidence deletes them. One
    /// failed write would turn a scan into a deletion.
    failed: bool,
}

impl ToIndex {
    fn flush(&mut self) {
        if self.buffer.is_empty() {
            return;
        }
        let batch = std::mem::replace(&mut self.buffer, Vec::with_capacity(BATCH));
        if let Err(e) = self.index.apply(&mut batch.into_iter()) {
            eprintln!("scourd: a batch of the scan was not indexed: {e}");
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
        }
        if self.stop.stop.load(Ordering::Relaxed) {
            Flow::Stop
        } else {
            Flow::Continue
        }
    }
}

#[cfg(test)]
mod tests {
    use super::coalesce;

    fn c(v: &[&str]) -> Vec<String> {
        coalesce(v.iter().map(|s| (*s).to_owned()).collect())
    }

    #[test]
    fn a_walk_of_a_parent_absorbs_every_walk_below_it() {
        // What makes this worth doing: a `git clone` creates a directory per
        // package and each one asks for a walk, and a walk is not cheap —
        // it flushes a segment, takes a generation and sweeps afterwards.
        assert_eq!(c(&["/a", "/a/b", "/a/b/c", "/a/d"]), vec!["/a"]);
        assert_eq!(c(&["/a/b/c", "/a/b", "/a"]), vec!["/a"]);
    }

    #[test]
    fn siblings_are_not_absorbed_and_neither_is_a_longer_name() {
        assert_eq!(c(&["/a/b", "/a/c"]), vec!["/a/b", "/a/c"]);
        // The trap `under` has too: a sibling whose name starts with the
        // prefix is not inside it.
        assert_eq!(c(&["/a/b", "/a/bc"]), vec!["/a/b", "/a/bc"]);
    }

    #[test]
    fn the_same_walk_asked_for_twice_is_one_walk() {
        assert_eq!(c(&["/a/b", "/a/b", "/a/b"]), vec!["/a/b"]);
    }

    #[test]
    fn an_empty_path_means_everything_and_subsumes_the_rest() {
        // The one message that says "I lost track and cannot say where".
        // Walking everything covers every other request by definition.
        assert_eq!(c(&["/a", "", "/b"]), vec![String::new()]);
    }

    #[test]
    fn a_trailing_slash_does_not_hide_a_child() {
        assert_eq!(
            c(&["/a/", "/a/b"]),
            vec!["/a"],
            "and the slash is normalised away"
        );
    }
}
