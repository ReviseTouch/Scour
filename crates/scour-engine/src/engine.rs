//! The orchestrator.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::reconcile::{Nudge, Pulses};
use crossbeam_channel::{Receiver, Sender, select, unbounded};
use parking_lot::{Condvar, Mutex, RwLock};

use scour_core::{
    Change, Completion, Entry, EntrySink, Error, FacetRequest, FacetResponse, Flow, Hit, Index,
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
    /// What the walk and the watchers skip, to begin with. Taken by [`Engine::new`]
    /// and never read again; [`Engine::set_scan_options`] replaces the live copy.
    pub scan: ScanOptions,
    /// How long changes accumulate before a commit.
    pub commit_interval: Duration,
    /// How large the unsorted tail may grow before a rebuild is advised.
    pub rebuild_threshold: u64,
    /// Rows a page may hold, whatever a caller asks for.
    pub result_limit: u32,
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
    pub reconcile_interval: Duration,
    /// How long a [`Change::Rescan`](scour_core::Change::Rescan) waits for its
    /// neighbours: 157 walks in twelve minutes of a build, 39.4 MB of writes.
    pub walk_debounce: Duration,
    /// The ceiling on that wait, however long the cluster keeps arriving. The
    /// whole of the latency the debounce can cost.
    pub walk_debounce_cap: Duration,
    /// How often [`Engine::await_change`] may wake the clients waiting in it; zero
    /// wakes on every bump. 3.5–6.2 bumps a second at 20.5–21 ms of CPU each.
    pub await_hold: Duration,
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
            // The burst clock exactly: `commit_interval` floors how often a segment is written.
            commit_watched: Duration::from_millis(1_000),
            compact_segments: 8,
            compact_urgent: 64,
            idle_after: Duration::from_secs(20),
            poll_interval: Duration::from_secs(60),
            reconcile_interval: Duration::from_secs(1_800),
            // Walks arrive at 13.3 a minute in bursts of 22 in five seconds: half a second is a real gap.
            walk_debounce: Duration::from_millis(500),
            // The worst a file in a brand-new directory waits; deliberately under `commit_idle`.
            walk_debounce_cap: Duration::from_secs(3),
            // `commit_watched`, exactly: a waiter cannot be shown what has not been written.
            await_hold: Duration::from_millis(1_000),
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
    /// What the walk and the watchers skip. Behind a lock because a person edits it
    /// while the service runs; an `Arc` inside so a walk copies it and lets go.
    scan: RwLock<Arc<ScanOptions>>,
    status: RwLock<Status>,
    pending: AtomicU64,
    scanning: AtomicBool,
    /// Per source: a whole-source walk is already waiting to run. Cleared as the
    /// walk begins, so a rule saved mid-walk still queues one behind it.
    queued: Vec<AtomicBool>,
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
    /// How many of them there are. Read by the commit clock: zero means nobody is
    /// looking and the batching stands.
    watchers: AtomicU32,
}

/// What the preparing thread is asked to have ready.
struct Prepare {
    query: String,
    sort: SortKey,
    descending: bool,
    count_cap: u32,
}

/// One query's ordered hits, kept so that paging through them is free: a page
/// otherwise costs `offset + limit` per segment, 16 ms at the top and 114 deep.
struct Prepared {
    query: String,
    parsed: scour_core::Ast,
    sort: SortKey,
    descending: bool,
    count_cap: u32,
    /// The index revision this was built from; anything else makes it wrong.
    revision: u64,
    hits: Vec<Hit>,
    total: u64,
    capped: bool,
}

/// How many ordered hits are kept hot — the ceiling on what any page may ask for.
/// Roughly a hundred bytes a hit, and there is only ever one of these.
const PREPARE: u32 = 20_000;

/// How often an ordering may be built, at the very least: a floor, not the whole
/// rule — one window of two hundred rows spans 3.2 ms to 2,463.6 ms by sort key.
const PREPARE_EVERY: Duration = Duration::from_secs(2);

/// Cheap pages already meet the interactive budget. Building a much larger
/// window for them spends memory and competes with the next keystroke.
const PREPARE_MIN_COST: Duration = Duration::from_millis(20);

/// What a walk buys the next one: it waits at least this many times what the last
/// took, so speculation costs at most a tenth of a machine. Deliberately uncapped.
const PREPARE_COST: u32 = 10;

/// How many of the biggest files are considered for duplication. A generous
/// ceiling: on 1,474,650 files, everything over a megabyte is 18,723 of them.
const CANDIDATES: u32 = 200_000;

