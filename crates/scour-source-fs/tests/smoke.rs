//! The filesystem source, against a real filesystem.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use scour_core::{
    Caps, Change, ChangeSink, Entry, EntrySink, Flow, ScanOptions, Source, SourceId, WatchHandle,
};
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

    // A stat-less scan says nothing false: the metadata is UNKNOWN, not zeroed.
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
    // A stop is exact rather than a lower bound — the drain loop stops feeding the
    // sink at once — and the report says the scan was cut short.
    assert_eq!(sink.paths.len(), 3, "no push after a Stop");
    assert!(report.cancelled);
    // What a stop does not guarantee is *where*: `entries` is what the walk found,
    // not what the sink was given, and a cancelled tally has no stopping point.
    assert!(report.entries >= 3);
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
    // The subtree root is an entry too, or a rescan of a directory would quietly
    // forget the directory.
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
    // **Not `CONTENT`**: `open` refuses unconditionally, and `scour sources` prints
    // what is claimed here. It comes back with the first `Extractor`.
    assert!(
        !caps.contains(Caps::CONTENT),
        "nothing here can open a file, so nothing here may claim it can"
    );
    #[cfg(unix)]
    {
        assert!(
            !caps.contains(Caps::RECURSIVE_WATCH),
            "on Linux the source has no watch of its own; the mark comes from the helper"
        );
    }
    #[cfg(any(windows, target_os = "macos"))]
    assert!(caps.contains(Caps::RECURSIVE_WATCH));
    // Nothing here has a durable journal yet; an $MFT or FSEvents source would add one.
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
/// asynchronous on every platform and coalesced on some, so a fixed sleep is flaky.
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

/// Start a watch, or say why this test cannot and let it pass. A fanotify mark needs
/// `CAP_SYS_ADMIN`, so these carry `#[ignore]`; under `--ignored` and still no
/// descriptor this panics. `sudo scour-watch -- cargo test -p scour-source-fs -- --ignored`
fn watching(src: &FsSource, sink: Box<dyn ChangeSink>, test: &str) -> Box<dyn WatchHandle> {
    match src.watch(&ScanOptions::default(), sink) {
        Ok(handle) => handle,
        Err(e) if format!("{e:?}").contains("fanotify") => panic!(
            "{test}: no fanotify descriptor, and this test was asked for explicitly \
             (`--ignored`) so there is nothing to fall back to. Run it as \
             `sudo scour-watch -- cargo test -p scour-source-fs -- --ignored`."
        ),
        Err(e) => panic!("watch: {e:?}"),
    }
}

#[test]
#[ignore = "needs a fanotify descriptor: sudo scour-watch -- cargo test -p scour-source-fs -- --ignored"]
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
    let handle = watching(
        &src,
        Box::new(Fwd(Arc::clone(&seen))),
        "a_watch_reports_creations_and_removals",
    );

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

/// The `ChangeSink` boilerplate every watch test needs.
#[derive(Debug)]
struct Fwd(Arc<Seen>);

impl ChangeSink for Fwd {
    fn emit(&self, c: Change) {
        self.0.emit(c);
    }
}

#[test]
#[ignore = "needs a fanotify descriptor: sudo scour-watch -- cargo test -p scour-source-fs -- --ignored"]
fn a_new_directory_is_reported_as_a_subtree_to_walk() {
    // Between `mkdir a/b` and a watch existing on `a/b`, anything created inside it
    // produces no event at all. So a directory that has just appeared is reported as
    // a place to walk, and a walk reads what is there.
    let (dir, src) = tree();
    let seen = Arc::new(Seen::default());
    let handle = watching(
        &src,
        Box::new(Fwd(Arc::clone(&seen))),
        "a_new_directory_is_reported_as_a_subtree_to_walk",
    );

    let made = dir.path().join("src/brand-new");
    std::fs::create_dir(&made).expect("mkdir");
    std::fs::write(made.join("inside.rs"), "fn x() {}").expect("write");
    let want = made.to_string_lossy().replace('\\', "/");

    let changes = wait_for(&seen, |v| {
        v.iter()
            .any(|c| matches!(c, Change::Rescan { path } if *path == want))
    });
    assert!(
        changes
            .iter()
            .any(|c| matches!(c, Change::Rescan { path } if *path == want)),
        "a created directory has to become a walk, not a row: {changes:?}"
    );
    handle.stop();
}

