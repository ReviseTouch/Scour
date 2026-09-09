//! The engine, wired to a real index and a source it cannot identify.

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use scour_core::{
    Caps, Change, ChangeSink, Entry, EntryId, EntrySink, Error, FacetBy, Maintenance, Page,
    ScanOptions, ScanReport, SortKey, Source, SourceId, SourceInfo, SourceKind, WatchHandle,
};
use scour_engine::{Engine, EngineOptions};
use scour_index_native::NativeIndex;
use scour_mock::{MockOptions, generate};

/// A source backed by a list that a test can change under the engine's feet. The
/// engine has no way of telling it from a filesystem.
#[derive(Debug)]
struct MemSource {
    entries: RwLock<Vec<Entry>>,
    scans: AtomicU64,
    watchable: bool,
    /// Where the engine asked to be told about changes.
    sink: RwLock<Option<Box<dyn ChangeSink>>>,
    /// Pretend the roots are not there — an unmounted volume, a pulled drive.
    offline: std::sync::atomic::AtomicBool,
    /// Pretend the walk stopped partway: some of the tree reached the sink.
    cancelled: std::sync::atomic::AtomicBool,
    /// Make the walk take this long, so a change can arrive during one.
    slow_ms: AtomicU64,
    /// Every `cover` and every `scan`, in the order they happened — the order is
    /// the property under test, and nothing else can see it.
    order: Arc<RwLock<Vec<&'static str>>>,
    /// What every walk was asked to cover, and when it started; `None` is the whole
    /// source. The debounce tests need each walk's subject, not just the count.
    walked: Arc<RwLock<Vec<(Option<String>, Instant)>>>,
    /// What every `retune` this source's watch was given said to skip.
    retuned: Arc<RwLock<Vec<Vec<String>>>>,
    /// Whether this source answers `excluder` at all.
    has_rules: bool,
    /// The roots this source claims and vouches for. More than one, because a real
    /// source has more than one and the defect only showed past the first root.
    roots: Vec<String>,
}

impl MemSource {
    fn new(entries: Vec<Entry>) -> Arc<MemSource> {
        Arc::new(MemSource {
            entries: RwLock::new(entries),
            scans: AtomicU64::new(0),
            watchable: true,
            sink: RwLock::new(None),
            offline: std::sync::atomic::AtomicBool::new(false),
            cancelled: std::sync::atomic::AtomicBool::new(false),
            slow_ms: AtomicU64::new(0),
            order: Arc::new(RwLock::new(Vec::new())),
            walked: Arc::new(RwLock::new(Vec::new())),
            retuned: Arc::new(RwLock::new(Vec::new())),
            has_rules: false,
            roots: vec!["/home/u".into()],
        })
    }

    /// Like [`MemSource::new`], but it knows what exclusion means.
    fn with_rules(entries: Vec<Entry>) -> Arc<MemSource> {
        let mut s = MemSource::new(entries);
        Arc::get_mut(&mut s).expect("sole owner").has_rules = true;
        s
    }

    /// A source rooted in several places, like every real one.
    fn many_roots(entries: Vec<Entry>, roots: Vec<String>) -> Arc<MemSource> {
        let mut s = MemSource::unwatchable(entries);
        Arc::get_mut(&mut s).expect("sole owner").roots = roots;
        s
    }

    fn unwatchable(entries: Vec<Entry>) -> Arc<MemSource> {
        Arc::new(MemSource {
            entries: RwLock::new(entries),
            scans: AtomicU64::new(0),
            watchable: false,
            sink: RwLock::new(None),
            offline: std::sync::atomic::AtomicBool::new(false),
            cancelled: std::sync::atomic::AtomicBool::new(false),
            slow_ms: AtomicU64::new(0),
            order: Arc::new(RwLock::new(Vec::new())),
            walked: Arc::new(RwLock::new(Vec::new())),
            retuned: Arc::new(RwLock::new(Vec::new())),
            has_rules: false,
            roots: vec!["/home/u".into()],
        })
    }

    /// Report a change the way a watcher would.
    fn changed(&self, c: Change) {
        if let Some(s) = self.sink.read().as_ref() {
            s.emit(c);
        }
    }
}

impl Source for MemSource {
    fn id(&self) -> SourceId {
        SourceId(0)
    }

    fn describe(&self) -> SourceInfo {
        SourceInfo {
            id: SourceId(0),
            name: "mem".into(),
            kind: SourceKind::Local,
            roots: self.roots.clone(),
            caps: self.caps(),
        }
    }

    fn caps(&self) -> Caps {
        let mut c = Caps::CONTENT;
        if self.watchable {
            c |= Caps::WATCH;
        }
        c
    }

    /// A simple stand-in for a real rule set: a path is skipped when a component is
    /// an excluded name or it sits under an excluded prefix. `None` unless asked
    /// for, so "a source with no notion of exclusions" stays reachable.
    fn excluder(
        &self,
        opts: &ScanOptions,
    ) -> Option<Box<dyn Fn(&str, bool) -> bool + Send + Sync>> {
        if !self.has_rules {
            return None;
        }
        let dirs = opts.exclude_dirs.clone();
        let paths = opts.exclude_paths.clone();
        Some(Box::new(move |path: &str, is_dir: bool| {
            if paths
                .iter()
                .any(|p| path == p || path.starts_with(&format!("{p}/")))
            {
                return true;
            }
            // A directory name matches a directory; a symlink sharing it does not.
            let mut parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
            if !is_dir {
                parts.pop();
            }
            parts.iter().any(|part| dirs.iter().any(|d| d == part))
        }))
    }

    fn scan(&self, opts: &ScanOptions, sink: &mut dyn EntrySink) -> scour_core::Result<ScanReport> {
        self.scans.fetch_add(1, Ordering::Relaxed);
        self.walked
            .write()
            .push((opts.subtree.clone(), Instant::now()));
        // A walk that takes a while, so something can happen during it.
        let slow = self.slow_ms.load(Ordering::Relaxed);
        if slow > 0 {
            std::thread::sleep(Duration::from_millis(slow));
        }
        // The report has to say it stopped: everything downstream reconciles on the
        // assumption that a walk saw everything.
        if self.cancelled.load(Ordering::Relaxed) {
            let mut n = 0;
            for e in self.entries.read().iter().take(5) {
                n += 1;
                let _ = sink.push(e.clone());
            }
            return Ok(ScanReport {
                entries: n,
                cancelled: true,
                vouched: vec![String::new()],
                took_ms: 0,
                ..Default::default()
            });
        }
        // Every walk, not only a subtree's: the start-up one is the bigger window.
        self.order.write().push("scan");
        // A root that is not there yet: the walk could not look, not found nothing.
        if self.offline.load(Ordering::Relaxed) {
            // Nothing vouched for, so nothing may be reconciled against it.
            return Ok(ScanReport {
                took_ms: 0,
                ..Default::default()
            });
        }
        let mut n = 0;
        for e in self.entries.read().iter() {
            if let Some(sub) = &opts.subtree
                && !e.path.starts_with(sub.as_str())
            {
                continue;
            }
            n += 1;
            if sink.push(e.clone()).is_stop() {
                return Ok(ScanReport {
                    entries: n,
                    cancelled: true,
                    ..Default::default()
                });
            }
        }
        Ok(ScanReport {
            entries: n,
            vouched: match &opts.subtree {
                Some(s) => vec![s.clone()],
                None => self.roots.clone(),
            },
            ..Default::default()
        })
    }

    fn watch(
        &self,
        _o: &scour_core::ScanOptions,
        s: Box<dyn ChangeSink>,
    ) -> scour_core::Result<Box<dyn WatchHandle>> {
        if !self.watchable {
            return Err(Error::unsupported("watch"));
        }
        // Installing a watch takes time and the test is about that time: a home
        // directory is 342,000 inotify watches, fifteen seconds, while `rescan`
        // only queues a job and returns at once.
        std::thread::sleep(Duration::from_millis(150));
        // Recorded once it is in place, not when it was asked for.
        self.order.write().push("watch");
        *self.sink.write() = Some(s);
        Ok(Box::new(NoopWatch {
            order: Arc::clone(&self.order),
            retuned: Arc::clone(&self.retuned),
        }))
    }

    fn open(&self, _id: &EntryId) -> scour_core::Result<Box<dyn Read + Send>> {
        Err(Error::unsupported("open"))
    }

