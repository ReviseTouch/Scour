//! The filesystem source, against a real filesystem.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use scour_core::{Caps, Change, ChangeSink, Entry, EntrySink, Flow, ScanOptions, Source, SourceId};
use scour_source_fs::FsSource;

/// A small tree, real on disk.
fn tree() -> (tempfile::TempDir, FsSource) {
    let dir = tempfile::tempdir().expect("temp dir");
    let r = dir.path();
    for d in [
        "src",
        "src/deep",
        "node_modules",
        "node_modules/pkg",
        "keep",
    ] {
        std::fs::create_dir_all(r.join(d)).expect("mkdir");
    }
    for (p, body) in [
        ("src/main.rs", "fn main() {}"),
        ("src/deep/RAPOR.pdf", "x"),
        ("node_modules/pkg/index.js", "//"),
        ("keep/notlar.txt", "merhaba"),
        ("README.md", "# hi"),
    ] {
        std::fs::write(r.join(p), body).expect("write");
    }
    let src = FsSource::new(SourceId(1), "test", vec![r.to_path_buf()]);
    (dir, src)
}

#[derive(Default)]
struct Collect {
    paths: Vec<String>,
    stop_after: usize,
}

impl EntrySink for Collect {
    fn push(&mut self, e: Entry) -> Flow {
        self.paths.push(e.path);
        if self.stop_after > 0 && self.paths.len() >= self.stop_after {
            Flow::Stop
        } else {
            Flow::Continue
        }
    }
}

fn scan(src: &FsSource, opts: &ScanOptions) -> (Vec<String>, scour_core::ScanReport) {
    let mut sink = Collect::default();
    let report = src.scan(opts, &mut sink).expect("scan");
    let mut paths = sink.paths;
    paths.sort();
    (paths, report)
}

/// Paths relative to the root, so assertions do not depend on the temp dir.
fn rel(paths: &[String], root: &std::path::Path) -> Vec<String> {
    let prefix = format!("{}/", root.to_string_lossy().replace('\\', "/"));
    let mut v: Vec<String> = paths
        .iter()
        .filter_map(|p| p.strip_prefix(&prefix).map(str::to_owned))
        .collect();
    v.sort();
    v
}

#[test]
fn a_scan_finds_everything_including_directories() {
    let (dir, src) = tree();
    let (paths, report) = scan(&src, &ScanOptions::default());
    assert_eq!(
        rel(&paths, dir.path()),
        vec![
            "README.md",
            "keep",
            "keep/notlar.txt",
            "node_modules",
            "node_modules/pkg",
            "node_modules/pkg/index.js",
            "src",
            "src/deep",
            "src/deep/RAPOR.pdf",
            "src/main.rs",
        ]
    );
    // The root itself is an entry too.
    assert_eq!(report.entries, 11);
    assert_eq!(report.dirs, 6);
    assert!(!report.cancelled);
}

#[test]
fn metadata_arrives_with_the_entry_unless_it_was_skipped() {
    let (_dir, src) = tree();
    let mut sink = Collect::default();
    let mut with = Vec::new();
    struct Keep(Vec<Entry>);
    impl EntrySink for Keep {
        fn push(&mut self, e: Entry) -> Flow {
            self.0.push(e);
            Flow::Continue
        }
    }
    let mut keep = Keep(Vec::new());
    src.scan(&ScanOptions::default(), &mut keep).expect("scan");
    let main = keep
        .0
        .iter()
        .find(|e| e.name() == "main.rs")
        .expect("main.rs");
    assert_eq!(main.meta.size, 12);
    assert!(main.meta.mtime > 0);
    with.push(main.path.clone());

    // A stat-less scan trades those fields for speed, and says nothing false:
    // the metadata is UNKNOWN rather than zeroed-and-plausible.
    let mut fast = Keep(Vec::new());
    src.scan(
        &ScanOptions {
            skip_metadata: true,
            ..Default::default()
        },
        &mut fast,
    )
    .expect("scan");
    let main = fast
        .0
        .iter()
        .find(|e| e.name() == "main.rs")
        .expect("main.rs");
    assert_eq!(main.meta, scour_core::Meta::UNKNOWN);
    assert!(
        !main.is_dir,
        "the type is known from the directory read alone"
    );
    let _ = &mut sink;
}