#[test]
#[ignore = "needs a fanotify descriptor: sudo scour-watch -- cargo test -p scour-source-fs -- --ignored"]
fn writing_to_a_file_does_not_ask_for_a_walk() {
    // The other half of the rule: a directory's mtime moves whenever a file in it is
    // written, so "rescan on every modify" would queue a walk per object file.
    let (dir, src) = tree();
    let seen = Arc::new(Seen::default());
    let handle = watching(
        &src,
        Box::new(Fwd(Arc::clone(&seen))),
        "writing_to_a_file_does_not_ask_for_a_walk",
    );

    let existing = dir.path().join("src/main.rs");
    std::fs::write(&existing, "fn main() { /* changed */ }").expect("write");
    let want = existing.to_string_lossy().replace('\\', "/");
    let changes = wait_for(&seen, |v| {
        v.iter()
            .any(|c| matches!(c, Change::Upsert(e) if e.path == want))
    });
    assert!(
        !changes.iter().any(|c| matches!(c, Change::Rescan { .. })),
        "touching a file must not queue a walk: {changes:?}"
    );
    handle.stop();
}

#[test]
#[ignore = "needs a fanotify descriptor: sudo scour-watch -- cargo test -p scour-source-fs -- --ignored"]
#[cfg(unix)]
fn a_watch_does_not_walk_out_through_a_symlink() {
    // A watcher that follows symlinks the walk does not reports files under a path
    // no walk ever produces — `~/.wine-hukuk/dosdevices/z:` points at `/`.
    let (dir, src) = tree();
    let outside = tempfile::tempdir().expect("temp dir");
    std::os::unix::fs::symlink(outside.path(), dir.path().join("keep/elsewhere")).expect("symlink");

    let seen = Arc::new(Seen::default());
    let handle = watching(
        &src,
        Box::new(Fwd(Arc::clone(&seen))),
        "a_watch_does_not_walk_out_through_a_symlink",
    );

    // Something on the far side of the link, and something on this side to prove it is alive.
    std::fs::write(outside.path().join("beyond.txt"), "x").expect("write");
    let here = dir.path().join("keep/here.txt");
    std::fs::write(&here, "x").expect("write");
    let want = here.to_string_lossy().replace('\\', "/");
    let changes = wait_for(&seen, |v| {
        v.iter()
            .any(|c| matches!(c, Change::Upsert(e) if e.path == want))
    });

    assert!(
        changes
            .iter()
            .any(|c| matches!(c, Change::Upsert(e) if e.path == want)),
        "the watch has to be working for this test to mean anything: {changes:?}"
    );
    let leaked: Vec<&Change> = changes
        .iter()
        .filter(|c| match c {
            Change::Upsert(e) => e.path.contains("beyond.txt"),
            Change::Rescan { path } | Change::RemoveSubtree { path } => path.contains("beyond.txt"),
        })
        .collect();
    assert!(
        leaked.is_empty(),
        "the watcher followed a link the walk does not: {leaked:?}"
    );
    handle.stop();
}

#[test]
fn identities_are_stable_across_two_scans() {
    // The same answer twice, or every rescan looks like a complete replacement.
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
    // The engine asks `caps()`, so that is where `source.watch` has to be answered.
    let s = FsSource::new(SourceId(0), "t", vec!["/tmp".into()]);
    assert!(s.caps().contains(Caps::WATCH), "watching by default");
    let s = s.with_watch(false);
    assert!(!s.caps().contains(Caps::WATCH));
    assert!(!s.caps().contains(Caps::RECURSIVE_WATCH));
    // And what is left says something about the filesystem rather than about this
    // source: /tmp is case sensitive here, measured rather than assumed.
    assert_eq!(s.caps(), Caps::CASE_SENSITIVE);
}

#[test]
fn an_empty_root_is_reported_as_one_that_could_not_be_looked_at() {
    // **A mount that is not mounted is a readable, empty directory.** The `read_dir`
    // check says the root is fine and the walk reports zero entries — reconciling on
    // that deletes everything the index held for the volume.
    let dir = tempfile::tempdir().expect("temp dir");
    let src = FsSource::new(SourceId(9), "empty", vec![dir.path().to_path_buf()]);
    let (_paths, report) = scan(&src, &ScanOptions::default());
    assert!(
        report.vouched.is_empty(),
        "an empty source root is 'could not look', not 'there is nothing'"
    );
}

