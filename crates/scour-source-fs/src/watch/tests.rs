use super::*;
use std::sync::Mutex as PlMutex;

/// Everything a translation emitted, in order.
#[derive(Debug, Default)]
struct Collect(PlMutex<Vec<Change>>);

impl ChangeSink for Collect {
    fn emit(&self, change: Change) {
        self.0.lock().expect("the collector").push(change);
    }
}

#[cfg(not(target_os = "linux"))]
fn translated(event: Event) -> Vec<Change> {
    let collector = Arc::new(Collect::default());
    let sink: Arc<dyn ChangeSink> = Arc::clone(&collector) as Arc<dyn ChangeSink>;
    translate(
        scour_core::SourceId(0),
        true,
        &Rules::from_options(&ScanOptions::default()),
        &event,
        &sink,
    );
    collector.0.lock().expect("the collector").clone()
}

#[cfg(not(target_os = "linux"))]
#[test]
fn losing_track_asks_for_a_walk_even_when_it_cannot_say_where() {
    // A queue overflow arrives as `EventKind::Other` with the rescan flag and *no paths*.
    let overflow = Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan);
    assert_eq!(
        translated(overflow),
        vec![Change::Rescan {
            path: String::new()
        }],
        "an empty path is how the engine is told to walk everything"
    );

    let somewhere = Event::new(EventKind::Other)
        .set_flag(notify::event::Flag::Rescan)
        .add_path("/home/u/Projeler".into());
    assert_eq!(
        translated(somewhere),
        vec![Change::Rescan {
            path: "/home/u/Projeler".into()
        }]
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn the_uncovered_list_stops_growing_instead_of_growing_forever() {
    // The paths are distinct, so nothing but the truncation bounds the list.
    let held: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
    for round in 0..40 {
        let batch: Vec<String> = (0..200)
            .map(|i| format!("/home/u/proje/paket-{round:03}/altdizin-{i:03}"))
            .collect();
        let mut guard = held.lock().expect("the list");
        guard.extend(batch);
        guard.sort_unstable();
        guard.dedup();
        guard.truncate(MAX_SKIPPED);
    }
    let n = held.lock().expect("the list").len();
    assert_eq!(
        n, MAX_SKIPPED,
        "8,000 distinct uncovered subtrees were kept as {n}, not {MAX_SKIPPED}"
    );
}

#[cfg(not(target_os = "linux"))]
#[test]
fn a_refused_subtree_is_still_named_when_there_are_few_of_them() {
    // 191 refused directories is the real report this list exists for.
    let mut skipped = Vec::new();
    for i in 0..191 {
        remember(&mut skipped, format!("/home/u/.local/share/waydroid/{i}"));
    }
    assert_eq!(skipped.len(), 191, "an ordinary report was truncated");

    for i in 0..MAX_SKIPPED * 2 {
        remember(&mut skipped, format!("/home/u/başka/{i}"));
    }
    assert_eq!(skipped.len(), MAX_SKIPPED);
}

#[test]
fn a_path_that_cannot_be_read_is_not_a_path_that_is_gone() {
    // A directory with no execute bit deterministically gives `EACCES`.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let dir = tmp.path().join("kapali");
    std::fs::create_dir(&dir).expect("mkdir");
    std::fs::write(dir.join("dosya.txt"), b"x").expect("write");
    let mut mode = std::fs::metadata(&dir).expect("stat").permissions();
    let was = std::os::unix::fs::PermissionsExt::mode(&mode);
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o600);
    std::fs::set_permissions(&dir, mode.clone()).expect("chmod");

    let inside = crate::path::from_path(&dir.join("dosya.txt"));
    let sink = Collect(PlMutex::new(Vec::new()));
    look(scour_core::SourceId(1), false, &inside, false, &sink);

    // Put the bit back before asserting, so a failure leaves nothing behind.
    std::os::unix::fs::PermissionsExt::set_mode(&mut mode, was);
    std::fs::set_permissions(&dir, mode).expect("chmod back");

    assert_eq!(
        sink.0.into_inner().unwrap(),
        vec![Change::Rescan {
            path: inside.clone()
        }],
        "an unreadable path was reported as deleted"
    );

    let missing = crate::path::from_path(&tmp.path().join("yok.txt"));
    let sink = Collect(PlMutex::new(Vec::new()));
    look(scour_core::SourceId(1), false, &missing, false, &sink);
    assert_eq!(
        sink.0.into_inner().unwrap(),
        vec![Change::RemoveSubtree { path: missing }]
    );
}
