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
    /// What the walk and the watchers skip, to begin with.
    ///
    /// **Taken by [`Engine::new`] and not read from here again.** These are the
    /// only options a person edits while the service runs — a rule typed into a
    /// window has to mean something before the next restart — so the engine
    /// keeps them behind a lock and [`Engine::set_scan_options`] replaces them.
    /// Left in this struct it would be a second copy that stopped being true
    /// the first time somebody saved a rule.
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
    /// The batching bound once a change has reached the worker. Detection,
    /// queued scans and failed writes can add delay; recovery intervals cover
    /// changes that a source did not report.
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
    /// Full reconciliation when neither a watch nor a pulse is available.
    pub poll_interval: Duration,
    /// Safety pass even when a source appears quiet. Pulses are only hints.
    pub reconcile_interval: Duration,
    /// How long a [`Change::Rescan`] waits for its neighbours before it walks.
    ///
    /// **Because a walk is the expensive kind of change and they arrive in
    /// clusters.** Every fresh directory a watcher sees asks for one, and a
    /// walk ends in a commit — a segment and a manifest fsync — however little
    /// it found. Measured live over twelve minutes with nothing but a build
    /// running: **157 subtree walks, 13.3 a minute, 0–1 ms each, 0–81,920
    /// entries, and every one of them committing.** Thirty-two of those
    /// segments held 48,829 bytes between them and cost the worker 39.4 MB of
    /// block writes.
    ///
    /// [`coalesce`] already merges the walks that happen to be in one channel
    /// batch, and that is the whole of what it can do: a `cargo build` writing
    /// a directory every few hundred milliseconds never puts two in the same
    /// batch. Waiting a moment for the next one is what turns a burst into a
    /// walk of the parent.
    ///
    /// [`Change::Rescan`]: scour_core::Change::Rescan
    pub walk_debounce: Duration,
    /// The ceiling on that wait, however long the cluster keeps arriving.
    ///
    /// A trickle that never stops would otherwise never settle, and a walk
    /// that never runs is a subtree the index does not have. This is the whole
    /// of the latency the debounce can cost: a file created inside a directory
    /// that did not exist a moment ago is findable this much later than it was,
    /// and no later.
    pub walk_debounce_cap: Duration,
    /// How often [`Engine::await_change`] may wake the clients waiting in it.
    ///
    /// **[`Status::revision`] is exact and this is not a rate limit on it** —
    /// it is a rate limit on the push. A client that asks gets the current
    /// number and the current counts, always; what this bounds is how often
    /// one that is asleep is woken to ask.
    ///
    /// Because the revision moves faster than the index can be written and
    /// every bump costs a client a full re-query. Measured: revision 5,259 →
    /// 5,477 in 62 s and 5,583 → 5,757 in 28 s — 3.5 to 6.2 a second, while
    /// commits are at most one a second — the excess being one bump per
    /// subtree walk and one per removal batch. Against **20.5–21 ms of service
    /// CPU per revision** for a single attached window (447 ticks over 218
    /// revisions, 364 over 174), that is 7–13% of a core spent telling one
    /// idle page what it already had.
    ///
    /// Deliberately the same number as [`EngineOptions::commit_watched`] and
    /// deliberately not that field: what a waiter is told about arrives in a
    /// commit, so waking faster than the commit clock cannot show anything
    /// new — but `commit_watched` is a decision about *writing* and this is a
    /// decision about *waking*, and one must be changeable without the other.
    ///
    /// Zero turns it off: every bump wakes every waiter, as it did before.
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
            // The burst clock, exactly. Below it nothing would happen sooner —
            // `commit_interval` is a floor on how often a segment is written at
            // all — so anything smaller would be a number that reads faster
            // than it behaves.
            commit_watched: Duration::from_millis(1_000),
            compact_segments: 8,
            compact_urgent: 64,
            idle_after: Duration::from_secs(20),
            poll_interval: Duration::from_secs(60),
            reconcile_interval: Duration::from_secs(1_800),
            // Long enough to catch the cluster, short enough that nobody
            // counts it out loud. The measured arrival rate is 13.3 walks a
            // minute in bursts — 22 in five seconds and 58 in seven were both
            // recorded — so half a second of quiet is a real gap and not a
            // hopeful one.
            walk_debounce: Duration::from_millis(500),
            // Three seconds is the worst a file inside a brand-new directory
            // can wait, and it is deliberately smaller than `commit_idle`:
            // the walk that finds it is not the slowest step on its way to
            // being findable, and this must not become the slowest one.
            walk_debounce_cap: Duration::from_secs(3),
            // `commit_watched`, exactly, and for the reason given on the
            // field: a waiter cannot be shown anything the index has not
            // written, and it is not written more often than that.
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
    /// What the walk and the watchers skip.
    ///
    /// **Apart from the rest of [`EngineOptions`], and behind a lock, because
    /// this is the one part of them a person edits while the service runs.**
    /// It used to be read once at start-up with everything else, which was
    /// true for as long as the only way to write a rule was a file the service
    /// reads on start. A window can write one now, and a rule that needs a
    /// restart to mean anything is a control that lies: the panel says the
    /// change takes effect on the next scan, and it has to.
    ///
    /// An `Arc` inside the lock so a scan can take its own copy and let go —
    /// a walk holds these for as long as it runs, and a rule saved during one
    /// must not block on it.
    scan: RwLock<Arc<ScanOptions>>,
    status: RwLock<Status>,
    pending: AtomicU64,
    scanning: AtomicBool,
    /// Per source: a whole-source walk is already waiting to run.
    ///
    /// Cleared as the walk begins rather than when it ends, so a rule saved
    /// while one is running still queues one behind it — that walk has
    /// something the running one does not.
    queued: Vec<AtomicBool>,
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
    /// When they were last woken, and whether a bump has been held since.
    ///
    /// See [`EngineOptions::await_hold`]. The flag is what the worker loop
    /// reads to know it owes a wake-up: a bump held back is not a bump
    /// dropped, and the deadline that delivers it is the one thing that makes
    /// the difference.
    wake_at: Mutex<Instant>,
    wake_owed: AtomicBool,
    /// The ordered hits of whichever query was asked for last.
    prepared: RwLock<Option<Prepared>>,
    /// When one was last asked for, so misses cannot queue one each.
    prepared_at: Mutex<Instant>,
    /// How long that one has to stand before another may be asked for, in
    /// microseconds. Written by the preparing thread out of what its last walk
    /// cost — see [`PREPARE_COST`] — and read by [`Engine::search`]. Atomic
    /// rather than behind `prepared_at`'s lock because the reader is on the
    /// path every search takes and the writer runs once a walk.
    prepare_floor: AtomicU64,
    /// Asks the preparing thread for a query's full ordered page. Bounded and
    /// tiny: only the newest request matters, and an older one still in the
    /// channel is work nobody wants done.
    prepare: Sender<Prepare>,
    /// How many of them there are.
    ///
    /// Read by the commit clock, and that is the whole reason it is counted: a
    /// change is worth writing sooner when something is waiting to be told
    /// about it. Zero means nobody is looking and the batching stands.
    watchers: AtomicU32,
}