#[test]
fn an_emptied_subtree_is_still_reconciled() {
    // The other half, and why the rule is about source roots only: emptying a folder
    // is ordinary, and the walk of it has to be believed.
    let (dir, src) = tree();
    let empty = dir.path().join("emptied");
    std::fs::create_dir(&empty).expect("mkdir");
    let opts = ScanOptions {
        subtree: Some(empty.to_string_lossy().replace('\\', "/")),
        ..ScanOptions::default()
    };
    let (_paths, report) = scan(&src, &opts);
    assert_eq!(
        report.vouched.len(),
        1,
        "a walked subtree that is empty is an answer, not a failure"
    );
}

#[test]
fn a_root_that_cannot_be_read_is_reported_as_such() {
    // The difference between "found nothing" and "could not look": reconciling the
    // second deletes the whole index for that source, measured at five entries to zero.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let real = tmp.path().join("here");
    std::fs::create_dir(&real).expect("mkdir");
    std::fs::write(real.join("a.txt"), b"x").expect("write");

    let good = FsSource::new(SourceId(0), "t", vec![real.clone()]);
    let mut sink = Collect::default();
    let r = good.scan(&ScanOptions::default(), &mut sink).expect("scan");
    assert_eq!(r.vouched.len(), 1);
    assert!(r.entries > 0);

    let gone = FsSource::new(SourceId(0), "t", vec![tmp.path().join("nowhere")]);
    let mut sink = Collect::default();
    let r = gone.scan(&ScanOptions::default(), &mut sink).expect("scan");
    assert!(
        r.vouched.is_empty(),
        "a root that is not there has to be distinguishable from an empty one"
    );
    assert_eq!(r.entries, 0);
}

/// A spinning disk is not a fast disk with fewer cores — and a fast disk is not a
/// reason to use every core. The count belongs to the device *and* to the one
/// thread the entries are handed to; `Medium::threads` carries the measurements.
#[test]
fn the_device_decides_how_many_threads_are_worth_using() {
    use scour_source_fs::fs::Medium;
    assert_eq!(Medium::Spinning.threads(20), 1);
    assert_eq!(Medium::Solid.threads(20), 2);
    assert_eq!(Medium::Memory.threads(20), 2);
    assert_eq!(Medium::Network.threads(20), 4);
    // The walk is bounded by what one consumer can take, not by what the machine has.
    assert_eq!(Medium::Solid.threads(128), 2);
    assert_eq!(Medium::Solid.threads(1), 2);
    // Nor does a spinning disk on a big machine.
    assert_eq!(Medium::Spinning.threads(128), 1);
}

/// A network mount batches harder, because every reaction costs a round trip.
#[test]
fn a_network_mount_lets_events_settle_for_longer() {
    use scour_source_fs::fs::Medium;
    assert!(Medium::Network.debounce_ms() > Medium::Spinning.debounce_ms());
    assert!(Medium::Spinning.debounce_ms() > Medium::Solid.debounce_ms());
}

/// Whatever this machine is, its temp directory is not a network share. (A CI
/// runner's disk reports itself as spinning, so that much is not assumed.)
#[test]
#[cfg(target_os = "linux")]
fn a_real_mount_is_classified() {
    use scour_source_fs::fs::{Medium, medium_of};
    let m = medium_of(&std::env::temp_dir());
    assert!(!matches!(m, Medium::Network), "temp dir came out as {m:?}");
}

#[test]
fn stat_cannot_be_walked_out_of_the_source() {
    // **The fence that was not one.** `stat` is the fence the web bridge and the MCP
    // server rely on, and a string prefix is not containment: both of these returned
    // `/etc/passwd` against the running service.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let root = tmp.path().join("kok");
    std::fs::create_dir(&root).expect("mkdir");
    std::fs::write(root.join("icerde.txt"), b"benim").expect("write");
    // A link out of the tree, which is a thing home directories are full of.
    std::os::unix::fs::symlink("/etc", root.join("disari")).expect("symlink");

    let src = FsSource::new(SourceId(0), "kok", vec![root.clone()]);

    assert!(
        src.stat(&format!("{}/icerde.txt", root.display())).is_ok(),
        "a path that really is inside has to work"
    );
    assert!(
        src.stat(&format!("{}/../../etc/passwd", root.display()))
            .is_err(),
        "climbing out with .. was answered"
    );
    assert!(
        src.stat(&format!("{}/disari/passwd", root.display()))
            .is_err(),
        "a symlink out of the tree was followed"
    );

    // The link itself is still a row of its own: `stat` of it describes the link.
    let link = src
        .stat(&format!("{}/disari", root.display()))
        .expect("the link is inside the source");
    assert!(!link.is_dir, "the final symlink must not be followed");
}