#[test]
fn exclusions_prune_the_walk() {
    let (dir, src) = tree();
    let (paths, report) = scan(
        &src,
        &ScanOptions {
            exclude_dirs: vec!["node_modules".into()],
            exclude_files: vec!["README.md".into()],
            ..Default::default()
        },
    );
    let rel = rel(&paths, dir.path());
    assert!(!rel.iter().any(|p| p.contains("node_modules")), "{rel:?}");
    assert!(!rel.contains(&"README.md".to_owned()));
    assert!(rel.contains(&"src/main.rs".to_owned()));
    assert!(report.excluded >= 2);
}

#[test]
fn an_allow_rule_reaches_inside_an_excluded_tree() {
    // The rule only means anything if the walk descends far enough to apply it.
    let (dir, src) = tree();
    let root = dir.path().to_string_lossy().replace('\\', "/");
    let (paths, _) = scan(
        &src,
        &ScanOptions {
            exclude_paths: vec![format!("{root}/node_modules")],
            allow: vec![format!("{root}/node_modules/pkg")],
            ..Default::default()
        },
    );
    let rel = rel(&paths, dir.path());
    assert!(
        rel.contains(&"node_modules/pkg/index.js".to_owned()),
        "{rel:?}"
    );
    assert!(!rel.contains(&"node_modules".to_owned()));
}

#[test]
fn a_sink_can_stop_a_walk_partway() {
    let (_dir, src) = tree();
    let mut sink = Collect {
        stop_after: 3,
        ..Default::default()
    };
    let report = src.scan(&ScanOptions::default(), &mut sink).expect("scan");
    assert!(sink.paths.len() >= 3);
    assert!(report.cancelled);
    assert!(report.entries < 11, "the walk should not have finished");
}

#[test]
fn scanning_a_subtree_ignores_the_configured_roots() {
    let (dir, src) = tree();
    let sub = format!("{}/src", dir.path().to_string_lossy().replace('\\', "/"));
    let (paths, _) = scan(
        &src,
        &ScanOptions {
            subtree: Some(sub),
            ..Default::default()
        },
    );
    let rel = rel(&paths, dir.path());
    // The subtree root is an entry too, exactly as a full scan includes its
    // roots — otherwise a rescan of a directory would quietly forget the
    // directory.
    assert_eq!(
        rel,
        vec!["src", "src/deep", "src/deep/RAPOR.pdf", "src/main.rs"]
    );
}

#[test]
fn stat_answers_for_one_path() {
    let (dir, src) = tree();
    let p = format!(
        "{}/README.md",
        dir.path().to_string_lossy().replace('\\', "/")
    );
    let e = src.stat(&p).expect("stat");
    assert_eq!(e.name(), "README.md");
    assert_eq!(e.meta.size, 4);
    assert!(!e.is_dir);
    assert_eq!(
        src.stat("/definitely/not/here").unwrap_err().code(),
        "not_found"
    );
}

#[test]
fn capabilities_describe_this_platform_honestly() {
    let (_dir, src) = tree();
    let caps = src.caps();
    assert!(caps.contains(Caps::WATCH));
    assert!(caps.contains(Caps::CONTENT));
    #[cfg(unix)]
    {
        assert!(
            caps.contains(Caps::STABLE_IDS),
            "an inode survives a rename"
        );
        assert!(
            !caps.contains(Caps::RECURSIVE_WATCH),
            "inotify is one watch per directory"
        );
    }
    #[cfg(any(windows, target_os = "macos"))]
    assert!(caps.contains(Caps::RECURSIVE_WATCH));
    // Nothing here has a durable journal yet; that is what an $MFT or FSEvents
    // history source would add.
    assert!(!caps.contains(Caps::JOURNAL));
    assert_eq!(src.describe().roots.len(), 1);
}

#[derive(Debug, Default)]
struct Seen(Mutex<Vec<Change>>);

impl ChangeSink for Seen {
    fn emit(&self, c: Change) {
        if let Ok(mut v) = self.0.lock() {
            v.push(c);
        }
    }
}