/// What the preparing thread is asked to have ready.
struct Prepare {
    query: String,
    sort: SortKey,
    descending: bool,
    count_cap: u32,
}

/// One query's ordered hits, kept so that paging through them is free.
///
/// **Why this exists.** The index answers a page by asking every segment for
/// `offset + limit` hits, merging them, sorting the lot and throwing the first
/// `offset` away. That is honest and it is linear in how deep the page is:
/// measured on 2.1 M entries, the first window of a broad query costs 16 ms
/// and the window at row nineteen thousand costs 114 — for the same two
/// hundred rows. A list being scrolled asks for one of those per window
/// crossed, so scrolling got slower the further it went, which is exactly how
/// it felt.
///
/// The order does not change while the index does not, so it is computed once.
/// A window is then a slice, and the cost of a page stops depending on where
/// the page is.
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

/// How many ordered hits are kept hot.
///
/// The window can reach twenty thousand rows and no further — beyond that a
/// query is too broad to page through and wants narrowing instead — so this is
/// the whole of what any page can ask for. At roughly a hundred bytes a hit it
/// is a couple of megabytes for the query being looked at, and there is only
/// ever one.
const PREPARE: u32 = 20_000;

/// How often an ordering may be built, at the very least.
///
/// A floor and not the whole rule. It used to be the whole rule, justified by
/// "the walk costs about a tenth of a second on a broad query, so once every
/// two seconds is at most a twentieth of a core spent on speculation" — which
/// is true of the *stored* order and of nothing else. This file's own
/// measurements of one window of two hundred rows span 3.2 ms sorted by
/// modification time and 2,463.6 ms sorted by path, and a walk of twenty
/// thousand is dearer again. At the top of that range a flat two seconds is
/// not a twentieth of a core, it is most of one.
///
/// It is also work that is thrown away. `prepare_loop` drops the result if the
/// index moved while it was walking, and a window being looked at holds
/// `watchers > 0`, which puts the commit clock on
/// [`EngineOptions::commit_watched`] — about a second. So a walk that takes
/// longer than that can essentially never survive, and repeating it every two
/// seconds is a core spent producing nothing.
///
/// See [`PREPARE_COST`] for what is charged instead.
const PREPARE_EVERY: Duration = Duration::from_secs(2);

/// Cheap pages already meet the interactive budget. Building a much larger
/// window for them spends memory and competes with the next keystroke.
const PREPARE_MIN_COST: Duration = Duration::from_millis(20);

/// What a walk buys the next one: it waits at least this many times what the
/// last one took.
///
/// The page solved the same problem for itself and the shape is taken from it
/// — `atMostEvery` in `page.html` charges `floor = max(ms, spent * COST)` with
/// `COST = 10`, after a three-second count refresh on a broad query was
/// measured at 930 ms of the service each time, "a third of a core, spent on a
/// number nobody was reading". The same number here for the same reason: a
/// speculative walk can then never take more than about a tenth of a machine,
/// however dear it is, and a cheap one is unaffected because the floor above
/// still applies.
///
/// Deliberately without a ceiling. A walk that costs 2.4 s backs off to
/// twenty-four, and that is the right answer rather than a regrettable one:
/// at that price the ordering is discarded before it lands every single time,
/// so nothing is lost by not building it, and the deep page it would have made
/// cheap costs the same 2.4 s either way.
const PREPARE_COST: u32 = 10;

/// How many of the biggest files are considered for duplication.
///
/// A ceiling rather than a target, and the measurement says it is a generous
/// one: on 1,474,650 files and 493.6 GB, everything over a megabyte is 18,723
/// files. Past that the list is dominated by build output and small files that
/// collide on size trivially — 59.3% of files are 4 KB or smaller — and the
/// bytes they could give back are a rounding error against the 141.8 GB above
/// the knee.
const CANDIDATES: u32 = 200_000;

/// How much CSV goes into one frame of an export.
///
/// **A row a frame would make the framing most of the bytes.** A frame is a
/// line of JSON — `{"id":7,"more":true,"ok":{"result":"export_chunk","csv":…}}`
/// — so a 60-byte row would pay 60 bytes of envelope, a `serde_json` call and
/// a socket write for itself. At 2.24 M rows that is 2.24 M of each.
///
/// A whole export in one frame is the other end and is what this exists to
/// avoid: it is the 200-odd MB nobody may hold.
///
/// 128 KB is roughly two thousand rows of this index, which is one write and
/// one JSON escape per two thousand rows, and a transient allocation of about
/// twice that while the frame is built. It is also comfortably under the
/// megabyte the server will read back on the request side — not that a reply
/// is subject to that ceiling, but a frame nobody could have sent in the other
/// direction is a frame worth being suspicious of.
const EXPORT_CHUNK: usize = 128 * 1024;

/// The parts of a query the parser could not read as written.
///
/// The service does this rather than the caller, and that is the whole point:
/// it is the one that read the query, so it is the one that can say how. A
/// client parsing the text a second time to find out would be a second opinion
/// about the same string, and the day the two disagree is the day the warning
/// is worse than nothing.
///
/// Measured at 0.5 µs for a query with five terms and 0.17 µs for two — cheaper
/// than the parse that precedes it, against 160 µs for the fastest search there
/// is. `scour-query/examples/spancost.rs`.
fn misread(query: &str) -> Vec<scour_core::Span> {
    scour_query::spans(query)
        .into_iter()
        .filter(|s| s.role.is_warning())
        .collect()
}

impl Shared {
    /// What the walk skips, right now.
    ///
    /// A pointer copy taken under the read lock, so a caller that walks for a
    /// minute is not holding anything a saved rule has to wait behind.
    fn scan(&self) -> Arc<ScanOptions> {
        Arc::clone(&self.scan.read())
    }

    /// A search run again could now answer differently.
    ///
    /// Bumped under the waiters' lock, so a client that has read the revision
    /// and not yet gone to sleep on it is not overtaken.
    ///
    /// **The number moves now; the wake-up may not.** See
    /// [`EngineOptions::await_hold`] — the revision is what anyone who asks is
    /// told, and it stays exact, but a client that is *asleep* on it is woken
    /// at most once per hold.
    fn touched(&self) {
        {
            let _held = self.waiters.0.lock();
            self.revision.fetch_add(1, Ordering::Release);
        }
        self.announce(Instant::now());
    }

