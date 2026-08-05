//! The orchestrator.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender, select, unbounded};
use parking_lot::{Mutex, RwLock};
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

    pub fn facets(&self, query: &str, by: scour_core::FacetBy) -> Result<FacetResponse> {
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
    let tick = crossbeam_channel::tick(Duration::from_millis(100));

    loop {
        select! {
            recv(jobs) -> job => match job {
                Ok(Job::Stop) | Err(_) => break,
                Ok(Job::Scan { source, subtree }) => {
                    scan(&shared, &changes_tx, source, subtree);
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
                        let _ = shared.index.apply(&mut batch.into_iter());
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
                                scan(&shared, &changes_tx, i, None);
                            }
                        } else if let Some(i) = owner_of(&shared, &path) {
                            scan(&shared, &changes_tx, i, Some(path.clone()));
                            // The walk succeeded, so the path is real and worth
                            // watching. Where the cover was rebuilt shallow it
                            // is the only way anything below here is ever seen
                            // again; everywhere else it is a no-op.
                            for (src, h) in shared.watches.lock().iter() {
                                if *src == i {
                                    h.cover(&path);
                                }
                            }
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
        if dirty
            && waited >= shared.opts.commit_interval
            && (enough || waited >= shared.opts.commit_idle)
        {
            let _ = shared.index.commit();
            shared.pending.store(0, Ordering::Relaxed);
            dirty = false;
            dirty_settled = true;
            last_commit = Instant::now();
            idle_done = false;
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
    shared.sources.iter().position(|s| {
        s.describe().roots.iter().any(|r| {
            let r = r.trim_end_matches('/');
            path == r || (path.starts_with(r) && path.as_bytes().get(r.len()) == Some(&b'/'))
        })
    })
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
        assert_eq!(c(&["/a/", "/a/b"]), vec!["/a"], "and the slash is normalised away");
    }
}
