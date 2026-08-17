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

/// A source backed by a list that a test can change under the engine's feet.
///
/// The point of the exercise: the engine is handed this and has no way of
/// telling it from a filesystem. If anything in `scour-engine` ever needed to
/// know, this would not compile.
#[derive(Debug)]
struct MemSource {
    entries: RwLock<Vec<Entry>>,
    scans: AtomicU64,
    watchable: bool,
    /// Where the engine asked to be told about changes.
    sink: RwLock<Option<Box<dyn ChangeSink>>>,
    /// Pretend the roots are not there — an unmounted volume, a pulled drive.
    offline: std::sync::atomic::AtomicBool,
    /// Pretend the walk stopped partway: some of the tree reached the sink and
    /// the rest never will.
    cancelled: std::sync::atomic::AtomicBool,
    /// Make the walk take this long, so a change can arrive during one.
    slow_ms: AtomicU64,
    /// Every `cover` and every `scan`, in the order they happened.
    ///
    /// The order is the thing being tested and nothing else can see it: a walk
    /// is a snapshot and a watch is everything after it, so anything that
    /// happens between them is covered by whichever came second — and by
    /// neither if the walk did.
    order: Arc<RwLock<Vec<&'static str>>>,
    /// What every `retune` this source's watch was given said to skip.
    retuned: Arc<RwLock<Vec<Vec<String>>>>,
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
            retuned: Arc::new(RwLock::new(Vec::new())),
        })
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
            retuned: Arc::new(RwLock::new(Vec::new())),
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
            roots: vec!["/home/u".into()],
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

    fn scan(&self, opts: &ScanOptions, sink: &mut dyn EntrySink) -> scour_core::Result<ScanReport> {
        self.scans.fetch_add(1, Ordering::Relaxed);
        // A walk that takes a while, so something can happen during it.
        let slow = self.slow_ms.load(Ordering::Relaxed);
        if slow > 0 {
            std::thread::sleep(Duration::from_millis(slow));
        }
        // A walk that stopped partway: some of the tree reached the sink and
        // the rest never will. The report has to say so, because everything
        // downstream reconciles on the assumption that it saw everything.
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
        // Every walk, not only a subtree's: the start-up ordering test is about
        // the whole-source one, and it is the bigger window of the two.
        self.order.write().push("scan");
        // A root that is not there yet: nothing found, and the report says the
        // walk could not look rather than that there was nothing to find.
        if self.offline.load(Ordering::Relaxed) {
            // Nothing vouched for: the walk could not look, so nothing may
            // be reconciled against it.
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
                None => vec!["/home/u".into()],
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
        // **Installing a watch takes time, and the test is about that time.**
        // On this machine a recursive watch over a home directory is 342,000
        // inotify watches installed one at a time — fifteen seconds — while
        // `rescan` only queues a job and returns at once. A source that watches
        // instantly cannot tell a correct ordering from a lucky one: with the
        // two calls deliberately swapped, the ordering test still passed.
        std::thread::sleep(Duration::from_millis(150));
        // Recorded once it is actually in place, which is what the ordering is
        // about — not when it was asked for.
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

/// An index that stops accepting rows partway through.
///
/// **Because a walk that found files the index could not take is a walk whose
/// evidence is incomplete**, and the sweep that follows judges by exactly that
/// evidence: rows the walk *did* find and the index does *not* have look, from
/// the sweep's side, like files that are gone. Deleting them is the failure the
/// guard exists for and it had no test — checked by removing it, which the
/// suite did not notice.
///
/// A disk that fills partway through is what this stands in for. Everything
/// else is passed through, so the engine sees a real index behaving normally
/// right up to the moment it does not.
#[derive(Debug)]
struct Fragile {
    inner: Arc<NativeIndex>,
    /// Applies to refuse. Counted down; zero refuses for ever after.
    left: AtomicU64,
    refused: AtomicU64,
}

impl scour_core::Index for Fragile {
    fn apply(
        &self,
        changes: &mut dyn Iterator<Item = scour_core::Change>,
    ) -> scour_core::Result<scour_core::ApplyReport> {
        if self.left.load(Ordering::Relaxed) == 0 {
            self.refused.fetch_add(1, Ordering::Relaxed);
            // Drained, because a caller that hands over an iterator has handed
            // it over whether or not this works.
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
        under: &str,
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
    let dir = tempfile::tempdir().expect("temp");
    let index = Arc::new(NativeIndex::open_or_create(dir.path()).expect("index"));
    let fs = generate(&MockOptions {
        files,
        ..Default::default()
    });
    let source = MemSource::new(fs.entries);
    let engine = Engine::new(
        vec![source.clone()],
        index,
        EngineOptions {
            commit_interval: Duration::from_millis(50),
            ..Default::default()
        },
    );
    Fixture {
        engine,
        source,
        _dir: dir,
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
    // The property that makes a rescan a reconciliation rather than an
    // accumulation: a scan reports what it found, and everything it did not
    // find has to stop being in the index.
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
    // The same reading, cut into pieces a search box can colour. The spans
    // cover the query byte for byte, so a frontend can rebuild it from them.
    let rebuilt: String = e.spans.iter().map(|s| s.of("rapor ext:pdf")).collect();
    assert_eq!(rebuilt, "rapor ext:pdf");
    assert!(
        e.spans.iter().all(|s| !s.role.is_warning()),
        "nothing in a correct query is a mistake"
    );
    assert!(e.completions.is_empty(), "no caret, no completions");
    // The forgiving parser is exactly why this exists: a field name that does
    // not exist is searched for as text, quietly and reasonably, and this is
    // how a caller finds that out. (Case is not the mistake — field names are
    // folded, so `sizE:` really is `size:`.)
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
    // `unsorted` means "rows outside the largest segment", so on this engine a
    // scan small enough to land in one segment has none — the tail is a
    // property of how many segments there are, not of how recent the rows are.
    // The tantivy engine this test was written against counted every
    // uncommitted document instead, which is why the assertion here used to be
    // the opposite.
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
    // Caps says so in advance; a cloud bucket has no change feed and is
    // reconciled by rescanning instead.
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
    // What a live list rests on: the client hands back the revision it is
    // showing and hears nothing until that is no longer what the index would
    // answer.
    let f = fixture(200);
    assert_eq!(f.engine.start_watching().expect("watch"), 1);
    f.engine.rescan(None).expect("rescan");
    settle(&f, |f| {
        !f.engine.status().scanning && f.engine.status().entries > 0
    });
    let seen = f.engine.status().revision;

    // Nothing is happening, so this costs the whole timeout and comes back
    // saying so. A client that treats the unchanged number as "nothing to do"
    // then simply asks again.
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

    // And now something does. The wait is on another thread because the point
    // is that it is *asleep* until the change lands, not that it polls.
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
    // The bound the whole feature is about: a file created while somebody is
    // watching is announced on the burst clock, not after `commit_idle`.
    assert!(
        took < Duration::from_secs(5),
        "waited {took:?} for a change a watched index should announce in about a second"
    );
    assert_eq!(count(&f, "canli"), 1, "and it is findable when announced");
}

#[test]
fn a_watcher_that_lost_track_causes_a_walk_rather_than_a_guess() {
    // Every platform loses track differently — inotify out of watches, a
    // Windows buffer overflow, a missed cloud poll — and all of them say the
    // same thing here.
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

#[test]
fn a_volume_that_was_not_mounted_yet_is_picked_up_without_being_asked() {
    // The service starts with the session, and the session starts before the
    // volumes: `/mnt/depo` is a readable, empty directory until something
    // mounts it. So the first walk of an external disk finds nothing.
    //
    // Two things have to be true, and only the first of them was.
    //
    // Refusing to reconcile is right — sweeping on a walk that could not look
    // deletes everything the index held for that volume. But refusing and then
    // never asking again means the disk stays as stale as it was until a person
    // types `scour rescan`, which is the thing nobody remembers to do.
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

/// Nothing is walked until everything is watched.
///
/// The start-up version of the test below, and the one that was missing. That
/// one covers a subtree a watcher discovers; this covers the first walk of
/// every source, which is the biggest window there is — on this machine it is
/// seven seconds during which a compile, a download or a `git checkout` is
/// perfectly likely.
///
/// It was live-checked before this existed, on the real service: 300 files
/// created into an already-walked and already-swept directory while a second
/// source was still being walked, and all 300 arrived — about thirteen seconds
/// later, by the queued events being replayed after the sweep. What this test
/// holds is the ordering that makes that true.
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

/// The same call, told not to walk, still watches.
///
/// `scan.on_start = false` is a real configuration — a machine that trusts its
/// watcher across restarts — and the ordering call must not make watching
/// conditional on walking.
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

/// What waited out a walk does not then wait out the clock as well.
///
/// A walk does not drain the change channel — deliberately, because the walk's
/// snapshot and the event stream have to stay ordered — so a file created while
/// one is running waits for it. That part is the design. What was not is being
/// charged twice: `commit_idle` is a staleness bound, and it was measured from
/// the commit rather than from the change, so a row already unwritten for the
/// whole walk waited the full bound again from the moment it landed.
///
/// Measured on the real service before this test existed: 7.6 and 13.3 seconds
/// from the walk ending to the rows being findable, all of it this wait,
/// against 0.00 with the fix.
#[test]
fn a_change_that_waited_out_a_walk_is_written_at_once() {
    let f = fixture(200);
    assert_eq!(f.engine.start_watching().expect("watch"), 1);
    // Nobody is looking, so the slow clock applies — and it is the one this is
    // about. Long enough that waiting it out would be unmistakable.
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
    // The walk has ended. With the clock measured from the commit this row
    // would wait `commit_idle` — fifteen seconds by default — from here.
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

/// A walk that stopped partway is not evidence either.
///
/// The same failure from the other side, and it also had no test. A cancelled
/// walk reached some of the tree and none of the rest; sweeping on it deletes
/// everything it never got to. What makes this worth its own test rather than
/// an argument is that the two guards are separate conditions on one line, and
/// removing either one leaves the other looking like it covers the case.
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

/// A walk the index could not take is not evidence, so nothing is swept on it.
///
/// **The guard whose failure is losing files, and it had no test.** A sweep
/// deletes what the walk did not stamp. If the index refused a batch — a full
/// disk, a write error — those rows exist on the filesystem and are missing
/// from the index, which from the sweep's side is indistinguishable from files
/// that were deleted. It would then remove them, and report success.
///
/// Checked by removing the guard: with `trustworthy` forced true this test
/// fails and nothing else in the suite notices.
#[test]
fn a_batch_the_index_refused_stops_the_sweep() {
    let dir = tempfile::tempdir().expect("temp");
    let real = Arc::new(NativeIndex::open_or_create(dir.path()).expect("index"));
    let fs = generate(&MockOptions {
        files: 400,
        ..Default::default()
    });
    let source = MemSource::new(fs.entries);
    // One walk lands whole; the next is refused partway. That order matters —
    // there has to be something in the index worth losing.
    let fragile = Arc::new(Fragile {
        inner: Arc::clone(&real),
        left: AtomicU64::new(u64::MAX),
        refused: AtomicU64::new(0),
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
    // A walk is a snapshot; a watch is everything after it. Between them there
    // must be no gap, and the obvious order — walk it, then watch it, because
    // now we know it is real — leaves exactly one: whatever is created while
    // the walk is running is reported by nothing and found by nothing.
    //
    // That is the same race a walk exists to close, moved rather than removed,
    // and it was measured on the live index before this test existed. Five
    // thousand files written into two hundred fresh directories left **1,260 of
    // them missing** — and not scattered: packages 32 to 82, one unbroken run,
    // which is the window in which the shell loop writing them was fastest.
    //
    // Watching something that is about to be walked costs a duplicate upsert
    // at worst, and an upsert is by identity.
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

/// New rules reach the walk *and* the watchers.
///
/// **Both halves, and the second is the one that was missing.** The rules used
/// to be read once at start-up, which was true for as long as the only way to
/// write one was a file. A window can write one now, and a rule that only the
/// walk hears about is worse than no rule at all: the scan sweeps `target`
/// clean, the next build writes two million files, and the watcher — still
/// filtering by what it was handed at start-up — puts every one of them back.
/// That is the exact failure the rule exists to prevent.
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

/// Setting the rules does not scan, and that is deliberate.
///
/// Whether the index should be brought in line is the caller's to decide —
/// `scourd` scans because a person just asked for this, and a walk that
/// happened as a side effect of a setter would be one nothing could opt out of.
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
    /// What each [`WatchHandle::retune`] was told to skip.
    ///
    /// Kept rather than counted, because "it was told something" is not the
    /// property that matters: a watcher told to re-tune with the *old* rules
    /// re-tunes to nothing, and would pass a test that only counted the calls.
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

/// The message that says "I lost track and cannot say where" used to be the
/// one message dropped.
///
/// `notify` emits `Rescan { path: "" }` when inotify runs out of watches or a
/// kernel buffer overflows — precisely when the index has started drifting and
/// nothing else will say so. Matching it against the sources' roots found no
/// owner, because no root is a prefix of the empty string, so it was discarded
/// and the drift continued until someone rescanned by hand.
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

/// A query the parser could not read comes back saying so.
///
/// **The failure this guards looks exactly like success.** The parser never
/// fails on purpose — a half-typed query has to stay usable — so `size:>abc`
/// is not a size, the whole term becomes a search for that text, and the
/// answer is `0 of 0`. That is also what a query which *was* understood and
/// matched nothing says, and nothing anywhere told the two apart. A window
/// colours the term while it is being typed; a command line and a model get
/// one answer and do not know to ask a second question.
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

    // The other half, and the half that makes the warning worth anything: one
    // that is always there is not a warning.
    let good = f
        .engine
        .search("ext:rs", SortKey::Modified, true, Page::new(0, 5))
        .expect("search");
    assert!(good.misread.is_empty(), "{:?}", good.misread);

    // Every shape of answer, because each is an arm somebody can forget: a
    // count is a search for no rows, and a facet is a grouping of the same
    // wrong set one level up.
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

/// An index whose *ordering* walk is expensive, and a note of when each one
/// began.
///
/// Only the walk is slowed. Ordinary pages go straight through, which is what
/// makes the test below about the speculation and nothing else: the engine
/// asks for twenty thousand hits when it is building an ordering and for a
/// screenful when it is answering a question.
#[derive(Debug)]
struct Slow {
    inner: Arc<NativeIndex>,
    walk: Duration,
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
        under: &str,
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
        // The engine keeps twenty thousand hits hot; nothing else asks for a
        // page that size.
        if req.page.limit >= 20_000 {
            self.walks
                .write()
                .push(self.began.elapsed().as_millis() as u64);
            std::thread::sleep(self.walk);
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

/// **A dear ordering is rebuilt at a share of the machine, not on a clock.**
///
/// The interval used to be a flat two seconds, justified by a walk costing
/// "about a tenth of a second" — which is true of the stored order and of
/// nothing else; the same file measures 2,463.6 ms for one window sorted by
/// path, and a walk of twenty thousand is dearer again. The walk is also
/// discarded whenever the index moves, and a machine with a window open on it
/// moves about once a second. So the expensive case was the one where every
/// walk was both dear and thrown away, repeated for as long as the window
/// stayed open.
///
/// Here the walk costs 400 ms, so nothing may ask for another inside four
/// seconds. Three seconds of deep pages against a moving index is one walk
/// where the fixed interval gave two.
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
    // Watching, because that is what gives the source somewhere to send a
    // change — and a moving index is the whole point: an ordering built
    // against one revision is refused by the next, so every walk below is one
    // that answers nothing.
    assert_eq!(f.engine.start_watching().expect("watch"), 1);
    f.engine.rescan(None).expect("rescan");
    settle(&f, |f| f.engine.status().entries > 0);
    settle(&f, |f| !f.engine.status().scanning);
    slow.walks.write().clear();

    // **Somebody is looking.** That is not decoration either: it is what puts
    // the commit clock on `commit_watched` instead of batching for fifteen
    // seconds, so the index moves about once a second and every ordering built
    // below is stale within one. It is also the only state in which any of
    // this is a problem — a machine nobody is watching commits rarely enough
    // that a walk survives.
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

        // A page past the first is what wants an ordering; a page at the top of
        // its results never asks for one.
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