    /// The same, for a change somebody asked for by name.
    ///
    /// An emptied trash and an explicit flush are answers to a command that
    /// was just typed, not churn from a watcher, and the window that sent the
    /// command is the one waiting to see it. They are also bounded by how fast
    /// a person can ask, which is what the hold exists to bound. So they skip
    /// it — and reset it, so the next held bump measures its second from here.
    fn touched_now(&self) {
        {
            let _held = self.waiters.0.lock();
            self.revision.fetch_add(1, Ordering::Release);
        }
        *self.wake_at.lock() = Instant::now();
        self.wake_owed.store(false, Ordering::Relaxed);
        self.waiters.1.notify_all();
    }

    /// Wake the waiters, unless one was woken less than a hold ago.
    ///
    /// Returns whether it did. A bump that is held back sets `wake_owed`, and
    /// the worker loop carries a deadline for exactly that: the announcement
    /// is late, never missing. Announcing from *there* rather than from a
    /// timer in the request thread is what keeps `watchers` above zero — a
    /// waiter that went away to sleep would put the commit clock back on
    /// `commit_idle`, and fifteen seconds is not a delay a live list survives.
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
        // Moved out of `opts` rather than copied from it, so there is one
        // answer to "what does the walk skip" and not two that can drift.
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
            // A hold ago, so the first change of the session is announced the
            // moment it happens rather than a second after start-up.
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
        // Bounded: a burst of filesystem events must slow the watcher down
        // rather than accumulate without limit in memory.
        let (changes_tx, changes_rx) = crossbeam_channel::bounded::<Change>(65_536);
        let worker = {
            let shared = Arc::clone(&shared);
            std::thread::Builder::new()
                .name("scour-worker".into())
                .spawn(move || run(shared, jobs_rx, changes_rx))
                .ok()
        };
        // Its own thread rather than the worker's: the worker is where scans
        // and commits happen, and a page has to be ready while one is running,
        // not after it.
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

    /// Start watching every source that can be watched.
    ///
    /// Sources that cannot are not an error: a cloud bucket has no change feed
    /// and is reconciled by rescanning instead. `Caps` says which is which, so
    /// nothing here has to know what it is talking to.
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

    /// Watch everything, then walk it — **and the order is the whole point.**
    ///
    /// A walk is a snapshot; a watch is everything after it. Walking first
    /// leaves the window between them covered by neither, and whatever is
    /// created in it is reported by nothing and found by nothing until the next
    /// full walk. The same race was found and fixed once for subtrees a watcher
    /// discovers — see `a_subtree_is_watched_before_it_is_walked` — and was
    /// still here afterwards on the biggest walk of all: 5,000 files written
    /// into 200 fresh directories left 1,260 of them missing, in one unbroken
    /// run rather than scattered, which is the window in which the shell loop
    /// writing them was fastest.
    ///
    /// **`rescan` only queues**, and that is exactly why the order has to be
    /// written down rather than trusted: watching a home directory installs
    /// 342,000 watches one at a time and takes fifteen seconds, so a walk
    /// queued first is picked up by the worker while most of the tree is still
    /// uncovered. It lived in `scourd`'s start-up as two calls with a comment
    /// between them, which is not something a test can hold on to.
    ///
    /// Returns what watching reported, so the caller can say so. Reported
    /// *after* the walk is queued rather than between the two, and that costs
    /// nothing: queueing is all `rescan` does.
    pub fn cover_then_walk(&self, walk: bool) -> Result<(u32, Vec<String>)> {
        let started = self.start_watching()?;
        let skipped = self.unwatched();
        if walk {
            self.rescan(None)?;
        }
        Ok((started, skipped))
    }

    /// Queue a full walk of every source, or of one subtree.
    ///
    /// **A whole-source walk that is already queued is not queued again.** The
    /// job channel is unbounded and nothing downstream collapses these, so
    /// before this the panel's switches were a way to stack walks of two
    /// volumes one per click — five taps, five walks, each of them by then
    /// answering a question the one before it had already answered. The flag
    /// clears when the walk starts, so a change made *during* a walk still gets
    /// its own: that one has something new to find.
    ///
    /// Subtree walks are left alone. They are cheap, they are usually about
    /// different subtrees, and the worker already folds overlapping ones.
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

