//! The orchestrator.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, select, unbounded};
use parking_lot::{Mutex, RwLock};
use scour_core::{
    Change, Entry, EntrySink, Error, FacetRequest, FacetResponse, Flow, Index, IndexStats,
    MaintReport, Maintenance, Page, Result, ScanOptions, SearchRequest, SearchResponse, SortKey,
    Source, SourceInfo, Status, TreeNode, WatchHandle,
};

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
            compact_segments: 8,
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
}

pub struct Engine {
    shared: Arc<Shared>,
    jobs: Sender<Job>,
    changes: Sender<Change>,
    worker: Mutex<Option<JoinHandle<()>>>,
    watches: Mutex<Vec<Box<dyn WatchHandle>>>,
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
            watches: Mutex::new(Vec::new()),
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
        let mut handles = self.watches.lock();
        for src in &self.shared.sources {
            if !src.caps().contains(scour_core::Caps::WATCH) {
                continue;
            }
            match src.watch(Box::new(Forward(self.changes.clone()))) {
                Ok(h) => {
                    handles.push(h);
                    started += 1;
                }
                // One source that will not watch does not stop the others.
                Err(_) => continue,
            }
        }
        self.shared.status.write().watching = started;
        Ok(started)
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
        self.shared.sources.iter().position(|s| {
            s.describe().roots.iter().any(|r| {
                let r = r.trim_end_matches('/');
                path == r || (path.starts_with(r) && path.as_bytes().get(r.len()) == Some(&b'/'))
            })
        })
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

    pub fn facets(&self, query: &str, by: scour_core::FacetBy) -> Result<FacetResponse> {
        self.shared.index.facets(&FacetRequest {
            query: scour_query::parse(query),
            by,
        })
    }

    /// Read a query back without running it.
    pub fn explain(&self, query: &str) -> (String, bool) {
        let ast = scour_query::parse(query);
        (scour_query::describe(&ast), ast.needs_content())
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

    pub fn stats(&self) -> Result<IndexStats> {
        self.shared.index.stats()
    }

    pub fn status(&self) -> Status {
        let mut s = self.shared.status.read().clone();
        s.pending = self.shared.pending.load(Ordering::Relaxed);
        s.scanning = self.shared.scanning.load(Ordering::Relaxed);
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
        for h in self.watches.lock().drain(..) {
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
    let mut last_commit = Instant::now();
    let tick = crossbeam_channel::tick(Duration::from_millis(100));

    loop {
        select! {
            recv(jobs) -> job => match job {
                Ok(Job::Stop) | Err(_) => break,
                Ok(Job::Scan { source, subtree }) => {
                    scan(&shared, &changes_tx, source, subtree);
                    dirty = true;
                    idle_done = false;
                }
                Ok(Job::Maintain(level)) => {
                    let _ = shared.index.maintain(level);
                    dirty = false;
                    last_commit = Instant::now();
                }
            },
            recv(changes) -> change => match change {
                Ok(c) => {
                    // A watcher that lost track becomes a walk of the subtree
                    // it lost. Every platform loses track differently; this is
                    // the one place that has to care.
                    if let Change::Rescan { path } = &c {
                        let owner = shared.sources.iter().position(|s| {
                            s.describe().roots.iter().any(|r| path.starts_with(r.as_str()))
                        });
                        if let Some(i) = owner {
                            let sub = (!path.is_empty()).then(|| path.clone());
                            scan(&shared, &changes_tx, i, sub);
                            dirty = true;
                        }
                        continue;
                    }
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
                    shared.pending.fetch_add(batch.len() as u64, Ordering::Relaxed);
                    let _ = shared.index.apply(&mut batch.into_iter());
                    dirty = true;
                    idle_done = false;
                }
                Err(_) => break,
            },
            recv(tick) -> _ => {}
        }

        if dirty && last_commit.elapsed() >= shared.opts.commit_interval {
            let _ = shared.index.commit();
            shared.pending.store(0, Ordering::Relaxed);
            dirty = false;
            last_commit = Instant::now();
            idle_done = false;
        }

        // Housekeeping, once the machine has stopped asking for anything.
        //
        // Both of these were reported and acted on by nobody: an index would
        // advise a rebuild forever and accumulate segments forever, waiting for
        // someone to type a command. Doing it while idle is the whole point —
        // neither is something to run while the user is waiting on a search.
        if !dirty && !idle_done && last_commit.elapsed() >= shared.opts.idle_after {
            if let Ok(stats) = shared.index.stats() {
                if stats.segments > shared.opts.compact_segments {
                    let _ = shared.index.maintain(Maintenance::Compact);
                } else if stats.unsorted_entries >= shared.opts.rebuild_threshold {
                    let _ = shared.index.maintain(Maintenance::Rebuild);
                }
            }
            // Whatever happened, stop holding a write buffer. On an idle
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

/// Walk one source and reconcile what it holds.
fn scan(shared: &Arc<Shared>, changes: &Sender<Change>, source: usize, subtree: Option<String>) {
    let Some(src) = shared.sources.get(source).cloned() else {
        return;
    };
    let Ok(generation) = shared.index.begin_generation() else {
        return;
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
    let trustworthy = report
        .as_ref()
        .is_ok_and(|r| !r.cancelled && !r.root_unreadable);
    if trustworthy {
        let roots: Vec<String> = match &subtree {
            Some(s) => vec![s.clone()],
            None => src.describe().roots,
        };
        for r in roots {
            let _ = shared.index.sweep(&r, generation);
        }
    }
    let _ = changes;

    shared.scanning.store(false, Ordering::Relaxed);
    let mut st = shared.status.write();
    st.scanning_source = None;
    st.scanned = seen;
    st.last_scan_ms = report.map(|r| r.took_ms).unwrap_or(0);
}

const BATCH: usize = 4_096;

/// Turns a walk into index updates, in batches.
struct ToIndex {
    index: Arc<dyn Index>,
    seen: u64,
    buffer: Vec<Change>,
    stop: Arc<Shared>,
}

impl ToIndex {
    fn flush(&mut self) {
        if self.buffer.is_empty() {
            return;
        }
        let batch = std::mem::replace(&mut self.buffer, Vec::with_capacity(BATCH));
        let _ = self.index.apply(&mut batch.into_iter());
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