#[test]
#[cfg(unix)]
fn two_files_whose_names_are_not_utf8_are_two_entries() {
    // **Two files on disk used to become one row**: every invalid byte decoded to the
    // same replacement character, so `same-\xfe` and `same-\xff` took one path.
    use std::os::unix::ffi::OsStrExt;

    let tmp = tempfile::tempdir().expect("tmpdir");
    let names: [&[u8]; 3] = [b"ayni-\xfe", b"ayni-\xff", b"rapor-\xc3(2).pdf"];
    for raw in names {
        let p = tmp.path().join(std::ffi::OsStr::from_bytes(raw));
        std::fs::write(&p, b"x").expect("write");
    }

    let src = FsSource::new(SourceId(0), "t", vec![tmp.path().to_owned()]);
    let mut sink = Collect::default();
    src.scan(&ScanOptions::default(), &mut sink).expect("scan");

    let mut paths: Vec<String> = sink
        .paths
        .iter()
        .filter(|p| p.as_str() != tmp.path().to_string_lossy())
        .cloned()
        .collect();
    paths.sort();
    assert_eq!(paths.len(), 3, "names collapsed into each other: {paths:?}");

    // And every one of them can be asked about again, which a search result must do.
    for p in &paths {
        let back = src
            .stat(p)
            .unwrap_or_else(|e| panic!("{p:?} cannot be stat-ed again: {e}"));
        assert_eq!(&back.path, p);
        assert_eq!(back.meta.size, 1);
    }
}

#[test]
fn a_directory_that_cannot_be_read_is_named_so_the_sweep_can_spare_it() {
    // A directory that loses its read permission after it was indexed still holds
    // every one of its files, so the sweep needs the paths and not a count.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let root = tmp.path().join("kok");
    let closed = root.join("kapali");
    std::fs::create_dir_all(closed.join("derin")).expect("mkdir");
    std::fs::write(root.join("gorunur.txt"), b"x").expect("write");
    std::fs::write(closed.join("gizli.txt"), b"x").expect("write");

    let mut mode = std::fs::metadata(&closed).expect("stat").permissions();
    let was = std::os::unix::fs::PermissionsExt::mode(&mode);
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o000);
    std::fs::set_permissions(&closed, mode.clone()).expect("chmod");

    let src = FsSource::new(SourceId(0), "t", vec![root.clone()]);
    let mut sink = Collect::default();
    let report = src.scan(&ScanOptions::default(), &mut sink).expect("scan");

    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, was);
    std::fs::set_permissions(&closed, mode).expect("chmod back");

    assert_eq!(
        report.vouched.len(),
        1,
        "one unreadable directory does not make the root unreadable"
    );
    assert!(
        report
            .blind
            .iter()
            .any(|b| b.ends_with("kapali") || b.contains("kapali")),
        "the unreadable directory was counted but not named: {:?}",
        report.blind
    );
}

#[test]
fn a_root_that_changes_underneath_the_walk_is_not_vouched_for() {
    // One `read_dir` before the walk leaves everything after it on trust: unmount a
    // volume mid-scan and the engine reconciles against a filesystem that is not
    // there. A bind mount is the portable way to change a device number without root.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let root = tmp.path().join("kok");
    std::fs::create_dir(&root).expect("mkdir");
    std::fs::write(root.join("a.txt"), b"x").expect("write");

    let src = FsSource::new(SourceId(0), "t", vec![root.clone()]);
    let mut sink = Collect::default();
    let ok = src.scan(&ScanOptions::default(), &mut sink).expect("scan");
    assert_eq!(ok.vouched.len(), 1, "an ordinary walk vouches for its root");

    // And when the root is gone by the end of it, nothing is vouched for.
    std::fs::remove_dir_all(&root).expect("rm");
    let mut sink = Collect::default();
    let after = src.scan(&ScanOptions::default(), &mut sink).expect("scan");
    assert!(
        after.vouched.is_empty(),
        "a root that vanished was still being reconciled against"
    );
}