    fn stat(&self, path: &str) -> scour_core::Result<Entry> {
        self.entries
            .read()
            .iter()
            .find(|e| e.path == path)
            .cloned()
            .ok_or_else(|| Error::NotFound { path: path.into() })
    }
}

struct Fixture {
    engine: Engine,
    source: Arc<MemSource>,
    _dir: tempfile::TempDir,
}

/// An index that stops accepting rows partway through, standing in for a disk that
/// fills: rows the walk found and the index does not have look, from the sweep's
/// side, exactly like files that are gone.
#[derive(Debug)]
struct Fragile {
    inner: Arc<NativeIndex>,
    /// Applies to refuse. Counted down; zero refuses for ever after.
    left: AtomicU64,
    refused: AtomicU64,
    sweep_failures: AtomicU64,
    abandoned: AtomicU64,
}

impl scour_core::Index for Fragile {
    fn apply(
        &self,
        changes: &mut dyn Iterator<Item = scour_core::Change>,
    ) -> scour_core::Result<scour_core::ApplyReport> {
        if self.left.load(Ordering::Relaxed) == 0 {
            self.refused.fetch_add(1, Ordering::Relaxed);
            // Drained: an iterator handed over is handed over either way.
            while changes.next().is_some() {}
            return Err(scour_core::Error::Io {
                detail: "disk dolu (test)".into(),
            });
        }
        self.left.fetch_sub(1, Ordering::Relaxed);
        self.inner.apply(changes)
    }
    fn begin_generation(&self) -> scour_core::Result<u64> {
        self.inner.begin_generation()
    }
    fn sweep(
        &self,
        source: SourceId,
        under: &[String],
        generation: u64,
        spare: &scour_core::PrefixSet,
    ) -> scour_core::Result<u64> {
        if self
            .sweep_failures
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(Error::Io {
                detail: "injected sweep failure".into(),
            });
        }
        self.inner.sweep(source, under, generation, spare)
    }
    fn abandon_generation(&self, g: u64) -> scour_core::Result<()> {
        self.abandoned.fetch_add(1, Ordering::Relaxed);
        self.inner.abandon_generation(g)
    }
    fn commit(&self) -> scour_core::Result<()> {
        self.inner.commit()
    }
    fn maintain(
        &self,
        level: scour_core::Maintenance,
    ) -> scour_core::Result<scour_core::MaintReport> {
        self.inner.maintain(level)
    }
    fn search(
        &self,
        req: &scour_core::SearchRequest,
    ) -> scour_core::Result<scour_core::SearchResponse> {
        self.inner.search(req)
    }
    fn facets(
        &self,
        req: &scour_core::FacetRequest,
    ) -> scour_core::Result<scour_core::FacetResponse> {
        self.inner.facets(req)
    }
    fn stats(&self) -> scour_core::Result<scour_core::IndexStats> {
        self.inner.stats()
    }
}

fn fixture(files: usize) -> Fixture {
    tuned_fixture(files, |_| {})
}

/// A fixture whose engine options the test gets to change first: the real clocks
/// are seconds, and scaling them down is the difference between a suite and a wait.
fn tuned_fixture(files: usize, tune: impl FnOnce(&mut EngineOptions)) -> Fixture {
    let dir = tempfile::tempdir().expect("temp");
    let index = Arc::new(NativeIndex::open_or_create(dir.path()).expect("index"));
    let fs = generate(&MockOptions {
        files,
        ..Default::default()
    });
    let source = MemSource::new(fs.entries);
    let mut opts = EngineOptions {
        commit_interval: Duration::from_millis(50),
        ..Default::default()
    };
    tune(&mut opts);
    let engine = Engine::new(vec![source.clone()], index, opts);
    Fixture {
        engine,
        source,
        _dir: dir,
    }
}

/// One entry, named. For the tests where the *paths* are the fixture.
fn row(path: &str) -> Entry {
    Entry {
        id: EntryId::path_hash(SourceId(0), path),
        path: path.to_owned(),
        is_dir: !path.contains('.'),
        meta: scour_core::Meta::UNKNOWN,
    }
}