/// How much CSV goes into one frame of an export. A frame is a line of JSON, so a
/// row per frame pays an envelope and a write each; 128 KB is about two thousand.
const EXPORT_CHUNK: usize = 128 * 1024;

/// The parts of a query the parser could not read as written. Answered here, by
/// what read it: 0.5 µs for five terms, against 160 µs for the fastest search.
fn misread(query: &str) -> Vec<scour_core::Span> {
    scour_query::spans(query)
        .into_iter()
        .filter(|s| s.role.is_warning())
        .collect()
}

impl Shared {
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
    changes: Sender<Change>,
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
        let shared = Arc::new(Shared {
            status: RwLock::new(Status {
                sources: sources.len() as u32,
                ..Default::default()
            }),
            sources,
            index,
            opts,
            scan,
            pending: AtomicU64::new(0),
            scanning: AtomicBool::new(false),
            queued: (0..source_count).map(|_| AtomicBool::new(false)).collect(),
            stop: AtomicBool::new(false),
            watches: Mutex::new(Vec::new()),
            revision: AtomicU64::new(0),
            waiters: (Mutex::new(()), Condvar::new()),
            // A hold ago, so the session's first change is announced as it happens.
            wake_at: Mutex::new(
                Instant::now()
                    .checked_sub(await_hold)
                    .unwrap_or_else(Instant::now),
            ),
            wake_owed: AtomicBool::new(false),
            watchers: AtomicU32::new(0),
            prepared: RwLock::new(None),
            prepared_at: Mutex::new(Instant::now() - PREPARE_EVERY),
            prepare_floor: AtomicU64::new(PREPARE_EVERY.as_micros() as u64),
            prepare: prepare_tx,
        });
        let (jobs_tx, jobs_rx) = unbounded::<Job>();
        // Bounded: a burst of events slows the watcher rather than filling memory.
        let (changes_tx, changes_rx) = crossbeam_channel::bounded::<Change>(65_536);
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

    pub fn sources(&self) -> Vec<SourceInfo> {
        self.shared.sources.iter().map(|s| s.describe()).collect()
    }