    /// Look at these paths again, now, rather than when a watcher gets to them.
    ///
    /// **What this exists for.** A person who deletes a file from inside Scour
    /// watches the row it was on. The watcher will notice — in three to eight
    /// seconds on this machine, most of that the index's own write interval —
    /// and for a change nobody is waiting on that is the right price. For this
    /// one it is not: a row that sits there for six seconds after being sent to
    /// the trash reads as a deletion that did not work, and the second press is
    /// on a file that is already gone.
    ///
    /// It is deliberately **not** a way to change anything. Whoever moved the
    /// file did the moving, with their own permissions; this only re-reads. The
    /// service never gained the ability to delete, which for something that
    /// runs in the background and has at one point been handed `CAP_SYS_ADMIN`
    /// is worth keeping true.
    ///
    /// Paths outside every source are skipped rather than refused: a selection
    /// can span a source boundary, and one path that is nobody's is not a
    /// reason to leave the other eleven stale.
    ///
    /// Returns how many were looked at.
    pub fn recheck(&self, paths: &[String]) -> Result<usize> {
        let sink = Forward(self.changes.clone());
        let mut done = 0;
        // Grouped by owner, because `recheck` is a source's method and a
        // selection of twelve rows is usually one source's twelve rows.
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
        // Flush is quick and callers want its result; the heavy levels go to
        // the worker so a request never blocks for minutes.
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

    /// What the walk and the watchers are skipping, right now.
    ///
    /// The rules in force, merged: the built-in set, the configuration's own,
    /// and whatever a window has added, all in the one shape the engine works
    /// in. A caller that has to show them *apart* — a panel with a switch
    /// beside each one does — knows where each group came from and does not
    /// ask here; this is for anyone who wants the single true answer to "what
    /// is being skipped".
    ///
    /// A snapshot, not a view: [`Engine::set_scan_options`] can replace them
    /// between one call and the next.
    pub fn scan_options(&self) -> Arc<ScanOptions> {
        self.shared.scan()
    }

    /// Skip by these from now on, and tell the watchers.
    ///
    /// **The rules are the one part of the engine's configuration a person
    /// edits while it runs**, and everything downstream of that has to be told
    /// rather than restarted: a walk started after this uses the new set, and
    /// every live watcher re-tunes to it. Without the second half a rule would
    /// sweep a tree clean and the watcher would put it straight back, which is
    /// the failure the rule was added to prevent.
    ///
    /// It does not scan. Deciding *when* the index should be brought in line
    /// with a new rule is the caller's, because only the caller knows whether
    /// a person just asked for this or a file changed on disk.
    pub fn set_scan_options(&self, opts: ScanOptions) {
        let fresh = Arc::new(opts);
        *self.shared.scan.write() = Arc::clone(&fresh);
        // Under the same lock the worker takes to cover a subtree, so a retune
        // and a cover cannot be inside one handle at once.
        for (_, handle) in self.shared.watches.lock().iter() {
            handle.retune(&fresh);
        }
    }

    /// Throw out everything the rules in force would now skip. Returns how many
    /// subtrees were dropped.
    ///
    /// **A new rule does not need a walk, and paying for one is the whole point
    /// of this.** Excluding something can only ever *remove* entries, and every
    /// path the answer is about is already in the index — so this is a pass
    /// over rows that are in memory, asking each source's own test, against a
    /// walk of two volumes that would go to the disk to learn nothing new. Only
    /// the opposite change — a rule taken away — needs a walk, because the
    /// entries it re-admits were never indexed and cannot be recovered from
    /// something that does not hold them.
    ///
    /// **Subtrees, not rows.** [`Change::RemoveSubtree`] takes a directory and
    /// everything beneath it in one step — measured at 1.3 µs for 378,100
    /// documents — so a directory that is now skipped costs one change rather
    /// than one per file inside it. Whatever is below it is still walked here
    /// and still tested, which is cheap and keeps this honest for the case the
    /// shortcut does not cover: a `file:` rule matching something inside a
    /// directory that stays.
    pub fn apply_rules(&self) -> Result<u64> {
        let opts = self.shared.scan();
        // One test per source, built once. A source that does not do exclusions
        // says so by returning `None`, and its rows are left alone rather than
        // being deleted on the strength of a test that answers `false` to
        // everything.
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
                // No `..Default::default()`: `ScanRequest` has one field
                // today, and a second one appearing should stop this line
                // compiling rather than silently take a default. What a sweep
                // walks is not somewhere to inherit a value nobody chose.
                query: scour_query::parse(""),
            },
            &mut |hit: &Hit| {
                // The index does not carry which source a row came from in a
                // `Hit`, and asking every test is both correct and cheap: a
                // path outside a source's roots is refused by that source's
                // rules on the root check, before any rule is looked at.
                // `is_dir` from the row, because a `dir:` rule is about
                // directories and a file that shares the name is not one.
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

        // **Folded to the topmost path of each excluded tree.** Every row under
        // an excluded directory matches the rule too, so without this a
        // directory of ten thousand files is ten thousand changes that each
        // remove a subtree of something already removed. `coalesce` is the same
        // one the watcher's rescans go through, and its tests are the reason it
        // is not written twice.
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

    /// One page of one query.
    ///
    /// Served from [`Prepared`] when the same query's order is already known,
    /// which is what makes a window at row nineteen thousand cost the same as
    /// the one at row zero. Otherwise the index answers it, and the ordering is
    /// asked for in the background so that the next window does not have to
    /// wait for the same walk twice.
    pub fn search(
        &self,
        query: &str,
        sort: SortKey,
        descending: bool,
        page: Page,
    ) -> Result<SearchResponse> {
        let started = Instant::now();
        let mut res = self.page_of(query, sort, descending, page)?;
        // Every answer, on every path, including the cached one — a warning
        // that appears on a cache miss and vanishes on a hit is worse than no
        // warning, because it teaches the reader that its absence means
        // something.
        res.misread = misread(query);
        self.weigh_folders(&mut res);
        res.took_us = started.elapsed().as_micros() as u64;
        Ok(res)
    }

    /// Fill in what each folder on this page holds.
    ///
    /// **Here rather than in the index's own search**, and that is the point:
    /// the index answers about rows and this is a question about subtrees, so
    /// keeping it out of the row loop means the search path is untouched and
    /// the cost is one batched call somebody can see.
    ///
    /// Nothing at all when the page has no folders on it, which is most pages.
    /// A failure is left as `None` — a folder with no number reads as a folder
    /// whose size is not known, which is true, where a zero would read as an
    /// empty one.
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
        // Not ready, or ready for something else. Ask for it while this page is
        // answered the long way; a full slot means a newer query is already
        // waiting, and that one is worth more than this one.
        //
        // **But not on every miss.** An ordering is thrown away whenever the
        // index moves, and a machine with a watcher on it moves the index
        // about once a second — so a window left open on a broad query had the
        // preparing thread walking twenty thousand hits over and over,
        // measured at **28% of a core with nobody touching anything**. The
        // cache is worth having when a list is being scrolled and worth
        // nothing when it is rebuilt faster than it is read, so it is rebuilt
        // at most this often and the ordinary path answers in between.
        // **Only for a list somebody is paging through.** An ordering exists to
        // make the *deep* windows cheap — 5.5 ms against 114 at row nineteen
        // thousand — and a window sitting at the top of its results never asks
        // for one. Preparing anyway is speculation nobody redeems, and while a
        // scan is running it is speculation thrown away before it lands: the
        // index moves, the ordering goes with it, and the thread starts over.
        // Measured at 27% of a core in exactly that state.
        // **And not until the last walk has been paid for.** The floor is what
        // that walk cost times `PREPARE_COST`, never less than `PREPARE_EVERY`
        // — so an ordering that is cheap to build stays as live as it was, and
        // one that is dear is built at a bounded share of the machine instead
        // of a fixed interval that knows nothing about the price.
        // A page beyond the preparation's bound cannot redeem it either.
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

    /// The page, if the order it belongs to is already known.
    ///
    /// Refuses on anything it cannot answer exactly: a different query, a
    /// different order, a different count cap, an index that has moved since,
    /// or a page reaching past what was prepared. A cache that guesses is worse
    /// than none.
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
        // Short of the ceiling means the walk reached the end of the matching
        // set, so an offset past it is genuinely empty rather than unknown.
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
            // Nothing was walked to answer this, which is the point of it.
            fast_path: true,
            rows_visited: 0,
            rows_built: 0,
            // Stamped by `search` for every path alike.
            misread: Vec::new(),
        })
    }

    /// The whole matching set, as CSV, in pieces.
    ///
    /// **The service makes the file, not the frontend.** It was the browser
    /// bridge's, in JavaScript's neighbourhood if not in JavaScript, and the
    /// owner's instruction was exactly this: the Rust service gives the CSV.
    /// What that buys is one implementation of the quoting instead of one per
    /// frontend — the terminal had no export at all and now reaches the same
    /// code — and it is the only arrangement in which the walk and the writing
    /// happen in the same place, which is what makes a stream possible.
    ///
    /// `out` is handed each piece and returns false to stop. Stopping is
    /// ordinary — a cancelled download — and it propagates all the way into
    /// the index's walk, which abandons it. Nothing is held: not the rows, not
    /// the file, not a buffer that grows with the answer.
    ///
    /// Returns how many rows were written.
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

        // Set when `out` refuses, because the walk below can only be stopped
        // by returning false and the reason has to survive back to here — a
        // caller that went away is not the same as a query that ran out of
        // rows, and the count alone cannot tell them apart.
        let mut stopped = false;
        let mut flush = |buf: &mut Vec<u8>, stopped: &mut bool| {
            if buf.is_empty() {
                return true;
            }
            // Rows are built from `String` paths, so this is UTF-8 by
            // construction; the lossy conversion is a refusal to panic on the
            // day that stops being true, not an expectation that it will.
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
            // The tail, and the header when the query matched nothing at all.
            // An empty result still produces a file with its heading row: a
            // spreadsheet with no rows says "nothing matched", and a zero-byte
            // download says the export broke.
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

    /// What a subtree weighs — all of it, or only the part a query names.
    ///
    /// Nearly straight through to the index: this is an aggregation over a
    /// layout, and the engine has nothing to add to it but the reading of the
    /// query. That reading belongs here for the same reason [`Engine::facets`]
    /// does it — the parser answers to one layer, so a frontend sends the text
    /// somebody typed and never a syntax tree it assembled itself.
    ///
    /// An empty query is the `du` question, answered exactly as before.
    pub fn usage(&self, path: &str, top: u32, query: &str) -> Result<scour_core::UsageResponse> {
        self.shared.index.usage(&scour_core::UsageRequest {
            path: path.to_owned(),
            top,
            query: scour_query::parse(query),
        })
    }

    /// The same file, several times over.
    ///
    /// **This needed nothing new from the index**, which is worth saying
    /// because the plan in `docs/REPORTS.md` expected a `duplicates()` on the
    /// trait and a digest column behind it. Neither is here. The candidates
    /// are a search — `size:>N`, ordered by size — and everything after that
    /// is `scour-dupes`, which takes paths and sizes and knows nothing about
    /// an index. The digest column stays unbuilt until somebody wants
    /// duplicates *below* the size where reading is affordable.
    ///
    /// The query is built here rather than taken from the caller, because a
    /// caller that could pass one could ask for duplicates among directories,
    /// and a directory has no bytes to compare.
    ///
    /// `index.search` directly, not [`Engine::search`]: that one clamps the
    /// page to `result_limit`, which exists so a window cannot ask for a
    /// million rows behind a keystroke. Nineteen thousand paths is the whole
    /// candidate set on the measured corpus and costs about twenty
    /// milliseconds to build.
    pub fn duplicates(
        &self,
        under: &str,
        opts: &scour_dupes::Options,
    ) -> Result<scour_dupes::Report> {
        let mut query = format!("file: size:>={}", opts.min_size);
        if !under.is_empty() {
            // Quoted, because a path can hold a space and an unquoted one
            // would become two terms — which would silently widen the scope
            // rather than fail, and a report about the wrong folder looks
            // exactly like a report about the right one.
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
        // Cancel an active walk before joining producers. Never hold the watch
        // lock across a join: the consumer also takes it while draining events.
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
        // A full channel means the worker is behind. Blocking here is the
        // point: it slows the watcher instead of growing a queue that would
        // eventually be the whole filesystem.
        let _ = self.0.send(change);
    }
}

/// The background thread: one loop for jobs, changes and the commit clock.
/// Keeps the ordering of whatever query was asked for last.
///
/// **Only the newest.** The channel holds one, and a request that arrives
/// while another is being built simply replaces it — a query nobody is looking
/// at any more is not worth the walk. The result is dropped if the index moved
/// while it was being built, because an order taken from an index that has
/// changed is an order that is wrong, and being wrong here means rows that do
/// not exist under a scrollbar that says they do.
fn prepare_loop(shared: Arc<Shared>, jobs: Receiver<Prepare>, stop: Receiver<()>) {
    loop {
        // Shared owns the sender too, so waiting for disconnection would keep
        // the entire engine and its index lock alive after Engine::drop.
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
        // **Charged whether or not it is kept.** A walk that is thrown away
        // below cost exactly as much as one that is used, and it is the thrown
        // away ones this is here to slow down: with a window open the index
        // moves about once a second, so a walk longer than that never survives
        // and would otherwise be repeated for as long as the window is open.
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

fn run(shared: Arc<Shared>, jobs: Receiver<Job>, changes: Receiver<Change>) {
    let mut dirty = false;
    // Set when a walk finishes with changes already queued behind it.
    let mut overdue = false;
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
    // Subtree walks asked for and waiting for their neighbours.
    let mut pending_walks = PendingWalks::default();
    let mut pulses = Pulses::new(
        shared.sources.len(),
        shared.opts.poll_interval,
        shared.opts.reconcile_interval,
    );

    loop {
        // **Wait for the next thing that has to happen, not for a tick.**
        //
        // This was `tick(100ms)`: ten wake-ups a second, forever, owed or not
        // — 864,000 a day on a machine where nothing changed. Measured on a
        // small, quiet, watched source, where what is left is the loop's own
        // pulse and nothing else: **20 ms of CPU per sixty seconds, 0.033% of
        // a core**, and it was the floor under an idle service.
        //
        // Every deadline the body below acts on is known here, so the wait is
        // the nearest of them. Idle — nothing dirty, housekeeping done, no
        // source waiting to be retried — the only one left is the pulse at two
        // seconds.
        //
        // **A deadline that has passed is not a deadline.** The first attempt
        // at this asked for the earlier of `commit_interval` and `patience`
        // whenever anything was dirty. One second after a commit the first is
        // behind us, the wait is zero, and the body declines to commit because
        // the batch is not full and the patience has not run out — so the loop
        // asks again immediately, and again, for the whole fifteen seconds.
        // Each deadline has to be the moment the body would actually *do*
        // something.
        // **The wake-up a bump was not allowed to send.**
        //
        // See [`EngineOptions::await_hold`]: a revision that moved during the
        // quiet second bumped the number and did not wake anybody, and this is
        // the other half of that — the moment the second is over, whoever is
        // asleep is told. First thing in the turn rather than last, so a
        // deadline that has already come due is paid here instead of being
        // asked for again below and floored.
        let now = Instant::now();
        if shared.wake_due().is_some_and(|at| at <= now) {
            shared.announce(now);
        }
        let left = |at: Instant| at.saturating_duration_since(now);
        let mut wake = Duration::from_secs(10);
        // Which deadline set the wake-up, when asked.
        //
        // A deadline that collapses onto the floor below is invisible from
        // outside — the loop looks like it is sleeping, and the only symptom is
        // a wake-up count. Finding the first one took stack samples and a
        // context-switch rate; this is so the second one does not.
        let trace = std::env::var_os("SCOUR_WAKE_TRACE").is_some();
        let mut who = "floor";
        // **Every deadline that has already passed, not only the first one.**
        //
        // `left` saturates at zero and the comparison below is strictly less,
        // so the first deadline to read zero takes the name and every later one
        // that also reads zero is refused it. `pulse` is evaluated first, and
        // it therefore wins every tie: measured over four traced runs of three
        // minutes, **177 of 182 floored lines said "pulse"** and three said
        // "commit" — and "commit" could only appear at all where it was
        // *strictly* smaller than a pulse that had also expired. A commit that
        // cannot run is the exact failure this trace was added to find, and the
        // trace was hiding it behind the pulse.
        //
        // Costs nothing when the trace is off: `Vec::new` does not allocate and
        // nothing is pushed.
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
            // A full batch is held only by the interval floor; an unfull one
            // waits out patience. More changes can fill it early, and those
            // arrive on `changes`, which wakes this anyway.
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
        // **`&& !dirty` is not decoration — it is the difference between a
        // deadline and a spin.** The work this wakes for is guarded by exactly
        // that condition further down, and for a while this half was not: with
        // changes still staged, the compaction below was skipped, so
        // `dirty_settled` was never cleared and `last_compact` never advanced.
        // Once `last_compact + COMPACT_EVERY` was in the past, `left` returned
        // zero every turn, `wake` collapsed onto the twenty-millisecond floor,
        // and the loop ran at **fifty turns a second** — five times the fixed
        // tick this computation replaced, and measured at 69 wake-ups a second
        // against 0.85% of a core with nothing else happening.
        //
        // It only showed once a second source was watched, because that is what
        // keeps `dirty` true often enough for the two guards to disagree. A
        // deadline for work that cannot run is not a deadline; the moment
        // `dirty` clears, the commit that cleared it is itself a wake-up and
        // this is recomputed there.
        if dirty_settled && !dirty {
            deadline!("compact", last_compact + COMPACT_EVERY);
        }
        if !dirty && !idle_done {
            deadline!("idle", last_busy + shared.opts.idle_after);
        }
        if let Some(at) = retries.iter().map(|(_, at, _)| *at).min() {
            deadline!("retry", at);
        }
        // The held walks. Cleared by the flush below in the same turn this
        // fires, so it cannot be the deadline that spins: an entry that reads
        // zero here is walked before the loop comes round again.
        if let Some(at) =
            pending_walks.next_due(shared.opts.walk_debounce, shared.opts.walk_debounce_cap)
        {
            deadline!("walk", at);
        }
        // The held wake-up. Already paid if it was due — the block above runs
        // before this one for exactly that reason — so what is left here is
        // always in the future.
        if let Some(at) = shared.wake_due() {
            deadline!("announce", at);
        }
        // A backstop, not a schedule. If a deadline above is ever computed
        // wrong the cost is fifty turns a second rather than a spun core, and
        // it shows as CPU instead of as housekeeping that quietly stopped.
        wake = wake.max(WAKE_FLOOR);
        if trace && wake <= WAKE_FLOOR {
            // All of them, comma separated. One name would be the shortest true
            // sentence only when exactly one deadline had passed, and the case
            // worth finding is the other one.
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
                        // Cleared here rather than after the walk: from this
                        // moment a request to walk again is about something
                        // this walk may already have passed.
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
                    // **Whatever waited out the walk has waited long enough.**
                    // A walk does not drain this channel, so a file created
                    // while one was running has already been unwritten for as
                    // long as the walk took — and `commit_idle` would then
                    // charge it that again from the moment it lands. The
                    // staleness that setting promises is measured from when
                    // something changed, not from when the index got round to
                    // it, so here the clock is treated as already spent.
                    //
                    // Measured on a start-up walk of two sources, 300 files
                    // created into the first after it was already walked and
                    // swept: **15.0 seconds** from the walk ending to the rows
                    // being findable, all of it this wait — against 0.5 with
                    // the bound turned down. One extra commit a walk, and only
                    // when something actually queued behind it.
                    //
                    // Noted here and acted on where the batch lands, because
                    // the commit at the bottom of this turn belongs to the
                    // walk's own rows: backdating the clock here is spent on
                    // those and the queued ones wait the full patience again.
                    // Which is what the first version of this did, and it
                    // measured worse than no change at all.
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
                    // Whoever these belong to has a watcher that is awake. A
                    // handful is enough to say so — the counter this clears
                    // only matters when a source produces *nothing at all*.
                    for change in batch.iter().take(64) {
                        if let Some(i) = owner_of(&shared, change.path()) {
                            pulses.saw_event(i);
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
                        match report {
                            Ok(r) if r.removed + r.subtrees_removed > 0 => shared.touched(),
                            Ok(_) => {},
                            Err(e) => {
                                scour_core::note!("scourd: changes could not be indexed: {e}");
                                // Applying can consume only part of an iterator.
                                // Reconcile instead of losing the unconsumed tail.
                                for i in 0..shared.sources.len() {
                                    schedule_retry(&mut retries, &pulses, i);
                                }
                            }
                        }
                        dirty = true;
                        idle_done = false;
                        // These are the ones that waited out a walk. See the
                        // note in the scan arm.
                        if overdue {
                            overdue = false;
                            last_commit = Instant::now() - shared.opts.commit_idle;
                        }
                    }
                    // A watcher that lost track becomes a walk of the subtree
                    // it lost. Every platform loses track differently; this is
                    // the one place that has to care.
                    //
                    // Held rather than run: the burst that makes these
                    // expensive arrives spread over seconds, one request per
                    // directory a build creates, and `coalesce` can only merge
                    // what shares a batch. See [`PendingWalks`]; the walking
                    // happens below, once the paths have stopped arriving.
                    let now = Instant::now();
                    for path in coalesce(walks) {
                        pending_walks.add(&path, now);
                    }
                }
                Err(_) => break,
            },
            default(wake) => {}
        }

        // The walks whose neighbours have stopped arriving.
        //
        // Here rather than in the arm that received them, because the moment
        // they become due is usually a moment when nothing arrived at all —
        // that is the entire point of waiting for one.
        for path in pending_walks.take_due(
            Instant::now(),
            shared.opts.walk_debounce,
            shared.opts.walk_debounce_cap,
        ) {
            // An empty path means "I lost track and cannot say where" —
            // inotify exhausting its watches, a kernel buffer overflowing. It
            // used to match no source and be dropped, which is the worst
            // possible reading: the one message that exists to say the index is
            // drifting was the one message thrown away, and the drift then
            // continued silently until someone rescanned by hand.
            if path.is_empty() {
                for i in 0..shared.sources.len() {
                    if !scan(&shared, &mut pulses, i, None) {
                        schedule_retry(&mut retries, &pulses, i);
                    }
                }
            } else if let Some(i) = owner_of(&shared, &path) {
                // **Watched before it is walked, and the order is the whole
                // point.** A walk is a snapshot; a watch is everything after
                // it. The other way round — walk it, then watch it, because now
                // we know it is real — leaves the gap between them covered by
                // neither, which is the same race the walk exists to close,
                // moved rather than removed.
                //
                // Measured on the live index, having got it backwards first:
                // five thousand files written into two hundred fresh
                // directories left **1,260 of them missing**, and not scattered
                // — packages 32 to 82, one unbroken run, which is the window in
                // which the shell loop was fastest. Watching first: 5,000 of
                // 5,000.
                //
                // Watching something about to be walked costs a duplicate
                // upsert at worst, and an upsert is by identity. Where the
                // cover was rebuilt shallow this is also the only way anything
                // below here is ever seen again; everywhere else it is a no-op.
                //
                // The debounce does not weaken this: covering happens when the
                // walk does, and everything the watcher reports in the meantime
                // still flows through `changes` as it always did.
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

        // The pulses, read outside the wait rather than inside it.
        //
        // As an arm of the `select!` they were only read when nothing else was
        // ready, so a machine producing a steady stream of changes could
        // starve them — and an unwatched volume is exactly what pulses exist
        // to notice. `due` carries its own two-second floor, so asking on
        // every turn of the loop costs a comparison.
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
                    // A source nobody is watching, whose pulse moved.
                    if !scan(&shared, &mut pulses, source, None) {
                        schedule_retry(&mut retries, &pulses, source);
                    }
                    dirty = true;
                    idle_done = false;
                    last_busy = Instant::now();
                }
                Nudge::Blind => {
                    // A source that *is* watched, whose pulse has been
                    // moving for minutes with nothing arriving. Said
                    // out loud because a watcher that has gone quiet
                    // is otherwise indistinguishable from a quiet disk.
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
        let watched = shared.watchers.load(Ordering::Relaxed) > 0;
        // **The batch is a latency rule, so it only applies when latency has
        // somebody to matter to.**
        //
        // `commit_batch` is sixty-four because a window waiting on a file it
        // just saved should not wait for a clock. With nothing open, nobody
        // can observe the index at all — there is no latency to protect, and
        // the only reason left to commit is to stop the staging buffer
        // growing. That is a memory bound, and sixty-four rows is nowhere
        // near one.
        //
        // The note below already recorded that this desktop produces 30–50
        // changes a second, which reaches sixty-four in about two: what it
        // did not follow through on is that `commit_idle` was therefore never
        // consulted, open window or not. Measured with nothing running: the
        // entry count moved by **2** in sixty seconds and the revision by
        // **14**. Fourteen commits a minute, at the ~13 ms a commit costs
        // whatever it holds, is 180 ms — the whole of the 0.30% of a core the
        // worker spent while nobody had asked it for anything.
        //
        // Kept as a ceiling rather than removed, because a burst is still
        // real: an unpacked archive or a build tree is hundreds of thousands
        // of changes, and holding fifteen seconds of those unstaged is the
        // memory problem the small number was never guarding against.
        let batch = if watched {
            shared.opts.commit_batch
        } else {
            shared.opts.commit_batch.max(IDLE_BATCH)
        };
        let enough = shared.pending.load(Ordering::Relaxed) >= batch;
        // How long a trickle may wait, and it depends on whether anyone is
        // watching. Nobody is: fifteen seconds, and the machine writes one
        // segment a minute instead of one a second. Somebody is: as soon as the
        // burst clock allows, because what they are waiting for is a file they
        // just saved.
        //
        // This is not a small difference by luck. Measured here, a file created
        // in a watched directory became findable in **0.90 s**.
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
            // And while nobody is waiting, work out what the folders weigh.
            //
            // **90 ms, and the only question is who pays it.** The prefix sums
            // behind the folder-size column are built on first use, and first
            // use is the window opening — so without this the first list
            // anybody sees costs an extra tenth of a second, once per service
            // start, at the moment somebody is watching. Here it is spent on a
            // machine that has been quiet for `idle_after` and has just been
            // asked to give its write buffer back.
            //
            // An empty path list builds the cache and asks nothing of it,
            // which is exactly the shape of a warm-up. After this a commit
            // rebuilds only the segment it changed.
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

/// Walks asked for and not run yet, and when each was first and last asked for.
///
/// **[`coalesce`] can only merge what arrived together, and the requests do not
/// arrive together.** They arrive as a `cargo build` or a `git clone` creates
/// directories, a few hundred milliseconds apart, so each one is its own
/// channel batch, its own walk, its own segment and its own manifest fsync.
/// Measured live over twelve minutes: **157 subtree walks, 13.3 a minute, 0–1
/// ms of walking each, every one of them committing** — the walking is free and
/// the writing is not.
///
/// So a walk waits [`EngineOptions::walk_debounce`] for its neighbours before
/// it runs, and the neighbours that arrive in that window either join it or
/// replace it with their common parent. Nothing is dropped: a request either
/// runs or is covered by an ancestor that runs, and
/// [`EngineOptions::walk_debounce_cap`] is the whole of the delay either can
/// cost.
#[derive(Default)]
struct PendingWalks {
    /// Path, first asked, last asked.
    ///
    /// A `Vec` and a linear scan rather than a map, because the map would have
    /// to be walked anyway to find the earliest deadline and this list is
    /// tiny: the bursts that make the debounce worth having are 22 requests in
    /// five seconds and 58 in seven, and they reduce to one or two paths *as
    /// they arrive* — a request under a path already waiting never becomes an
    /// entry of its own.
    at: Vec<(String, Instant, Instant)>,
}

impl PendingWalks {
    /// Note that `path` wants walking, merging it with what is already waiting.
    fn add(&mut self, path: &str, now: Instant) {
        // Normalised the way `PrefixSet` normalises, so `/a/` and `/a` are one
        // entry and `/` is the empty path — which is what the caller already
        // reads as "walk everything".
        let path = path.trim_end_matches('/');
        if path.is_empty() {
            // A watcher that lost track and cannot say where. Walking every
            // source covers every request in here by definition, and it does
            // not wait: this is the one message that says the index is
            // drifting, and the drift continues until it is answered.
            self.at.clear();
            self.at.push((String::new(), now, now));
            return;
        }
        if let Some(e) = self
            .at
            .iter_mut()
            .find(|(p, _, _)| scour_core::under(path, p))
        {
            // Already covered by a walk that is waiting — the same path, or an
            // ancestor of it. Its clock is refreshed rather than a second entry
            // made, because this arrival is more of the same churn and the
            // point is to let it settle. The cap is what stops that being
            // unbounded, and it is measured from *its* first arrival, which is
            // no later than this one.
            e.2 = now;
            return;
        }
        // The other direction: this path covers some of what is waiting. Those
        // entries go, and the earliest first-asked among them comes with it, so
        // absorbing a request cannot postpone the deadline it already had.
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

    /// Take the walks that have waited long enough, reduced.
    ///
    /// Whatever is left that a taken path covers goes with it: the walk about
    /// to run is that request's walk too, and leaving it behind would run the
    /// same walk again a moment later.
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

/// Walk one source and reconcile what it holds.
/// Walk one source, and say whether the walk could see what it came for.
///
/// `false` means the roots were not there to be read — a volume that has not
/// been mounted yet, a drive pulled out, a share that dropped. The caller is
/// expected to come back later rather than to treat it as an answer.
fn scan(shared: &Arc<Shared>, pulses: &mut Pulses, source: usize, subtree: Option<String>) -> bool {
    let Some(src) = shared.sources.get(source).cloned() else {
        return true;
    };
    let began = Instant::now();
    // A generation the index could not open is a scan that cannot reconcile:
    // its rows would be stamped with the previous one and the sweep would then
    // judge them by it. Reported and retried rather than run blind.
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
    // Read once, here, rather than per directory: a walk has to skip by one
    // set of rules from beginning to end. A rule saved halfway through takes
    // effect on the scan that follows — which is the scan the save asks for.
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
    let mut ended = true;
    if trustworthy {
        // **One call for every root the walk vouched for.** A pass notes the
        // rows it found unchanged so that an untouched filesystem does not
        // have to be rewritten to prove it is still there, and those notes are
        // the pass's rather than any one root's. Sweeping root by root, the
        // first call consumed them and every root after it was reconciled
        // against nothing: this source is rooted at `/usr /etc /opt /var`, and
        // the last three were deleted on every other walk and put back on the
        // one between — `/opt` alternating between 5,477 rows and none, about
        // once a minute, for as long as the service was up.
        let gone = match shared.index.sweep(src.id(), &vouched, generation, &spare) {
            Ok(n) => n,
            // Half a reconciliation. Saying so is all that can be done here;
            // the retry is the caller's, and the rows that should have gone
            // are found again by the next full scan.
            Err(e) => {
                scour_core::note!("scourd: {vouched:?} could not be reconciled: {e}");
                ended = false;
                if let Err(e) = shared.index.abandon_generation(generation) {
                    scour_core::note!("scourd: an incomplete pass could not be ended: {e}");
                }
                0
            }
        };
        // A sweep takes effect at once, like any other removal, so anyone
        // watching should hear about it now rather than at the next commit.
        // The walk's own upserts are staged and announce themselves then.
        if gone > 0 {
            shared.touched();
        }
    } else if let Err(e) = shared.index.abandon_generation(generation) {
        ended = false;
        // **A pass that will not be swept still has to end.** Not sweeping is
        // the right answer here — the walk could not look, and deleting on no
        // evidence is how a directory that lost its read permission loses its
        // files too — but the generation was the sweep's to close, and nobody
        // else was going to.
        //
        // What it cost while nothing did: the index keeps per-segment notes
        // about rows a walk found unchanged, a noted segment cannot be folded,
        // and the notes only go when a generation ends. One walk of a directory
        // that had just been deleted — which a watcher asks for routinely —
        // was enough to stop compaction for good. Measured on the live index at
        // 241 segments, every search reading all of them.
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
fn schedule_retry(retries: &mut Vec<(usize, Instant, usize)>, pulses: &Pulses, source: usize) {
    if !retries.iter().any(|(s, _, _)| *s == source) {
        retries.push((source, pulses.retry_at(source, retry_delay(0)), 0));
    }
}

/// The floor on how often segments are merged without being asked.
///
/// Not a cost of merging — a fold no longer holds anything a search needs —
/// but a cost of *deciding*: `stats()` reads every segment's live count, and
/// there is no point paying it on a machine whose segment count moves by one
/// every few seconds.
const COMPACT_EVERY: Duration = Duration::from_secs(60);

/// The shortest the worker will ever sleep.
///
/// A backstop rather than a schedule: if a deadline is ever computed wrong the
/// cost is fifty turns a second rather than a spun core. Named because the
/// wake-up trace has to compare against exactly the number the sleep is
/// clamped to — the two drifting apart is how a floored wake stops being
/// reported as one.
const WAKE_FLOOR: Duration = Duration::from_millis(20);

/// The batch that stands in for `commit_batch` when nobody is watching.
///
/// `commit_batch` is a latency rule and sixty-four is a latency number. With
/// nothing open there is no latency to protect, and the only reason left to
/// commit is to keep the staging buffer bounded — so this is a memory number.
/// It is a ceiling rather than a removal because a burst is real: an unpacked
/// archive is hundreds of thousands of changes, and holding fifteen seconds of
/// those unstaged is the problem the small number was never guarding against.
const IDLE_BATCH: u64 = 4_096;

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
            // **How far this walk has got, while it is still walking.** The
            // count used to be written once, at the end — so `scanned` was
            // zero for the whole minute a pass takes and every face that
            // showed it showed a zero that never moved. A person who has just
            // switched a skip rule off is watching for exactly this number,
            // and a still one reads as nothing happening.
            //
            // Once a batch, not once a row: a write lock every four thousand
            // entries against one every one.
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

    /// The clock, without one: every instant this file cares about is relative
    /// to when the first request arrived, and a test that slept for them would
    /// take twelve seconds to assert what a subtraction can.
    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    fn due(w: &mut PendingWalks, base: Instant, ms: u64) -> Vec<String> {
        w.take_due(at(base, ms), DEBOUNCE, CAP)
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

    #[test]
    fn nothing_is_walked_while_the_requests_are_still_arriving() {
        // The measured shape: a request every few hundred milliseconds as a
        // build creates directories. Each one is its own channel batch, so
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
        // Without this the debounce is not a delay, it is a cancellation: a
        // build that writes a directory every hundred milliseconds for a
        // minute would leave the subtree out of the index for the minute.
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
        // The trap in merging: `/a/pkg/x` has been held since zero by a trickle
        // of its own descendants, and at 2,900 the parent arrives and swallows
        // it. If the merged entry took the *new* first-asked, the cap would
        // restart and a request that had 100 ms of patience left would be given
        // 3 s more — the debounce would be postponing without bound, which is
        // the one thing the cap exists to prevent.
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
        // `/a/pkg` is due; `/a/pkg/x` arrived a moment ago and is not. Walking
        // `/a/pkg` *is* the walk `/a/pkg/x` asked for, so leaving it behind
        // would run the same walk twice — which is the cost this exists to
        // remove. `/a/other` is covered by neither and must survive.
        let t = Instant::now();
        let mut w = PendingWalks::default();
        w.add("/a/pkg", at(t, 0));
        w.add("/a/other", at(t, 400));
        // Late enough to be swallowed rather than to hold the parent back: it
        // is not the parent's own entry, so it does not refresh its clock.
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
        // An empty path is "the index is drifting and I cannot say where".
        // Holding it holds the only message that stops the drift.
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