/// Wait for a condition, or give up. The engine is asynchronous by design.
fn settle(f: &Fixture, what: impl Fn(&Fixture) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if what(f) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn count(f: &Fixture, q: &str) -> u64 {
    f.engine
        .search(q, SortKey::Modified, true, Page::new(0, 1))
        .expect("search")
        .total
}

#[test]
fn a_rescan_fills_the_index() {
    let f = fixture(2_000);
    assert!(f.engine.status().cold, "nothing has been indexed yet");
    f.engine.rescan(None).expect("rescan");
    settle(&f, |f| {
        f.engine.status().entries > 0 && !f.engine.status().scanning
    });

    let s = f.engine.status();
    assert_eq!(s.entries, f.source.entries.read().len() as u64);
    assert!(!s.cold);
    assert_eq!(s.sources, 1);
    assert!(count(&f, "ext:rs") > 0);
}

#[test]
fn a_second_rescan_removes_what_disappeared() {
    // A rescan reconciles: what a scan did not find stops being in the index.
    let f = fixture(2_000);
    f.engine.rescan(None).expect("rescan");
    settle(&f, |f| {
        !f.engine.status().scanning && f.engine.status().entries > 0
    });
    let before = f.engine.status().entries;

    let removed: Vec<String> = {
        let mut all = f.source.entries.write();
        let doomed: Vec<String> = all.iter().rev().take(500).map(|e| e.path.clone()).collect();
        let keep = all.len() - 500;
        all.truncate(keep);
        doomed
    };
    f.engine.rescan(None).expect("rescan");
    settle(&f, |f| {
        !f.engine.status().scanning && f.engine.status().entries < before
    });

    assert_eq!(f.engine.status().entries, before - 500);
    for p in removed.iter().take(20) {
        let name = p.rsplit('/').next().unwrap_or(p);
        if name.len() >= 3 {
            let hits = f
                .engine
                .search(
                    &format!("\"{name}\""),
                    SortKey::Name,
                    false,
                    Page::new(0, 50),
                )
                .expect("search");
            assert!(
                !hits.hits.iter().any(|h| &h.path == p),
                "{p} was deleted and is still indexed"
            );
        }
    }
}

#[test]
fn rescanning_one_subtree_leaves_the_rest_alone() {
    let f = fixture(3_000);
    f.engine.rescan(None).expect("rescan");
    settle(&f, |f| {
        !f.engine.status().scanning && f.engine.status().entries > 0
    });
    let before = f.engine.status().entries;

    // Empty the archive subtree and reconcile only that.
    let sub = "/home/u/Projeler/eski-arsiv";
    let n_under = {
        let mut all = f.source.entries.write();
        let n = all.iter().filter(|e| e.path.starts_with(sub)).count();
        all.retain(|e| !e.path.starts_with(sub));
        n
    };
    assert!(n_under > 0, "the fixture should have an archive tree");

    f.engine.rescan(Some(sub.into())).expect("rescan");
    settle(&f, |f| {
        !f.engine.status().scanning && f.engine.status().entries < before
    });
    assert_eq!(f.engine.status().entries, before - n_under as u64);
    assert!(
        count(&f, "ext:rs") > 0,
        "the rest of the index is untouched"
    );
}

#[test]
fn a_tree_listing_is_bounded_per_level_and_says_when_it_truncated() {
    let f = fixture(3_000);
    f.engine.rescan(None).expect("rescan");
    settle(&f, |f| {
        !f.engine.status().scanning && f.engine.status().entries > 0
    });

    let root = f.engine.tree("/home/u", 1, 5).expect("tree");
    assert_eq!(root.path, "/home/u");
    assert!(
        root.children > 5,
        "the fixture root has more than five children"
    );
    assert_eq!(root.nodes.len(), 5);
    assert!(root.truncated, "a listing that omits things must admit it");
    // Directories first, so a reader sees the structure before the contents.
    let dirs_first = root.nodes.iter().map(|n| n.is_dir).collect::<Vec<_>>();
    let mut sorted = dirs_first.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(dirs_first, sorted);

    let deep = f.engine.tree("/home/u", 2, 3).expect("tree");
    assert!(
        deep.nodes.iter().any(|n| !n.nodes.is_empty()),
        "depth 2 should reach grandchildren"
    );
    let shallow = f.engine.tree("/home/u", 0, 3).expect("tree");
    assert!(shallow.nodes.is_empty());
}

#[test]
fn explain_reads_a_query_back_without_running_it() {
    let f = fixture(100);
    let e = f.engine.explain("rapor ext:pdf", None);
    assert_eq!(
        e.description,
        "name contains \"rapor\" and extension is .pdf"
    );
    assert!(!e.needs_content);
    // The same reading in pieces a search box can colour, covering it byte for byte.
    let rebuilt: String = e.spans.iter().map(|s| s.of("rapor ext:pdf")).collect();
    assert_eq!(rebuilt, "rapor ext:pdf");
    assert!(
        e.spans.iter().all(|s| !s.role.is_warning()),
        "nothing in a correct query is a mistake"
    );
    assert!(e.completions.is_empty(), "no caret, no completions");
    // The forgiving parser is why this exists: a field name that does not exist is
    // searched for as text. Case is not the mistake — field names are folded.
    let bad = f.engine.explain("sze:>1mb", None);
    assert_eq!(bad.description, "name contains \"sze:>1mb\"");
    assert!(
        bad.spans.iter().any(|s| s.role.is_warning()),
        "and the colouring says so too, while it is being typed"
    );
    assert!(f.engine.explain("content:x", None).needs_content);
    // With a caret, the same call offers what could come next.
    let c = f.engine.explain("ext", Some(3));
    assert_eq!(
        c.completions.first().map(|c| c.insert.as_str()),
        Some("ext:")
    );
}

#[test]
fn stat_answers_from_the_source_so_a_new_file_is_never_missing() {
    let f = fixture(50);
    let path = f.source.entries.read()[10].path.clone();
    // Nothing has been indexed at all, and it still answers.
    assert_eq!(f.engine.stat(&path).expect("stat").path, path);
    assert_eq!(
        f.engine.stat("/home/u/nope").unwrap_err().code(),
        "not_found"
    );
}

#[test]
fn maintenance_reports_and_the_rebuild_clears_the_tail() {
    let f = fixture(2_000);
    f.engine.rescan(None).expect("rescan");
    settle(&f, |f| {
        !f.engine.status().scanning && f.engine.status().entries > 0
    });
    // `unsorted` means "rows outside the largest segment", so a scan that lands in
    // one segment has none: the tail counts segments, not recency.
    let before = f.engine.status().unsorted;

    f.engine.maintain(Maintenance::Rebuild).expect("maintain");
    settle(&f, |f| f.engine.status().unsorted == 0);
    assert_eq!(
        f.engine.status().unsorted,
        0,
        "a rebuild folds everything into one segment, tail or no tail (was {before})"
    );
    assert!(!f.engine.status().rebuild_advised);

    // Flush is answered directly, because callers want its result.
    let r = f.engine.maintain(Maintenance::Flush).expect("flush");
    assert_eq!(r.level, Maintenance::Flush);
}

#[test]
fn a_source_that_cannot_be_watched_is_not_an_error() {
    // Caps says so in advance; a source with no change feed is rescanned instead.
    let dir = tempfile::tempdir().expect("temp");
    let index = Arc::new(NativeIndex::open_or_create(dir.path()).expect("index"));
    let source = MemSource::unwatchable(Vec::new());
    let engine = Engine::new(vec![source], index, EngineOptions::default());
    assert_eq!(engine.start_watching().expect("watch"), 0);
    assert_eq!(engine.status().watching, 0);
}

#[test]
fn watched_changes_are_batched_and_committed() {
    let f = fixture(500);
    assert_eq!(f.engine.start_watching().expect("watch"), 1);
    f.engine.rescan(None).expect("rescan");
    settle(&f, |f| {
        !f.engine.status().scanning && f.engine.status().entries > 0
    });
    let before = f.engine.status().entries;

    let path = "/home/u/brand-new-thing.rs";
    f.source.changed(Change::Upsert(Entry {
        id: EntryId::path_hash(SourceId(0), path),
        path: path.into(),
        is_dir: false,
        meta: scour_core::Meta {
            mtime: 2_000_000_000,
            size: 7,
            ..scour_core::Meta::UNKNOWN
        },
    }));
    settle(&f, |f| f.engine.status().entries > before);
    assert_eq!(count(&f, "brand-new-thing"), 1);

    // And a removal takes effect without waiting for the commit clock.
    f.source
        .changed(Change::RemoveSubtree { path: path.into() });
    settle(&f, |f| count(f, "brand-new-thing") == 0);
    assert_eq!(count(&f, "brand-new-thing"), 0);
}

#[test]
fn waiting_returns_when_something_changes_and_not_before() {
    // What a live list rests on: the client hands back the revision it is showing.
    let f = fixture(200);
    assert_eq!(f.engine.start_watching().expect("watch"), 1);
    f.engine.rescan(None).expect("rescan");
    settle(&f, |f| {
        !f.engine.status().scanning && f.engine.status().entries > 0
    });
    let seen = f.engine.status().revision;

    // Nothing is happening, so this costs the whole timeout and says so.
    let started = Instant::now();
    let quiet = f.engine.await_change(seen, Duration::from_millis(300));
    assert_eq!(
        quiet.revision, seen,
        "nothing changed, so nothing to report"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(250),
        "it returned early from a change that did not happen"
    );

    // On another thread, because the point is that it sleeps rather than polls.
    let (after, took) = std::thread::scope(|s| {
        let waiter = s.spawn(|| {
            let at = Instant::now();
            (f.engine.await_change(seen, Duration::from_secs(20)), at)
        });
        std::thread::sleep(Duration::from_millis(50));
        let path = "/home/u/canli.rs";
        f.source.changed(Change::Upsert(Entry {
            id: EntryId::path_hash(SourceId(0), path),
            path: path.into(),
            is_dir: false,
            meta: scour_core::Meta {
                mtime: 2_000_000_000,
                size: 7,
                ..scour_core::Meta::UNKNOWN
            },
        }));
        let (status, at) = waiter.join().expect("the waiter");
        (status, at.elapsed())
    });
    assert_ne!(after.revision, seen, "a commit landed and nobody was told");
    // A file created while somebody is watching is announced on the burst clock.
    assert!(
        took < Duration::from_secs(5),
        "waited {took:?} for a change a watched index should announce in about a second"
    );
    assert_eq!(count(&f, "canli"), 1, "and it is findable when announced");
}

/// Remove one indexed row every `every`, for `how_long`, and say how many. A
/// removal is the cheapest honest bump: `apply` hides it before it returns.
fn drive_removals(f: &Fixture, every: Duration, how_long: Duration) -> u64 {
    let doomed: Vec<String> = f
        .source
        .entries
        .read()
        .iter()
        .filter(|e| !e.is_dir)
        .map(|e| e.path.clone())
        .collect();
    let began = Instant::now();
    let mut n = 0;
    for path in doomed {
        if began.elapsed() >= how_long {
            break;
        }
        f.source.changed(Change::RemoveSubtree { path });
        n += 1;
        std::thread::sleep(every);
    }
    n
}

#[test]
fn the_revision_is_exact_even_when_nobody_is_woken() {
    // `Status::revision` is exact for anyone who asks: the hold is on the push.
    let f = tuned_fixture(200, |o| {
        // Long enough that nothing in this test could possibly be a wake-up.
        o.await_hold = Duration::from_secs(30);
    });
    assert_eq!(f.engine.start_watching().expect("watch"), 1);
    f.engine.rescan(None).expect("rescan");
    settle(&f, |f| {
        !f.engine.status().scanning && f.engine.status().entries > 0
    });
    let before = f.engine.status().revision;

    // One named row, so "the revision moved" is checked against something that left.
    let doomed = f
        .source
        .entries
        .read()
        .iter()
        .find(|e| !e.is_dir)
        .expect("a file to remove")
        .path
        .clone();
    let term = doomed.rsplit('/').next().expect("a name").to_owned();
    assert_eq!(count(&f, &term), 1, "the row was not there to remove");

    f.source.changed(Change::RemoveSubtree { path: doomed });
    settle(&f, |f| f.engine.status().revision > before);

    assert!(
        f.engine.status().revision > before,
        "the revision stopped moving because the wake-up was held"
    );
    assert_eq!(
        count(&f, &term),
        0,
        "and it moved because the row really went"
    );
}

#[test]
fn a_waiter_is_woken_once_a_hold_however_fast_the_index_moves() {
    // Measured live: 3.5–6.2 bumps a second against at most one commit a second, at
    // 20.5–21 ms of service CPU each — 7–13% of a core for one idle page.
    let hold = Duration::from_millis(500);
    let drive = Duration::from_secs(3);
    let f = tuned_fixture(400, |o| o.await_hold = hold);
    assert_eq!(f.engine.start_watching().expect("watch"), 1);
    f.engine.rescan(None).expect("rescan");
    settle(&f, |f| {
        !f.engine.status().scanning && f.engine.status().entries > 0
    });

    let stop = std::sync::atomic::AtomicBool::new(false);
    let start = f.engine.status().revision;
    let (wakes, last_seen, moved, reached) = std::thread::scope(|s| {
        // A long timeout is the measurement: a waiter timing out every 400 ms would
        // read held-back bumps off the clock and count them as wake-ups.
        let waiter = s.spawn(|| {
            let mut seen = start;
            let mut wakes = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let st = f.engine.await_change(seen, Duration::from_secs(20));
                if st.revision != seen {
                    wakes += 1;
                    seen = st.revision;
                }
            }
            (wakes, seen)
        });
        // Ten removals a second — twice the fastest rate measured live.
        let sent = drive_removals(&f, Duration::from_millis(100), drive);
        assert!(sent >= 20, "only {sent} removals were sent");
        // Longer than the hold, so the last held bump has been announced.
        std::thread::sleep(hold * 3);
        let reached = f.engine.status().revision;
        let moved = reached - start;
        // An explicit flush is announced at once, releasing the waiter.
        stop.store(true, Ordering::Relaxed);
        f.engine.maintain(Maintenance::Flush).expect("flush");
        let (wakes, seen) = waiter.join().expect("the waiter");
        (wakes, seen, moved, reached)
    });

    assert!(
        moved >= 20,
        "only {moved} revisions in {drive:?} — this is not the churn being measured"
    );
    // The drive plus the settling sleep plus the flush, with two spare for boundaries.
    let allowed = ((drive + hold * 3).as_millis() / hold.as_millis()) as u64 + 3;
    assert!(
        wakes <= allowed,
        "woken {wakes} times for {moved} revisions; at most {allowed} were allowed"
    );
    assert!(
        last_seen >= reached,
        "the waiter stopped at {last_seen} while the index had reached {reached} — \
         a held-back wake-up must be late, not missing"
    );
}