/// Wait for a predicate to hold, or give up. Filesystem notifications are
/// asynchronous on every platform and coalesced on some; a fixed sleep is
/// either flaky or slow.
fn wait_for(seen: &Arc<Seen>, what: impl Fn(&[Change]) -> bool) -> Vec<Change> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        {
            let v = seen.0.lock().expect("lock");
            if what(&v) {
                return v.clone();
            }
        }
        if Instant::now() > deadline {
            return seen.0.lock().expect("lock").clone();
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn a_watch_reports_creations_and_removals() {
    let (dir, src) = tree();
    let seen = Arc::new(Seen::default());
    struct Fwd(Arc<Seen>);
    impl std::fmt::Debug for Fwd {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Fwd")
        }
    }
    impl ChangeSink for Fwd {
        fn emit(&self, c: Change) {
            self.0.emit(c);
        }
    }
    let handle = src.watch(Box::new(Fwd(Arc::clone(&seen)))).expect("watch");

    let created: PathBuf = dir.path().join("src/yeni.rs");
    std::fs::write(&created, "fn x() {}").expect("write");
    let want = created.to_string_lossy().replace('\\', "/");
    let changes = wait_for(&seen, |v| {
        v.iter()
            .any(|c| matches!(c, Change::Upsert(e) if e.path == want))
    });
    assert!(
        changes
            .iter()
            .any(|c| matches!(c, Change::Upsert(e) if e.path == want)),
        "a new file should be reported: {changes:?}"
    );

    std::fs::remove_file(&created).expect("remove");
    let changes = wait_for(&seen, |v| {
        v.iter()
            .any(|c| matches!(c, Change::RemoveSubtree { path } if *path == want))
    });
    assert!(
        changes
            .iter()
            .any(|c| matches!(c, Change::RemoveSubtree { path } if *path == want)),
        "a removal should be reported: {changes:?}"
    );
    handle.stop();
}

#[test]
fn identities_are_stable_across_two_scans() {
    // Whatever a platform can offer, it has to offer the same answer twice, or
    // every rescan would look like a complete replacement of the index.
    let (_dir, src) = tree();
    struct Ids(Vec<(String, scour_core::EntryId)>);
    impl EntrySink for Ids {
        fn push(&mut self, e: Entry) -> Flow {
            self.0.push((e.path, e.id));
            Flow::Continue
        }
    }
    let run = || {
        let mut s = Ids(Vec::new());
        src.scan(&ScanOptions::default(), &mut s).expect("scan");
        s.0.into_iter()
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    assert_eq!(run(), run());
    let ids: HashSet<_> = run().into_values().collect();
    assert_eq!(ids.len(), 11, "two entries must never share an id");
}

#[test]
fn a_source_told_not_to_watch_says_it_cannot() {
    // `source.watch` had been in the configuration since it was written and
    // nothing read it — every source was watched regardless. The engine asks
    // `caps()`, so this is where the answer belongs.
    let s = FsSource::new(SourceId(0), "t", vec!["/tmp".into()]);
    assert!(s.caps().contains(Caps::WATCH), "watching by default");
    let s = s.with_watch(false);
    assert!(!s.caps().contains(Caps::WATCH));
    assert!(!s.caps().contains(Caps::RECURSIVE_WATCH));
    // And the rest of what it can do is unchanged.
    assert!(s.caps().contains(Caps::CONTENT));
}

#[test]
fn a_root_that_cannot_be_read_is_reported_as_such() {
    // The difference between "found nothing" and "could not look". The engine
    // reconciles on a scan report, and reconciling the second deletes the whole
    // index for that source — measured at five entries becoming zero.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let real = tmp.path().join("here");
    std::fs::create_dir(&real).expect("mkdir");
    std::fs::write(real.join("a.txt"), b"x").expect("write");

    let good = FsSource::new(SourceId(0), "t", vec![real.clone()]);
    let mut sink = Collect::default();
    let r = good.scan(&ScanOptions::default(), &mut sink).expect("scan");
    assert!(!r.root_unreadable);
    assert!(r.entries > 0);

    let gone = FsSource::new(SourceId(0), "t", vec![tmp.path().join("nowhere")]);
    let mut sink = Collect::default();
    let r = gone.scan(&ScanOptions::default(), &mut sink).expect("scan");
    assert!(
        r.root_unreadable,
        "a root that is not there has to be distinguishable from an empty one"
    );
    assert_eq!(r.entries, 0);
}