    /// Start watching every source that can be watched. One that cannot is not an
    /// error: it has no change feed and is reconciled by rescanning instead.
    pub fn start_watching(&self) -> Result<u32> {
        let mut started = 0;
        let scan = self.shared.scan();
        let mut handles = self.shared.watches.lock();
        for (i, src) in self.shared.sources.iter().enumerate() {
            if !src.caps().contains(scour_core::Caps::WATCH) {
                continue;
            }
            match src.watch(&scan, Box::new(Forward(self.changes.clone()))) {
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

    /// Subtrees no watcher is covering, gathered from every handle. Empty is the
    /// ordinary answer; when it is not, live updates are partial.
    pub fn unwatched(&self) -> Vec<String> {
        self.shared
            .watches
            .lock()
            .iter()
            .flat_map(|(_, h)| h.unwatched())
            .collect()
    }

    /// Watch everything, then walk it — a walk is a snapshot and a watch is
    /// everything after it, so walking first leaves a window covered by neither.
    pub fn cover_then_walk(&self, walk: bool) -> Result<(u32, Vec<String>)> {
        let started = self.start_watching()?;
        let skipped = self.unwatched();
        if walk {
            self.rescan(None)?;
        }
        Ok((started, skipped))
    }

    /// Queue a full walk of every source, or of one subtree. A whole-source walk
    /// already queued is not queued again, and the flag clears when the walk starts.
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
                    if self.shared.queued[i].swap(true, Ordering::AcqRel) {
                        continue;
                    }
                    if let Err(e) = self.send(Job::Scan {
                        source: i,
                        subtree: None,
                    }) {
                        // Nothing will clear it, because nothing will run it.
                        self.shared.queued[i].store(false, Ordering::Release);
                        return Err(e);
                    }
                }
                Ok(())
            }
        }
    }

    /// Look at these paths again, now: a watcher takes three to eight seconds, too
    /// long for a watched row. Re-reads only; paths outside every source are skipped.
    pub fn recheck(&self, paths: &[String]) -> Result<usize> {
        let sink = Forward(self.changes.clone());
        let mut done = 0;
        // Grouped by owner, because `recheck` is a source's method.
        let mut by_source: std::collections::BTreeMap<usize, Vec<String>> = Default::default();
        for path in paths {
            if let Some(idx) = self.owner_of(path) {
                by_source.entry(idx).or_default().push(path.clone());
            }
        }
        for (idx, group) in by_source {
            done += self.shared.sources[idx].recheck(&group, &sink);
        }
        Ok(done)
    }

    /// Which source owns this path?
    fn owner_of(&self, path: &str) -> Option<usize> {
        owner_of(&self.shared, path)
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

    /// What the walk and the watchers are skipping, right now: built-in, configured
    /// and window-added rules merged. A snapshot, not a view.
    pub fn scan_options(&self) -> Arc<ScanOptions> {
        self.shared.scan()
    }

    /// Skip by these from now on, and tell the watchers, or a rule would sweep a
    /// tree clean and the watcher would put it straight back. Does not scan.
    pub fn set_scan_options(&self, opts: ScanOptions) {
        let fresh = Arc::new(opts);
        *self.shared.scan.write() = Arc::clone(&fresh);
        // Under the lock the worker covers a subtree with: never both in one handle.
        for (_, handle) in self.shared.watches.lock().iter() {
            handle.retune(&fresh);
        }
    }

    /// Throw out everything the rules in force would now skip. No walk: excluding
    /// only removes, and removal is by subtree — 1.3 µs for 378,100 rows.
    pub fn apply_rules(&self) -> Result<u64> {
        let opts = self.shared.scan();
        // One per source. `None` means no exclusions, and its rows are left alone.
        let tests: Vec<Option<Box<dyn Fn(&str, bool) -> bool + Send + Sync>>> = self
            .shared
            .sources
            .iter()
            .map(|s| s.excluder(&opts))
            .collect();
        if tests.iter().all(Option::is_none) {
            return Ok(0);
        }

        let mut doomed: Vec<String> = Vec::new();
        self.shared.index.scan(
            &scour_core::ScanRequest {
                // No `..Default::default()`: a second field appearing must stop this compiling.
                query: scour_query::parse(""),
            },
            &mut |hit: &Hit| {
                // A `Hit` does not carry its source, and a path outside a source's
                // roots fails its root check before any rule is read.
                if tests
                    .iter()
                    .flatten()
                    .any(|excluded| excluded(hit.path.as_str(), hit.is_dir))
                {
                    doomed.push(hit.path.clone());
                }
                true
            },
        )?;

        // Folded to the topmost path of each excluded tree, one removal per tree.
        let doomed = coalesce(doomed);
        if std::env::var_os("SCOUR_TRACE_RULES").is_some() {
            for p in doomed.iter().take(8) {
                scour_core::note!("scourd: rule drops {p}");
            }
        }
        let n = doomed.len() as u64;
        if n > 0 {
            let mut changes = doomed
                .into_iter()
                .map(|path| Change::RemoveSubtree { path });
            self.shared.index.apply(&mut changes)?;
            self.shared.index.commit()?;
            self.shared.touched_now();
        }
        Ok(n)
    }

    /// One page of one query. Served from [`Prepared`] when the query's order is
    /// known, which is what makes row nineteen thousand cost what row zero does.
    pub fn search(
        &self,
        query: &str,
        sort: SortKey,
        descending: bool,
        page: Page,
    ) -> Result<SearchResponse> {
        let started = Instant::now();
        let mut res = self.page_of(query, sort, descending, page)?;
        // On every path, cached included: a warning that comes and goes teaches nothing.
        res.misread = misread(query);
        self.weigh_folders(&mut res);
        res.took_us = started.elapsed().as_micros() as u64;
        Ok(res)
    }

    /// Fill in what each folder on this page holds, in one batched call outside the
    /// row loop. A failure leaves `None`, where a zero would read as an empty folder.
    fn weigh_folders(&self, res: &mut SearchResponse) {
        let where_dirs: Vec<usize> = res
            .hits
            .iter()
            .enumerate()
            .filter(|(_, h)| h.is_dir)
            .map(|(i, _)| i)
            .collect();
        if where_dirs.is_empty() {
            return;
        }
        let paths: Vec<String> = where_dirs
            .iter()
            .map(|&i| res.hits[i].path.clone())
            .collect();
        let Ok(sizes) = self.shared.index.subtree_sizes(&paths) else {
            return;
        };
        for (&i, size) in where_dirs.iter().zip(sizes) {
            res.hits[i].under = size.map(|(disk, files)| scour_core::Subtree { disk, files });
        }
    }

    fn page_of(
        &self,
        query: &str,
        sort: SortKey,
        descending: bool,
        page: Page,
    ) -> Result<SearchResponse> {
        let page = Page {
            limit: page.limit.min(self.shared.opts.result_limit),
            ..page
        };
        let started = Instant::now();
        if let Some(res) = self.sliced(query, sort, descending, &page, started) {
            return Ok(res);
        }
        // Ask, and answer this page the long way. Only for a deep page, and never
        // under a scan, which cost 27-28% of a core on orderings nothing redeemed.
        let floor = Duration::from_micros(self.shared.prepare_floor.load(Ordering::Relaxed));
        if page.offset == 0
            || page.limit == 0
            || page.offset.saturating_add(page.limit) > PREPARE
            || self.shared.scanning.load(Ordering::Acquire)
            || self.shared.prepared_at.lock().elapsed() < floor
        {
            return self.shared.index.search(&SearchRequest {
                query: scour_query::parse(query),
                sort,
                descending,
                page,
            });
        }
        let res = self.shared.index.search(&SearchRequest {
            query: scour_query::parse(query),
            sort,
            descending,
            page,
        })?;
        if started.elapsed() < PREPARE_MIN_COST {
            return Ok(res);
        }
        let mut prepared_at = self.shared.prepared_at.lock();
        // A simultaneous expensive page may already have requested it.
        if prepared_at.elapsed() < floor || self.shared.scanning.load(Ordering::Acquire) {
            return Ok(res);
        }
        *prepared_at = Instant::now();
        let _ = self.shared.prepare.try_send(Prepare {
            query: query.to_string(),
            sort,
            descending,
            count_cap: page.count_cap,
        });
        Ok(res)
    }

    /// The page, if the order it belongs to is already known. Refuses on anything it
    /// cannot answer exactly, including a page reaching past what was prepared.
    fn sliced(
        &self,
        query: &str,
        sort: SortKey,
        descending: bool,
        page: &Page,
        started: Instant,
    ) -> Option<SearchResponse> {
        let held = self.shared.prepared.read();
        let ready = held.as_ref()?;
        if ready.query != query
            || ready.sort != sort
            || ready.descending != descending
            || ready.count_cap != page.count_cap
            || ready.revision != self.shared.revision.load(Ordering::Acquire)
            // Relative time windows change even while the index is quiet.
            || ready.parsed != scour_query::parse(query)
        {
            return None;
        }
        let offset = page.offset as usize;
        // Short of the ceiling, the walk reached the end: past it is empty, not unknown.
        let complete = ready.hits.len() < PREPARE as usize;
        if offset > ready.hits.len() && !complete {
            return None;
        }
        let end = (offset + page.limit as usize).min(ready.hits.len());
        if end < offset + page.limit as usize && !complete {
            return None;
        }
        Some(SearchResponse {
            hits: ready.hits[offset.min(end)..end].to_vec(),
            total: ready.total,
            capped: ready.capped,
            took_us: started.elapsed().as_micros() as u64,
            fast_path: true,
            rows_visited: 0,
            rows_built: 0,
            // Stamped by `search` for every path alike.
            misread: Vec::new(),
        })
    }

    /// The whole matching set, as CSV, in pieces; returns how many rows were written.
    /// `out` returns false to stop, which abandons the walk. Nothing grows with it.
    pub fn export(
        &self,
        query: &str,
        columns: &[String],
        mut out: impl FnMut(String) -> bool,
    ) -> Result<u64> {
        let sheet = if columns.is_empty() {
            scour_export::Sheet::new(scour_export::Sheet::default_columns())
        } else {
            scour_export::Sheet::new(columns.to_vec())
        };
        let mut buf: Vec<u8> = Vec::with_capacity(EXPORT_CHUNK + 4096);
        sheet.header(&mut buf);

        // A caller that went away is not a query that ran out of rows.
        let mut stopped = false;
        let mut flush = |buf: &mut Vec<u8>, stopped: &mut bool| {
            if buf.is_empty() {
                return true;
            }
            // UTF-8 by construction: rows are built from `String` paths.
            let text = String::from_utf8_lossy(buf).into_owned();
            buf.clear();
            if out(text) {
                true
            } else {
                *stopped = true;
                false
            }
        };

        let mut wrote = 0u64;
        let scanned = self.shared.index.scan(
            &scour_core::ScanRequest {
                query: scour_query::parse(query),
            },
            &mut |hit| {
                sheet.row(hit, &mut buf);
                wrote += 1;
                if buf.len() >= EXPORT_CHUNK {
                    return flush(&mut buf, &mut stopped);
                }
                true
            },
        )?;
        debug_assert_eq!(scanned, wrote);
        if !stopped {
            // The tail, and the header alone when nothing matched.
            flush(&mut buf, &mut stopped);
        }
        Ok(wrote)
    }

    /// Every facet question about one query, answered from one walk.
    pub fn facets(&self, query: &str, by: Vec<scour_core::FacetBy>) -> Result<FacetResponse> {
        let mut res = self.shared.index.facets(&FacetRequest {
            query: scour_query::parse(query),
            by,
        })?;
        res.misread = misread(query);
        Ok(res)
    }

    /// Read a query back without running it: what it means, what its pieces are, and
    /// what could follow the caret — one call, so the three answers agree.
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

    /// One entry, from the index if it is there and from the source if not: a file
    /// created a moment ago is on disk before it is indexed.
    pub fn stat(&self, path: &str) -> Result<Entry> {
        // Only an owning source may answer: `stat` is a bare `symlink_metadata`, so
        // any fallback would disclose every path on the machine.
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

    /// What a subtree weighs — all of it, or only the part a query names; an empty
    /// query is the `du` question. Parsed here, so a frontend sends only typed text.
    pub fn usage(&self, path: &str, top: u32, query: &str) -> Result<scour_core::UsageResponse> {
        self.shared.index.usage(&scour_core::UsageRequest {
            path: path.to_owned(),
            top,
            query: scour_query::parse(query),
        })
    }

    /// The same file, several times over. The query is built here, not taken, so
    /// nobody can ask for duplicate directories; `result_limit` would clamp it.
    pub fn duplicates(
        &self,
        under: &str,
        opts: &scour_dupes::Options,
    ) -> Result<scour_dupes::Report> {
        let mut query = format!("file: size:>={}", opts.min_size);
        if !under.is_empty() {
            // Quoted: an unquoted path with a space becomes two terms.
            query.push_str(&format!(" under:\"{under}\""));
        }
        let found = self.shared.index.search(&SearchRequest {
            query: scour_query::parse(&query),
            sort: SortKey::Size,
            descending: true,
            page: Page {
                offset: 0,
                limit: CANDIDATES,
                count_cap: CANDIDATES,
            },
        })?;
        Ok(scour_dupes::find(
            found
                .hits
                .into_iter()
                .map(|h| (h.path, h.meta.size.max(0) as u64)),
            opts,
        ))
    }

    pub fn stats(&self) -> Result<IndexStats> {
        self.shared.index.stats()
    }

    /// Wait until the index would answer differently; `since` is the last
    /// [`Status::revision`] seen. Waiting puts commits on `commit_watched`.
    pub fn await_change(&self, since: u64, timeout: Duration) -> Status {
        let deadline = Instant::now() + timeout;
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

    /// Stop watching, finish what is queued, and commit.
    pub fn shutdown(&self) {
        // Never hold the watch lock across a join: the consumer takes it too.
        {
            let _held = self.shared.waiters.0.lock();
            self.shared.stop.store(true, Ordering::Release);
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
struct Forward(Sender<Change>);

impl scour_core::ChangeSink for Forward {
    fn emit(&self, change: Change) {
        // A full channel slows the watcher rather than queueing the whole filesystem.
        let _ = self.0.send(change);
    }
}

/// Keeps the ordering of whatever query was asked for last. The channel holds one,
/// and the result is dropped if the index moved while it was built.
fn prepare_loop(shared: Arc<Shared>, jobs: Receiver<Prepare>, stop: Receiver<()>) {
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
        let parsed = scour_query::parse(&job.query);
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

/// The background thread: one loop for jobs, changes and the commit clock.
fn run(shared: Arc<Shared>, jobs: Receiver<Job>, changes: Receiver<Change>) {
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
    // Roots that were not there to be read: a session starts before its mounts do.
    let mut retries: Vec<(usize, Instant, usize)> = Vec::new();
    // Subtree walks asked for and waiting for their neighbours.
    let mut pending_walks = PendingWalks::default();
    let mut pulses = Pulses::new(
        shared.sources.len(),
        shared.opts.poll_interval,
        shared.opts.reconcile_interval,
    );

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
                shared.opts.commit_idle
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
                    if !scan(&shared, &mut pulses, source, subtree) {
                        schedule_retry(&mut retries, &pulses, source);
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
                    let mut batch = vec![c];
                    while let Ok(more) = changes.try_recv() {
                        batch.push(more);
                        if batch.len() >= 4_096 {
                            break;
                        }
                    }
                    // A handful is enough to prove the watcher is awake.
                    for change in batch.iter().take(64) {
                        if let Some(i) = owner_of(&shared, change.path()) {
                            pulses.saw_event(i);
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
                                    schedule_retry(&mut retries, &pulses, i);
                                }
                            }
                        }
                        dirty = true;
                        idle_done = false;
                        // The ones that waited out a walk; see the scan arm.
                        if overdue {
                            overdue = false;
                            last_commit = Instant::now() - shared.opts.commit_idle;
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
            // An empty path means "I lost track and cannot say where" — exhausted
            // inotify watches, an overflowed kernel buffer — so every source is walked.
            if path.is_empty() {
                for i in 0..shared.sources.len() {
                    if !scan(&shared, &mut pulses, i, None) {
                        schedule_retry(&mut retries, &pulses, i);
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
                if !scan(&shared, &mut pulses, i, Some(path.clone())) {
                    schedule_retry(&mut retries, &pulses, i);
                }
            }
            dirty = true;
            idle_done = false;
            last_busy = Instant::now();
        }

        // The pulses, read outside the wait, where a steady stream of changes would
        // starve them. `is_due` carries the two-second floor, so asking is a compare.
        let nudges = if pulses.is_due() {
            let watched: Vec<usize> = shared
                .watches
                .lock()
                .iter()
                .filter(|(_, handle)| handle.unwatched().is_empty())
                .map(|(i, _)| *i)
                .collect();
            let readings: Vec<Option<u64>> = shared.sources.iter().map(|s| s.pulse()).collect();
            pulses.decide(&readings, &watched)
        } else {
            Vec::new()
        };
        for (source, job) in nudges {
            // Failed passes have their own widening retry clock.
            if retries.iter().any(|(s, _, _)| *s == source) {
                continue;
            }
            match job {
                Nudge::Reconcile => {
                    if !scan(&shared, &mut pulses, source, None) {
                        schedule_retry(&mut retries, &pulses, source);
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
                    if !scan(&shared, &mut pulses, source, None) {
                        schedule_retry(&mut retries, &pulses, source);
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
                if scan(&shared, &mut pulses, source, None) {
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
            let _ = shared.index.maintain(Maintenance::Idle);
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
fn owner_of(shared: &Shared, path: &str) -> Option<usize> {
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

/// Reduce a set of requested walks to the ones not already covered: walking `/a`
/// walks `/a/b`, and a walk flushes a segment, takes a generation and sweeps.
fn coalesce(paths: Vec<String>) -> Vec<String> {
    scour_core::PrefixSet::new(paths).into_paths()
}

/// Walks asked for and not run yet, with when each was first and last asked for.
/// Each waits [`EngineOptions::walk_debounce`] for its neighbours; none is dropped.
#[derive(Default)]
struct PendingWalks {
    /// Path, first asked, last asked. A `Vec` and a linear scan: the earliest
    /// deadline needs a full pass anyway, and a burst reduces as it arrives.
    at: Vec<(String, Instant, Instant)>,
}

impl PendingWalks {
    /// Note that `path` wants walking, merging it with what is already waiting.
    fn add(&mut self, path: &str, now: Instant) {
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
    fn next_due(&self, debounce: Duration, cap: Duration) -> Option<Instant> {
        self.at
            .iter()
            .map(|(p, f, l)| Self::due_at(p, *f, *l, debounce, cap))
            .min()
    }

    /// Take the walks that have waited long enough, reduced. Whatever a taken path
    /// covers goes with it, or the same walk runs again a moment later.
    fn take_due(&mut self, now: Instant, debounce: Duration, cap: Duration) -> Vec<String> {
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
fn scan(shared: &Arc<Shared>, pulses: &mut Pulses, source: usize, subtree: Option<String>) -> bool {
    let Some(src) = shared.sources.get(source).cloned() else {
        return true;
    };
    let began = Instant::now();
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

    let mut sink = ToIndex {
        index: Arc::clone(&shared.index),
        seen: 0,
        buffer: Vec::with_capacity(BATCH),
        stop: Arc::clone(shared),
        failed: false,
    };
    // Read once, here: a walk skips by one set of rules from beginning to end, and
    // a rule saved halfway through takes effect on the scan that follows.
    let opts = ScanOptions {
        subtree: subtree.clone(),
        ..(*shared.scan()).clone()
    };
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
    if trustworthy {
        // One call for every root the walk vouched for: the pass's notes of unchanged
        // rows are consumed by the first call, leaving later roots reconciled to nothing.
        let gone = match shared.index.sweep(src.id(), &vouched, generation, &spare) {
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
        pulses.scanned(source, began.elapsed());
    }

    let finished = report
        .as_ref()
        .is_ok_and(|r| !r.cancelled && r.unreadable == 0 && r.blind.is_empty());
    let covered = scour_core::PrefixSet::new(vouched);
    let all_roots = src.describe().roots.iter().all(|root| covered.covers(root));

    shared.scanning.store(false, Ordering::Relaxed);
    let mut st = shared.status.write();
    st.scanning_source = None;
    st.scanned = seen;
    st.last_scan_ms = report.map(|r| r.took_ms).unwrap_or(0);
    // A missing subtree alone is not a missing source. Any incomplete pass or
    // failed write needs a retry of the source to recover what it could not see.
    !sink.failed && ended && finished && (subtree.is_some() || (could_look && all_roots))
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
fn schedule_retry(retries: &mut Vec<(usize, Instant, usize)>, pulses: &Pulses, source: usize) {
    if !retries.iter().any(|(s, _, _)| *s == source) {
        retries.push((source, pulses.retry_at(source, retry_delay(0)), 0));
    }
}

/// The floor on how often segments are merged without being asked. Not a cost of
/// merging but of deciding: `stats()` reads every segment's live count.
const COMPACT_EVERY: Duration = Duration::from_secs(60);

/// The shortest the worker will ever sleep: a backstop, so a deadline computed
/// wrong costs fifty turns a second rather than a spun core.
const WAKE_FLOOR: Duration = Duration::from_millis(20);

/// The batch that stands in for `commit_batch` when nobody is watching: with
/// nothing open there is no latency to protect, so this is a memory ceiling.
const IDLE_BATCH: u64 = 4_096;

const BATCH: usize = 4_096;

/// Turns a walk into index updates, in batches.
struct ToIndex {
    index: Arc<dyn Index>,
    seen: u64,
    buffer: Vec<Change>,
    stop: Arc<Shared>,
    /// A batch the index refused. The sweep removes everything the walk did not
    /// stamp, so one failed write would otherwise turn a scan into a deletion.
    failed: bool,
}

impl ToIndex {
    fn flush(&mut self) {
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
            self.stop.status.write().scanned = self.seen;
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
    use super::{PendingWalks, coalesce};
    use std::time::{Duration, Instant};

    fn c(v: &[&str]) -> Vec<String> {
        coalesce(v.iter().map(|s| (*s).to_owned()).collect())
    }

    const DEBOUNCE: Duration = Duration::from_millis(500);
    const CAP: Duration = Duration::from_secs(3);

    /// The clock, without one: every instant here is relative to the first request,
    /// and sleeping through them would take twelve seconds to assert a subtraction.
    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    fn due(w: &mut PendingWalks, base: Instant, ms: u64) -> Vec<String> {
        w.take_due(at(base, ms), DEBOUNCE, CAP)
    }

    #[test]
    fn a_walk_of_a_parent_absorbs_every_walk_below_it() {
        // A `git clone` asks for a walk per package directory, and a walk flushes a
        // segment, takes a generation and sweeps.
        assert_eq!(c(&["/a", "/a/b", "/a/b/c", "/a/d"]), vec!["/a"]);
        assert_eq!(c(&["/a/b/c", "/a/b", "/a"]), vec!["/a"]);
    }

    #[test]
    fn siblings_are_not_absorbed_and_neither_is_a_longer_name() {
        assert_eq!(c(&["/a/b", "/a/c"]), vec!["/a/b", "/a/c"]);
        // A sibling whose name starts with the prefix is not inside it.
        assert_eq!(c(&["/a/b", "/a/bc"]), vec!["/a/b", "/a/bc"]);
    }

    #[test]
    fn the_same_walk_asked_for_twice_is_one_walk() {
        assert_eq!(c(&["/a/b", "/a/b", "/a/b"]), vec!["/a/b"]);
    }

    #[test]
    fn an_empty_path_means_everything_and_subsumes_the_rest() {
        // "I lost track and cannot say where": walking everything covers the rest.
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

    #[test]
    fn nothing_is_walked_while_the_requests_are_still_arriving() {
        // A request every few hundred milliseconds is its own channel batch, so
        // `coalesce` never sees two together — this is what does.
        let t = Instant::now();
        let mut w = PendingWalks::default();
        for ms in [0, 100, 200, 300, 400] {
            w.add("/a/pkg", at(t, ms));
            assert!(
                due(&mut w, t, ms).is_empty(),
                "walked at {ms} ms, while the cluster was still arriving"
            );
        }
        assert!(due(&mut w, t, 800).is_empty(), "400 ms of quiet is not 500");
        assert_eq!(due(&mut w, t, 900), vec!["/a/pkg"], "one walk for five");
        assert!(
            due(&mut w, t, 5_000).is_empty(),
            "and it is not walked twice"
        );
    }

    #[test]
    fn a_trickle_is_walked_at_the_cap_however_long_it_goes_on() {
        // Without the cap the debounce is a cancellation, not a delay: a directory
        // written every 100 ms for a minute keeps the subtree out for the minute.
        let t = Instant::now();
        let mut w = PendingWalks::default();
        let mut walked_at = None;
        for ms in (0..6_000).step_by(100) {
            w.add("/a/pkg", at(t, ms));
            if !due(&mut w, t, ms).is_empty() && walked_at.is_none() {
                walked_at = Some(ms);
            }
        }
        assert_eq!(
            walked_at,
            Some(CAP.as_millis() as u64),
            "a request under a trickle must walk at the cap and not later"
        );
    }

    #[test]
    fn a_request_absorbed_by_a_parent_keeps_the_older_deadline() {
        // A parent that swallows a held child must not take the new first-asked:
        // that restarts the cap, and the debounce postpones without bound.
        let t = Instant::now();
        let mut w = PendingWalks::default();
        for ms in (0..2_900).step_by(100) {
            w.add("/a/pkg/x", at(t, ms));
        }
        w.add("/a/pkg", at(t, 2_900));
        assert!(due(&mut w, t, 2_950).is_empty());
        assert_eq!(
            due(&mut w, t, 3_000),
            vec!["/a/pkg"],
            "the parent inherited the child's cap and walked at it"
        );
    }

    #[test]
    fn a_walk_that_runs_takes_the_requests_it_covers_with_it() {
        // Walking `/a/pkg` is the walk `/a/pkg/x` asked for, so leaving it behind
        // runs the same walk twice; `/a/other` is covered by neither and survives.
        let t = Instant::now();
        let mut w = PendingWalks::default();
        w.add("/a/pkg", at(t, 0));
        w.add("/a/other", at(t, 400));
        // Late enough to be swallowed, not to hold the parent back.
        w.at.push(("/a/pkg/x".to_owned(), at(t, 480), at(t, 480)));
        assert_eq!(due(&mut w, t, 500), vec!["/a/pkg"]);
        assert!(
            due(&mut w, t, 800).is_empty(),
            "the sibling is covered by nothing and is not due yet"
        );
        assert_eq!(
            due(&mut w, t, 900),
            vec!["/a/other"],
            "and `/a/pkg/x` went with the walk that covered it"
        );
    }

    #[test]
    fn siblings_both_walk_and_neither_is_swallowed() {
        let t = Instant::now();
        let mut w = PendingWalks::default();
        w.add("/a/one", at(t, 0));
        w.add("/a/two", at(t, 10));
        w.add("/a/one/deep", at(t, 20));
        let mut walked = due(&mut w, t, 600);
        walked.sort();
        assert_eq!(walked, vec!["/a/one".to_owned(), "/a/two".to_owned()]);
    }

    #[test]
    fn a_lost_watcher_does_not_wait_and_covers_everything_waiting() {
        // An empty path is "the index is drifting"; holding it holds the only
        // message that stops the drift.
        let t = Instant::now();
        let mut w = PendingWalks::default();
        w.add("/a/pkg", at(t, 0));
        w.add("", at(t, 10));
        assert_eq!(
            due(&mut w, t, 10),
            vec![String::new()],
            "it walks the moment it arrives"
        );
        assert!(
            due(&mut w, t, 5_000).is_empty(),
            "and walking everything answered the subtree that was waiting"
        );
    }

    #[test]
    fn a_trailing_slash_is_the_same_pending_walk() {
        let t = Instant::now();
        let mut w = PendingWalks::default();
        w.add("/a/pkg/", at(t, 0));
        w.add("/a/pkg", at(t, 100));
        assert_eq!(due(&mut w, t, 700), vec!["/a/pkg"]);
    }
}