#[test]
fn a_watcher_that_lost_track_causes_a_walk_rather_than_a_guess() {
    // Every platform loses track differently and all of them say this same thing.
    let f = fixture(300);
    assert_eq!(f.engine.start_watching().expect("watch"), 1);
    let scans_before = f.source.scans.load(Ordering::Relaxed);

    f.source.changed(Change::Rescan {
        path: "/home/u".into(),
    });
    settle(&f, |f| {
        f.source.scans.load(Ordering::Relaxed) > scans_before
    });
    settle(&f, |f| {
        !f.engine.status().scanning && f.engine.status().entries > 0
    });
    assert!(
        f.source.scans.load(Ordering::Relaxed) > scans_before,
        "a rescan hint must walk"
    );
    assert_eq!(
        f.engine.status().entries,
        f.source.entries.read().len() as u64
    );
}

/// Every walk of a subtree the source was asked for, in order.
fn subtree_walks(f: &Fixture) -> Vec<(String, Instant)> {
    f.source
        .walked
        .read()
        .iter()
        .filter_map(|(s, at)| s.clone().map(|s| (s, *at)))
        .collect()
}

#[test]
fn a_cluster_of_rescans_becomes_one_walk() {
    // A walk is cheap and the commit it ends in is not: 157 subtree walks in twelve
    // minutes of a build, 0–1 ms each, every one writing a segment and fsyncing.
    let f = tuned_fixture(300, |o| {
        o.walk_debounce = Duration::from_millis(300);
        o.walk_debounce_cap = Duration::from_secs(3);
    });
    assert_eq!(f.engine.start_watching().expect("watch"), 1);

    // Ten requests for one subtree, none far enough apart to settle.
    for _ in 0..10 {
        f.source.changed(Change::Rescan {
            path: "/home/u/pkg".into(),
        });
        std::thread::sleep(Duration::from_millis(25));
    }
    settle(&f, |f| !subtree_walks(f).is_empty());
    // The cluster spans 250 ms and the debounce is 300, so a second walk would show.
    std::thread::sleep(Duration::from_millis(600));

    let walks = subtree_walks(&f);
    assert_eq!(
        walks.iter().map(|(p, _)| p.as_str()).collect::<Vec<_>>(),
        vec!["/home/u/pkg"],
        "ten requests for one subtree are one walk"
    );
}

#[test]
fn a_walk_of_a_parent_answers_the_requests_below_it_and_siblings_still_walk() {
    // Merging is allowed only where the walk that runs covers the request dropped;
    // two directories beside each other cover nothing of each other's.
    let f = tuned_fixture(300, |o| {
        o.walk_debounce = Duration::from_millis(300);
        o.walk_debounce_cap = Duration::from_secs(3);
    });
    assert_eq!(f.engine.start_watching().expect("watch"), 1);

    for p in [
        "/home/u/pkg/a",
        "/home/u/pkg/b",
        "/home/u/pkg",
        "/home/u/etc",
    ] {
        f.source.changed(Change::Rescan { path: p.into() });
        std::thread::sleep(Duration::from_millis(25));
    }
    settle(&f, |f| subtree_walks(f).len() >= 2);
    std::thread::sleep(Duration::from_millis(600));

    let mut walked: Vec<String> = subtree_walks(&f).into_iter().map(|(p, _)| p).collect();
    walked.sort();
    assert_eq!(
        walked,
        vec!["/home/u/etc".to_owned(), "/home/u/pkg".to_owned()],
        "the parent absorbed its children and the sibling walked on its own"
    );
}

#[test]
fn a_trickle_that_never_stops_is_walked_at_the_cap_anyway() {
    // The debounce is a bet that the cluster ends; without the cap, a build that
    // never lets it would postpone the walk for as long as it ran.
    let f = tuned_fixture(300, |o| {
        o.walk_debounce = Duration::from_millis(300);
        o.walk_debounce_cap = Duration::from_secs(1);
    });
    assert_eq!(f.engine.start_watching().expect("watch"), 1);

    let began = Instant::now();
    // Requests 100 ms apart: the debounce can never expire, so only the cap fires.
    let trickle = std::thread::scope(|s| {
        let h = s.spawn(|| {
            while began.elapsed() < Duration::from_secs(3) {
                f.source.changed(Change::Rescan {
                    path: "/home/u/pkg".into(),
                });
                std::thread::sleep(Duration::from_millis(100));
            }
        });
        settle(&f, |f| !subtree_walks(f).is_empty());
        let first = subtree_walks(&f).first().map(|(_, at)| *at);
        h.join().expect("the trickle");
        first
    });
    let first = trickle.expect("the trickle was never walked at all");
    let waited = first.duration_since(began);
    assert!(
        waited < Duration::from_millis(1_800),
        "a walk asked for at the start of a continuous trickle waited {waited:?}, \
         past the {:?} cap",
        Duration::from_secs(1)
    );
}

#[test]
fn a_watcher_that_lost_track_does_not_wait_for_the_debounce() {
    // An empty path says the index is drifting, so it bypasses the debounce —
    // asserted against a debounce longer than the assertion's own patience.
    let f = tuned_fixture(300, |o| {
        o.walk_debounce = Duration::from_secs(30);
        o.walk_debounce_cap = Duration::from_secs(60);
    });
    assert_eq!(f.engine.start_watching().expect("watch"), 1);
    let before = f.source.scans.load(Ordering::Relaxed);

    let began = Instant::now();
    f.source.changed(Change::Rescan {
        path: String::new(),
    });
    settle(&f, |f| f.source.scans.load(Ordering::Relaxed) > before);
    let took = began.elapsed();

    assert!(
        f.source.scans.load(Ordering::Relaxed) > before,
        "a watcher that lost track was left waiting for a debounce"
    );
    assert!(
        took < Duration::from_secs(5),
        "it waited {took:?} — the bypass is not a bypass"
    );
    assert_eq!(
        f.source.walked.read().last().expect("a walk").0,
        None,
        "and it walked the whole source, not a subtree"
    );
}

