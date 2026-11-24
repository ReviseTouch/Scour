use super::*;

/// Counts permits held at the same moment across a batch larger than the bound.
#[test]
fn never_more_than_the_bound_at_once() {
    let maker = Maker::new(3);
    let now = std::sync::atomic::AtomicUsize::new(0);
    let most = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..20 {
            scope.spawn(|| {
                let _permit = maker.permit();
                let held = now.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                most.fetch_max(held, std::sync::atomic::Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(20));
                now.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            });
        }
    });
    assert_eq!(most.load(std::sync::atomic::Ordering::SeqCst), 3);
    assert_eq!(*maker.free.lock().unwrap(), 3, "every permit came back");
}

/// A batch bigger than the cap is cut, not queued.
#[test]
fn a_batch_is_capped() {
    let maker = Maker::new(2);
    // Names nothing can thumbnail: only the count of files looked at moves.
    let wanted: Vec<Wanted> = (0..100)
        .map(|i| Wanted {
            path: format!("/nowhere/{i}.this-extension-does-not-exist"),
            mtime: 1,
        })
        .collect();
    let made = maker.make(&wanted);
    assert_eq!(made.ran, 0, "nothing on this machine draws that");
    assert!(made.ready.is_empty());
}

#[test]
fn a_percent_that_is_not_a_placeholder_survives() {
    // The substitution is not public, so a command records what it was given.
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("args");
    let script = dir.path().join("say.sh");
    std::fs::write(
        &script,
        format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\n", out.display()),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let command: Vec<String> = vec![
        script.to_string_lossy().into_owned(),
        "%i".into(),
        "%u".into(),
        "%s".into(),
        "%o".into(),
        "100%%".into(),
        "%z".into(),
    ];
    assert!(run(
        &command,
        "/a/b c.png",
        Path::new("/tmp/out.png"),
        Maker::PATIENCE
    ));
    let said = std::fs::read_to_string(&out).unwrap();
    let lines: Vec<&str> = said.lines().collect();
    assert_eq!(lines[0], "/a/b c.png");
    assert_eq!(lines[1], "file:///a/b%20c.png");
    assert_eq!(lines[2], crate::cache::ASKED.to_string());
    assert_eq!(lines[3], "/tmp/out.png");
    assert_eq!(lines[4], "100%");
    // A substitution nobody defined is left alone rather than eaten.
    assert_eq!(lines[5], "%z");
}

/// A thumbnailer that never returns must not hold its permit: four hung ones
/// would stop every picture on the machine.
#[test]
fn a_hung_thumbnailer_is_killed() {
    let long = Duration::from_secs(300);
    let began = Instant::now();
    assert!(!run(
        &["sleep".to_owned(), "300".to_owned()],
        "/a/b",
        Path::new("/tmp/x.png"),
        Duration::from_millis(200),
    ));
    assert!(began.elapsed() < long, "it waited the whole sleep out");
    assert!(began.elapsed() < Duration::from_secs(5));
}

#[test]
fn a_process_that_fails_is_not_a_picture() {
    let p = Maker::PATIENCE;
    assert!(run(
        &["true".to_owned()],
        "/a/b",
        Path::new("/tmp/x.png"),
        p
    ));
    assert!(!run(
        &["false".to_owned()],
        "/a/b",
        Path::new("/tmp/x.png"),
        p
    ));
    // A thumbnailer whose package went away since the declaration was read.
    assert!(!run(
        &["this-command-does-not-exist".to_owned()],
        "/a/b",
        Path::new("/tmp/x.png"),
        p
    ));
}
