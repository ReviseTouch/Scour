//! The engine, wired to a real index and a source it cannot identify.

use std::io::Read;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use parking_lot::RwLock;
use scour_core::{
    Caps, Change, ChangeSink, Entry, EntryId, EntrySink, Error, Maintenance, Page, ScanOptions,
    ScanReport, SortKey, Source, SourceId, SourceInfo, SourceKind, WatchHandle,
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
}

impl MemSource {
    fn new(entries: Vec<Entry>) -> Arc<MemSource> {
        Arc::new(MemSource {
            entries: RwLock::new(entries),
            scans: AtomicU64::new(0),
            watchable: true,
            sink: RwLock::new(None),
        })
    }

    fn unwatchable(entries: Vec<Entry>) -> Arc<MemSource> {
        Arc::new(MemSource {
            entries: RwLock::new(entries),
            scans: AtomicU64::new(0),
            watchable: false,
            sink: RwLock::new(None),
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
        let mut c = Caps::STABLE_IDS;
        if self.watchable {
            c |= Caps::WATCH;
        }
        c
    }

    fn scan(&self, opts: &ScanOptions, sink: &mut dyn EntrySink) -> scour_core::Result<ScanReport> {
        self.scans.fetch_add(1, Ordering::Relaxed);
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
        *self.sink.write() = Some(s);
        Ok(Box::new(NoopWatch))
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

#[derive(Debug)]
struct NoopWatch;

impl WatchHandle for NoopWatch {
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