#[test]
fn a_volume_that_was_not_mounted_yet_is_picked_up_without_being_asked() {
    // The service starts before the volumes are mounted, so the first walk of an
    // external disk finds nothing. Refusing to reconcile is right; never asking
    // again leaves the disk stale until somebody types `scour rescan`.
    let f = fixture(400);
    let held = f.source.entries.read().len() as u64;
    f.engine.rescan(None).expect("rescan");
    settle(&f, |f| f.engine.status().entries == held);
    assert_eq!(f.engine.status().entries, held);

    // The drive goes away, and something asks for a walk.
    f.source.offline.store(true, Ordering::Relaxed);
    let before = f.source.scans.load(Ordering::Relaxed);
    f.engine.rescan(None).expect("rescan");
    settle(&f, |f| f.source.scans.load(Ordering::Relaxed) > before);
    settle(&f, |f| !f.engine.status().scanning);
    assert_eq!(
        f.engine.status().entries,
        held,
        "a walk that could not look must not empty the index"
    );

    // It comes back. Nobody types anything.
    let tries = f.source.scans.load(Ordering::Relaxed);
    f.source.offline.store(false, Ordering::Relaxed);
    settle(&f, |f| f.source.scans.load(Ordering::Relaxed) > tries);
    assert!(
        f.source.scans.load(Ordering::Relaxed) > tries,
        "the engine has to come back on its own"
    );
    settle(&f, |f| f.engine.status().entries == held);
    assert_eq!(f.engine.status().entries, held);
}

/// Nothing is walked until everything is watched, on the first walk of every
/// source — the biggest window there is, seven seconds on this machine.
#[test]
fn everything_is_watched_before_the_first_walk() {
    let f = fixture(200);
    f.engine.cover_then_walk(true).expect("cover then walk");
    settle(&f, |f| f.source.order.read().contains(&"scan"));

    let order = f.source.order.read().clone();
    assert_eq!(
        order.first(),
        Some(&"watch"),
        "the first walk must not start before the watch is in place: {order:?}"
    );
    assert!(
        order.contains(&"scan"),
        "and the walk has to happen: {order:?}"
    );
}

/// The same call, told not to walk, still watches: `scan.on_start = false` is a
/// real configuration and must not make watching conditional on walking.
#[test]
fn not_walking_on_start_still_watches() {
    let f = fixture(50);
    let (n, _) = f.engine.cover_then_walk(false).expect("cover only");
    assert_eq!(n, 1, "the source is watchable and was not watched");
    assert!(
        !f.source.order.read().contains(&"scan"),
        "nothing asked for a walk"
    );
}

/// What waited out a walk does not then wait out the clock as well. A walk does not
/// drain the change channel, and `commit_idle` is a staleness bound measured from
/// the change: 7.6 and 13.3 s from walk end to findable, against 0.00.
#[test]
fn a_change_that_waited_out_a_walk_is_written_at_once() {
    let f = fixture(200);
    assert_eq!(f.engine.start_watching().expect("watch"), 1);
    // Nobody is looking, so the slow clock applies — long enough to be unmistakable.
    let held = f.engine.status().entries;

    f.source.slow_ms.store(1_200, Ordering::Relaxed);
    f.engine.rescan(None).expect("rescan");
    // Into the window: the worker is inside the walk and not draining changes.
    std::thread::sleep(Duration::from_millis(300));
    let mut e = f.source.entries.read()[0].clone();
    e.path = "/home/u/gecikme-testi.txt".into();
    e.id = scour_core::EntryId::path_hash(SourceId(0), &e.path);
    f.source.changed(Change::Upsert(e));

    settle(&f, |f| !f.engine.status().scanning);
    // Measured from the commit, this row would wait the whole `commit_idle` here.
    let deadline = Instant::now() + Duration::from_secs(4);
    while Instant::now() < deadline {
        if count(&f, "gecikme-testi") > 0 {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "a row that queued behind the walk was still unwritten four seconds \
         after it ended (index holds {}, was {held})",
        f.engine.status().entries
    );
}

/// A walk that stopped partway is not evidence either: it reached some of the tree
/// and none of the rest, and sweeping on it deletes what it never got to. Its own
/// test, because the two guards are one line and either alone looks sufficient.
#[test]
fn a_walk_that_stopped_partway_stops_the_sweep() {
    let f = fixture(400);
    f.engine.rescan(None).expect("rescan");
    settle(&f, |f| f.engine.status().entries > 0);
    settle(&f, |f| !f.engine.status().scanning);
    let held = f.engine.status().entries;
    assert!(held > 100, "the first walk has to land: {held}");

    f.source.cancelled.store(true, Ordering::Relaxed);
    let before = f.source.scans.load(Ordering::Relaxed);
    f.engine.rescan(None).expect("rescan");
    settle(&f, |f| f.source.scans.load(Ordering::Relaxed) > before);
    settle(&f, |f| !f.engine.status().scanning);

    assert_eq!(
        f.engine.status().entries,
        held,
        "a walk that never finished was swept on anyway"
    );
}

/// A walk the index could not take is not evidence, so nothing is swept on it: a
/// refused batch leaves rows on the filesystem and missing from the index, which
/// from the sweep's side is indistinguishable from a deletion.
#[test]
fn a_batch_the_index_refused_stops_the_sweep() {
    let dir = tempfile::tempdir().expect("temp");
    let real = Arc::new(NativeIndex::open_or_create(dir.path()).expect("index"));
    let fs = generate(&MockOptions {
        files: 400,
        ..Default::default()
    });
    let source = MemSource::new(fs.entries);
    // One walk lands whole, the next is refused: there must be something to lose.
    let fragile = Arc::new(Fragile {
        inner: Arc::clone(&real),
        left: AtomicU64::new(u64::MAX),
        refused: AtomicU64::new(0),
        sweep_failures: AtomicU64::new(0),
        abandoned: AtomicU64::new(0),
    });
    let engine = Engine::new(
        vec![source.clone()],
        Arc::clone(&fragile) as Arc<dyn scour_core::Index>,
        EngineOptions {
            commit_interval: Duration::from_millis(50),
            ..Default::default()
        },
    );
    let f = Fixture {
        engine,
        source,
        _dir: dir,
    };
    f.engine.rescan(None).expect("rescan");
    settle(&f, |f| f.engine.status().entries > 0);
    settle(&f, |f| !f.engine.status().scanning);
    let held = f.engine.status().entries;
    assert!(held > 100, "the first walk has to land: {held}");

    // And now the index stops taking rows.
    fragile.left.store(0, Ordering::Relaxed);
    let before = f.source.scans.load(Ordering::Relaxed);
    f.engine.rescan(None).expect("rescan");
    settle(&f, |f| f.source.scans.load(Ordering::Relaxed) > before);
    settle(&f, |f| !f.engine.status().scanning);

    assert!(
        fragile.refused.load(Ordering::Relaxed) > 0,
        "the fixture did not actually refuse anything"
    );
    assert_eq!(
        f.engine.status().entries,
        held,
        "a walk the index could not take was swept on anyway"
    );
}

#[test]
fn a_subtree_is_watched_before_it_is_walked() {
    // A walk is a snapshot and a watch is everything after it, so walking first
    // leaves a gap covered by neither: 5,000 files into 200 fresh directories lost
    // 1,260. Watching something about to be walked costs a duplicate upsert at worst.
    let f = fixture(200);
    assert_eq!(f.engine.start_watching().expect("watch"), 1);
    f.source.order.write().clear();

    f.source.changed(Change::Rescan {
        path: "/home/u/Projeler".into(),
    });
    settle(&f, |f| f.source.order.read().contains(&"scan"));

    let order = f.source.order.read().clone();
    assert_eq!(
        order.first(),
        Some(&"cover"),
        "the watch has to be in place before the walk starts: {order:?}"
    );
}

/// New rules reach the walk *and* the watchers: without the second, a scan sweeps
/// `target` clean and the watcher puts every file the next build writes back.
#[test]
fn new_rules_reach_the_walk_and_every_watcher() {
    let f = fixture(200);
    assert_eq!(f.engine.start_watching().expect("watch"), 1);

    let mut fresh = (*f.engine.scan_options()).clone();
    fresh.exclude_dirs = vec!["target".into()];
    f.engine.set_scan_options(fresh);

    assert_eq!(
        f.engine.scan_options().exclude_dirs,
        vec!["target".to_owned()],
        "the walk is still being configured by the old rules"
    );
    let retuned = f.source.retuned.read().clone();
    assert_eq!(
        retuned,
        vec![vec!["target".to_owned()]],
        "the watcher was not told, or was told the old rules: {retuned:?}"
    );
}

/// A tightened rule empties the index of what it now skips — without a walk.
/// Excluding only removes, and the index already holds every path concerned.
#[test]
fn a_tightened_rule_clears_the_index_without_walking() {
    let dir = tempfile::tempdir().expect("temp");
    let index = Arc::new(NativeIndex::open_or_create(dir.path()).expect("index"));
    let source = MemSource::with_rules(vec![
        row("/home/u/keep/a.txt"),
        row("/home/u/skipme"),
        row("/home/u/skipme/b.txt"),
        row("/home/u/skipme/deep/c.txt"),
    ]);
    let engine = Engine::new(
        vec![source.clone()],
        index,
        EngineOptions {
            commit_interval: Duration::from_millis(50),
            ..Default::default()
        },
    );
    let f = Fixture {
        engine,
        source,
        _dir: dir,
    };
    f.engine.rescan(None).expect("rescan");
    settle(&f, |f| f.engine.status().entries == 4);
    let walks = f.source.scans.load(Ordering::Relaxed);

    let mut fresh = (*f.engine.scan_options()).clone();
    fresh.exclude_dirs = vec!["skipme".into()];
    f.engine.set_scan_options(fresh);
    let dropped = f.engine.apply_rules().expect("apply");

    assert_eq!(dropped, 1, "one subtree, not one change per file inside it");
    settle(&f, |f| f.engine.status().entries == 1);
    assert_eq!(
        f.source.scans.load(Ordering::Relaxed),
        walks,
        "the index was brought in line by walking the disk"
    );
    let hits = f
        .engine
        .search("", SortKey::Path, false, Page::new(0, 50))
        .expect("search");
    let paths: Vec<&str> = hits.hits.iter().map(|h| h.path.as_str()).collect();
    assert_eq!(paths, vec!["/home/u/keep/a.txt"]);
}

/// Applying the same rules twice finds nothing the second time — the property that
/// says the removal happened. Many subtrees, because one is what a wrong bound passes.
#[test]
fn applying_the_rules_twice_drops_nothing_the_second_time() {
    let dir = tempfile::tempdir().expect("temp");
    let index = Arc::new(NativeIndex::open_or_create(dir.path()).expect("index"));
    let mut rows = Vec::new();
    for i in 0..120 {
        rows.push(row(&format!("/home/u/p{i}")));
        rows.push(row(&format!("/home/u/p{i}/node_modules")));
        rows.push(row(&format!("/home/u/p{i}/node_modules/x.js")));
        rows.push(row(&format!("/home/u/p{i}/keep.txt")));
    }
    let source = MemSource::with_rules(rows);
    let engine = Engine::new(
        vec![source.clone()],
        index,
        EngineOptions {
            commit_interval: Duration::from_millis(50),
            ..Default::default()
        },
    );
    let f = Fixture {
        engine,
        source,
        _dir: dir,
    };
    f.engine.rescan(None).expect("rescan");
    settle(&f, |f| f.engine.status().entries == 480);

    let mut fresh = (*f.engine.scan_options()).clone();
    fresh.exclude_dirs = vec!["node_modules".into()];
    f.engine.set_scan_options(fresh);

    let first = f.engine.apply_rules().expect("first");
    assert_eq!(first, 120, "one subtree per project");
    let second = f.engine.apply_rules().expect("second");
    assert_eq!(
        second, 0,
        "the first pass reported {first} removals it did not make"
    );
    settle(&f, |f| f.engine.status().entries == 240);
}

/// A source that does not do exclusions keeps its rows: the test is `Option`, so
/// "I have no rules" is not read as "none of my rows are excluded".
#[test]
fn rows_from_a_source_with_no_rules_are_left_alone() {
    let f = fixture(20);
    f.engine.rescan(None).expect("rescan");
    settle(&f, |f| f.engine.status().entries > 0);
    let before = f.engine.status().entries;
    assert!(before > 0);

    let mut fresh = (*f.engine.scan_options()).clone();
    // A rule that would match everything, if this source had rules at all.
    fresh.exclude_paths = vec!["/home".into()];
    f.engine.set_scan_options(fresh);

    assert_eq!(f.engine.apply_rules().expect("apply"), 0);
    assert_eq!(
        f.engine.status().entries,
        before,
        "rows were deleted on the strength of a test the source never answered"
    );
}

/// Asking for the same full walk twice while it waits is one walk: the panel's
/// switches are one save per click.
#[test]
fn a_full_walk_already_waiting_is_not_queued_again() {
    let f = fixture(200);
    // Slow enough that the queue is still being filled while the first runs.
    f.source.slow_ms.store(300, Ordering::Relaxed);
    let before = f.source.scans.load(Ordering::Relaxed);
    for _ in 0..5 {
        f.engine.rescan(None).expect("rescan");
    }
    std::thread::sleep(Duration::from_millis(1200));
    let walks = f.source.scans.load(Ordering::Relaxed) - before;
    assert!(
        walks <= 2,
        "five clicks queued {walks} walks; at most the running one and its successor"
    );
}

/// Setting the rules does not scan: whether the index is brought in line is the
/// caller's to decide, and a walk from a setter is one nothing can opt out of.
#[test]
fn setting_the_rules_is_not_itself_a_scan() {
    let f = fixture(200);
    assert_eq!(f.engine.start_watching().expect("watch"), 1);
    let before = f.source.scans.load(Ordering::Relaxed);

    let mut fresh = (*f.engine.scan_options()).clone();
    fresh.exclude_dirs = vec!["target".into()];
    f.engine.set_scan_options(fresh);
    std::thread::sleep(Duration::from_millis(200));

    assert_eq!(
        f.source.scans.load(Ordering::Relaxed),
        before,
        "changing the rules walked the disk on its own"
    );
}

#[derive(Debug)]
struct NoopWatch {
    order: Arc<RwLock<Vec<&'static str>>>,
    /// What each [`WatchHandle::retune`] was told to skip. Kept rather than counted:
    /// a watcher re-tuned with the old rules would pass a test that counted calls.
    retuned: Arc<RwLock<Vec<Vec<String>>>>,
}

impl WatchHandle for NoopWatch {
    fn cover(&self, _path: &str) {
        self.order.write().push("cover");
    }

    fn retune(&self, opts: &ScanOptions) {
        self.order.write().push("retune");
        self.retuned.write().push(opts.exclude_dirs.clone());
    }

    fn stop(self: Box<Self>) {}
}

/// The message that says "I lost track and cannot say where" reaches the engine.
/// `notify` emits `Rescan { path: "" }` when inotify runs out of watches, and no
/// root is a prefix of the empty string, so owner matching cannot place it.
#[test]
fn a_rescan_that_cannot_say_where_walks_everything() {
    let f = fixture(200);
    assert_eq!(f.engine.start_watching().expect("watch"), 1);
    let before = f.source.scans.load(Ordering::Relaxed);

    f.source.changed(Change::Rescan {
        path: String::new(),
    });

    settle(&f, |f| f.source.scans.load(Ordering::Relaxed) > before);
    assert!(
        f.source.scans.load(Ordering::Relaxed) > before,
        "an empty rescan path must still walk"
    );
    settle(&f, |f| {
        !f.engine.status().scanning && f.engine.status().entries > 0
    });
    assert_eq!(
        f.engine.status().entries,
        f.source.entries.read().len() as u64
    );
}

/// A query the parser could not read comes back saying so. The parser never fails
/// on purpose, so `size:>abc` becomes a text search answering `0 of 0` — which is
/// also what an understood query that matched nothing says.
#[test]
fn a_term_the_parser_could_not_read_is_reported_with_the_answer() {
    let f = fixture(200);
    f.engine.rescan(None).expect("rescan");
    settle(&f, |f| f.engine.status().entries > 0);

    let bad = f
        .engine
        .search("size:>abc", SortKey::Modified, true, Page::new(0, 5))
        .expect("search");
    let terms: Vec<&str> = bad.misread.iter().map(|s| s.term_of("size:>abc")).collect();
    assert_eq!(terms, ["size:>abc"], "name the field that refused it");

    // A warning that is always there is not a warning.
    let good = f
        .engine
        .search("ext:rs", SortKey::Modified, true, Page::new(0, 5))
        .expect("search");
    assert!(good.misread.is_empty(), "{:?}", good.misread);

    // Every shape of answer, because each is an arm somebody can forget.
    let counted = f
        .engine
        .search("kind:zurna", SortKey::Modified, true, Page::new(0, 0))
        .expect("count");
    assert!(!counted.misread.is_empty(), "a count dropped the warning");
    let grouped = f
        .engine
        .facets("kind:zurna", vec![FacetBy::Kind])
        .expect("facets");
    assert!(!grouped.misread.is_empty(), "a facet dropped the warning");
}

/// An index whose *ordering* walk is expensive, and a note of when each one began.
/// Page and preparation costs are controlled separately, so the test below is about
/// speculation: an ordering asks for twenty thousand hits, a page for a screenful.
#[derive(Debug)]
struct Slow {
    inner: Arc<NativeIndex>,
    walk: Duration,
    page: Duration,
    began: Instant,
    /// Milliseconds after `began` at which each ordering walk started.
    walks: RwLock<Vec<u64>>,
}

impl scour_core::Index for Slow {
    fn apply(
        &self,
        changes: &mut dyn Iterator<Item = scour_core::Change>,
    ) -> scour_core::Result<scour_core::ApplyReport> {
        self.inner.apply(changes)
    }
    fn begin_generation(&self) -> scour_core::Result<u64> {
        self.inner.begin_generation()
    }
    fn sweep(
        &self,
        source: SourceId,
        under: &[String],
        generation: u64,
        spare: &scour_core::PrefixSet,
    ) -> scour_core::Result<u64> {
        self.inner.sweep(source, under, generation, spare)
    }
    fn abandon_generation(&self, g: u64) -> scour_core::Result<()> {
        self.inner.abandon_generation(g)
    }
    fn commit(&self) -> scour_core::Result<()> {
        self.inner.commit()
    }
    fn maintain(
        &self,
        level: scour_core::Maintenance,
    ) -> scour_core::Result<scour_core::MaintReport> {
        self.inner.maintain(level)
    }
    fn search(
        &self,
        req: &scour_core::SearchRequest,
    ) -> scour_core::Result<scour_core::SearchResponse> {
        // The engine keeps twenty thousand hits hot; nothing else asks for that many.
        if req.page.limit >= 20_000 {
            self.walks
                .write()
                .push(self.began.elapsed().as_millis() as u64);
            std::thread::sleep(self.walk);
        } else {
            std::thread::sleep(self.page);
        }
        self.inner.search(req)
    }
    fn facets(
        &self,
        req: &scour_core::FacetRequest,
    ) -> scour_core::Result<scour_core::FacetResponse> {
        self.inner.facets(req)
    }
    fn stats(&self) -> scour_core::Result<scour_core::IndexStats> {
        self.inner.stats()
    }
}

#[test]
fn cheap_pages_do_not_trigger_a_larger_preparation() {
    let dir = tempfile::tempdir().expect("temp");
    let slow = Arc::new(Slow {
        inner: Arc::new(NativeIndex::open_or_create(dir.path()).expect("index")),
        walk: Duration::ZERO,
        page: Duration::ZERO,
        began: Instant::now(),
        walks: RwLock::new(Vec::new()),
    });
    let engine = Engine::new(Vec::new(), slow.clone(), EngineOptions::default());
    for offset in [200, 400, 2_000, 10_000] {
        engine
            .search("", SortKey::Modified, true, Page::new(offset, 200))
            .expect("page");
    }
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        slow.walks.read().is_empty(),
        "cheap paging must not warm 20,000 unused hits"
    );
}

#[test]
fn pages_outside_the_prepared_window_do_not_start_a_useless_walk() {
    let dir = tempfile::tempdir().expect("temp");
    let slow = Arc::new(Slow {
        inner: Arc::new(NativeIndex::open_or_create(dir.path()).expect("index")),
        walk: Duration::ZERO,
        page: Duration::from_millis(30),
        began: Instant::now(),
        walks: RwLock::new(Vec::new()),
    });
    let engine = Engine::new(
        Vec::new(),
        Arc::clone(&slow) as Arc<dyn scour_core::Index>,
        EngineOptions::default(),
    );
    for (offset, limit) in [(20_000, 200), (19_999, 200), (u32::MAX, 200), (10, 0)] {
        engine
            .search("", SortKey::Modified, true, Page::new(offset, limit))
            .expect("page");
    }
    // Let a wrongly queued preparation reach the recording index.
    std::thread::sleep(Duration::from_millis(100));
    assert!(
        slow.walks.read().is_empty(),
        "none of these pages can use a prepared window"
    );
}

/// A dear ordering is rebuilt at a share of the machine, not on a clock. Here the
/// walk costs 400 ms, so nothing may ask for another inside four seconds: three
/// seconds of deep pages against a moving index is one walk, not two.
#[test]
fn an_expensive_ordering_is_not_rebuilt_on_a_clock() {
    let dir = tempfile::tempdir().expect("temp");
    let real = Arc::new(NativeIndex::open_or_create(dir.path()).expect("index"));
    let fs = generate(&MockOptions {
        files: 400,
        ..Default::default()
    });
    let source = MemSource::new(fs.entries);
    let slow = Arc::new(Slow {
        inner: Arc::clone(&real),
        walk: Duration::from_millis(400),
        page: Duration::from_millis(30),
        began: Instant::now(),
        walks: RwLock::new(Vec::new()),
    });
    let engine = Engine::new(
        vec![source.clone()],
        Arc::clone(&slow) as Arc<dyn scour_core::Index>,
        EngineOptions {
            commit_interval: Duration::from_millis(50),
            ..Default::default()
        },
    );
    let f = Fixture {
        engine,
        source,
        _dir: dir,
    };
    // Watching gives the source somewhere to send a change, and a moving index
    // refuses every ordering built against the revision before it.
    assert_eq!(f.engine.start_watching().expect("watch"), 1);
    f.engine.rescan(None).expect("rescan");
    settle(&f, |f| f.engine.status().entries > 0);
    settle(&f, |f| !f.engine.status().scanning);
    slow.walks.write().clear();

    // Somebody looking puts the commit clock on `commit_watched`, so the index moves
    // about once a second and every ordering built below is stale within one.
    let stop = std::sync::atomic::AtomicBool::new(false);
    let walks = std::thread::scope(|s| {
        s.spawn(|| {
            let mut seen = 0;
            while !stop.load(Ordering::Relaxed) {
                seen = f
                    .engine
                    .await_change(seen, Duration::from_millis(250))
                    .revision;
            }
        });

        // A page past the first is what wants an ordering; the top one never asks.
        let deep = Page::new(50, 20);
        let from = Instant::now();
        let mut n = 0u64;
        while from.elapsed() < Duration::from_secs(3) {
            n += 1;
            let mut e = f.source.entries.read()[0].clone();
            e.path = format!("/mock/moving-{n}.txt");
            e.id = scour_core::EntryId::path_hash(SourceId(0), &e.path);
            f.source.changed(Change::Upsert(e));
            let _ = f
                .engine
                .search("", SortKey::Modified, true, deep)
                .expect("search");
            std::thread::sleep(Duration::from_millis(100));
        }
        let walks = slow.walks.read().clone();
        stop.store(true, Ordering::Relaxed);
        walks
    });

    assert!(
        f.engine.status().revision > 0,
        "the index has to move for this to be measuring anything"
    );
    assert_eq!(
        walks.len(),
        1,
        "a 400 ms walk buys four seconds of quiet, so three seconds of deep \
         pages must not start a second one. Walks began at {walks:?} ms"
    );
}

/// A source with several roots keeps all of them across repeated walks: the marks
/// saying "seen and unmoved" belong to the pass, so a sweep per root consumes them
/// on the first and empties the rest — `/opt` alternated 5,477 rows and none.
#[test]
fn walking_a_many_rooted_source_twice_keeps_every_root() {
    let dir = tempfile::tempdir().expect("temp");
    let index = Arc::new(NativeIndex::open_or_create(dir.path()).expect("index"));
    let entries: Vec<Entry> = ["/usr", "/etc", "/opt", "/var"]
        .iter()
        .flat_map(|root| (0..50).map(move |i| row(&format!("{root}/thing{i}.txt"))))
        .collect();
    let source = MemSource::many_roots(
        entries,
        ["/usr", "/etc", "/opt", "/var"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
    );
    let f = Fixture {
        engine: Engine::new(
            vec![source.clone()],
            index,
            EngineOptions {
                commit_interval: Duration::from_millis(50),
                ..Default::default()
            },
        ),
        source,
        _dir: dir,
    };

    f.engine.rescan(None).expect("first walk");
    settle(&f, |f| {
        !f.engine.status().scanning && f.engine.status().entries == 200
    });
    assert_eq!(f.engine.status().entries, 200, "four roots, fifty each");

    // Nothing changed on disk. Twice, because the defect alternated by walk.
    for walk in 2..=3 {
        f.engine.rescan(None).expect("walk again");
        settle(&f, |f| !f.engine.status().scanning);
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(
            f.engine.status().entries,
            200,
            "walk {walk} of an unchanged tree lost rows"
        );
        for root in ["/usr", "/etc", "/opt", "/var"] {
            let n = f
                .engine
                .search(
                    &format!("under:{root}"),
                    SortKey::Name,
                    false,
                    Page::new(0, 100),
                )
                .expect("search")
                .total;
            assert_eq!(n, 50, "walk {walk} emptied {root}");
        }
    }
}

/// How far a walk has got is readable *while it walks*: written only at the end, it
/// reads zero for the whole minute a large walk takes, which is indistinguishable
/// from nothing happening.
#[test]
fn a_walk_says_how_far_it_has_got_before_it_finishes() {
    let f = fixture(20_000);
    // Slow enough that there is a middle to look at.
    f.source.slow_ms.store(300, Ordering::Relaxed);
    f.engine.rescan(None).expect("rescan");

    let deadline = Instant::now() + Duration::from_secs(20);
    let mut seen_moving = 0u64;
    while Instant::now() < deadline {
        let st = f.engine.status();
        if !st.scanning && st.entries > 0 {
            break;
        }
        if st.scanning {
            seen_moving = seen_moving.max(st.scanned);
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    settle(&f, |f| {
        !f.engine.status().scanning && f.engine.status().entries > 0
    });
    assert!(
        seen_moving > 0,
        "the walk never said how far it had got until it was over"
    );
    assert_eq!(
        f.engine.status().entries,
        f.source.entries.read().len() as u64,
        "and it still finished"
    );
}

#[test]
fn dropping_an_idle_engine_releases_its_index_and_source() {
    let dir = tempfile::tempdir().expect("index directory");
    let index = Arc::new(NativeIndex::open_or_create(dir.path()).expect("index"));
    let source = MemSource::new(Vec::new());
    let index_weak = Arc::downgrade(&index);
    let source_weak = Arc::downgrade(&source);
    let engine = Engine::new(vec![source], index, EngineOptions::default());
    drop(engine);
    assert!(
        index_weak.upgrade().is_none(),
        "an idle preparer retains the index"
    );
    assert!(
        source_weak.upgrade().is_none(),
        "a stopped engine retains its source"
    );
    NativeIndex::open_or_create(dir.path()).expect("the index lock must be released");
}

#[test]
fn an_unwatched_source_without_a_pulse_converges_after_silent_changes() {
    let dir = tempfile::tempdir().expect("index directory");
    let index = Arc::new(NativeIndex::open_or_create(dir.path()).expect("index"));
    let source = MemSource::unwatchable(vec![row("/home/u/old.txt")]);
    let engine = Engine::new(
        vec![source.clone()],
        index,
        EngineOptions {
            poll_interval: Duration::from_millis(100),
            commit_interval: Duration::from_millis(50),
            commit_idle: Duration::from_millis(50),
            ..Default::default()
        },
    );
    engine.rescan(None).expect("initial scan");
    let f = Fixture {
        engine,
        source,
        _dir: dir,
    };
    settle(&f, |f| count(f, "old.txt") == 1);
    assert_eq!(count(&f, "old.txt"), 1);
    *f.source.entries.write() = vec![row("/home/u/new.txt")];
    settle(&f, |f| count(f, "new.txt") == 1 && count(f, "old.txt") == 0);
    assert_eq!(count(&f, "new.txt"), 1, "a silent create must be found");
    assert_eq!(
        count(&f, "old.txt"),
        0,
        "a silent delete must be reconciled"
    );
}

#[test]
fn shutdown_wakes_a_client_waiting_on_an_unchanged_index() {
    let f = fixture(0);
    std::thread::scope(|scope| {
        let waiter = scope.spawn(|| f.engine.await_change(0, Duration::from_secs(3)));
        std::thread::sleep(Duration::from_millis(50));
        let began = Instant::now();
        f.engine.shutdown();
        waiter.join().expect("waiter");
        assert!(
            began.elapsed() < Duration::from_secs(1),
            "shutdown must wake idle clients"
        );
    });
}

#[test]
fn an_explicit_flush_announces_the_new_revision() {
    let f = fixture(0);
    let before = f.engine.status().revision;
    f.engine.maintain(Maintenance::Flush).expect("flush");
    assert!(f.engine.status().revision > before);
}

#[test]
fn a_failed_sweep_is_abandoned_and_retried_without_another_event() {
    use scour_core::Index;
    let dir = tempfile::tempdir().expect("index directory");
    let index = Arc::new(NativeIndex::open_or_create(dir.path()).expect("index"));
    index
        .apply(&mut [Change::Upsert(row("/home/u/old.txt"))].into_iter())
        .expect("seed");
    index.commit().expect("seed commit");
    let fragile = Arc::new(Fragile {
        inner: index,
        left: AtomicU64::new(u64::MAX),
        refused: AtomicU64::new(0),
        sweep_failures: AtomicU64::new(1),
        abandoned: AtomicU64::new(0),
    });
    let source = MemSource::unwatchable(vec![row("/home/u/new.txt")]);
    let engine = Engine::new(
        vec![source.clone()],
        fragile.clone(),
        EngineOptions {
            commit_interval: Duration::from_millis(50),
            commit_idle: Duration::from_millis(50),
            ..Default::default()
        },
    );
    engine.rescan(None).expect("scan");
    let f = Fixture {
        engine,
        source,
        _dir: dir,
    };
    settle(&f, |f| {
        f.source.scans.load(Ordering::Relaxed) >= 2 && count(f, "old.txt") == 0
    });
    assert!(
        fragile.abandoned.load(Ordering::Relaxed) >= 1,
        "a failed sweep must close its generation"
    );
    assert!(
        f.source.scans.load(Ordering::Relaxed) >= 2,
        "failed reconciliation must retry"
    );
    assert_eq!(count(&f, "old.txt"), 0);
    assert_eq!(count(&f, "new.txt"), 1);
}

#[test]
fn a_prepared_relative_time_query_expires_without_a_file_event() {
    use scour_core::Index;
    let dir = tempfile::tempdir().expect("index directory");
    let index = Arc::new(NativeIndex::open_or_create(dir.path()).expect("index"));
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs() as i64;
    let entries = (0..3).map(|i| {
        let mut e = row(&format!("/home/u/recent-{i}.txt"));
        e.meta.mtime = now - 3600 + 2;
        Change::Upsert(e)
    });
    index.apply(&mut entries.into_iter()).expect("seed");
    index.commit().expect("commit");
    let slow = Arc::new(Slow {
        inner: index,
        walk: Duration::ZERO,
        page: Duration::from_millis(30),
        began: Instant::now(),
        walks: RwLock::new(Vec::new()),
    });
    let engine = Engine::new(Vec::new(), slow, EngineOptions::default());
    let ask = || {
        engine
            .search("dm:1h", SortKey::Modified, true, Page::new(1, 1))
            .expect("search")
    };
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut cached = false;
    while Instant::now() < deadline {
        let page = ask();
        if page.rows_built == 0 && page.hits.len() == 1 {
            cached = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(
        cached,
        "the relative query must reach the prepared path first"
    );
    std::thread::sleep(Duration::from_secs(3));
    assert!(
        ask().hits.is_empty(),
        "the cached cutoff must move with the clock"
    );
}
