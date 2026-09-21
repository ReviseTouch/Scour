//! Against the public API: finding the binary, and the three ways waiting for
//! it can end.

use std::path::{Path, PathBuf};
use std::time::Duration;

use scour_launch::{Autostart, NAME, Outcome, locate};

/// A file named like the service, marked runnable.
fn stub(dir: &Path, body: &str) -> PathBuf {
    let path = dir.join(NAME);
    std::fs::write(&path, body).expect("write the stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .expect("mark it runnable");
    }
    path
}

#[test]
fn the_copy_beside_the_face_wins_over_the_one_on_the_path() {
    let beside = tempfile::tempdir().expect("temp");
    let elsewhere = tempfile::tempdir().expect("temp");
    let here = stub(beside.path(), "#!/bin/sh\nexit 0\n");
    stub(elsewhere.path(), "#!/bin/sh\nexit 0\n");

    let face = beside.path().join("scour-tui");
    let path = std::env::join_paths([elsewhere.path()]).expect("PATH");
    assert_eq!(locate(Some(&face), Some(&path)), Some(here));
}

#[test]
fn the_path_is_searched_when_nothing_sits_beside_the_face() {
    let empty = tempfile::tempdir().expect("temp");
    let elsewhere = tempfile::tempdir().expect("temp");
    let there = stub(elsewhere.path(), "#!/bin/sh\nexit 0\n");

    let face = empty.path().join("scour-tui");
    let path = std::env::join_paths([elsewhere.path()]).expect("PATH");
    assert_eq!(locate(Some(&face), Some(&path)), Some(there));
    // And nothing is invented when it is on neither.
    let nothing = std::env::join_paths([empty.path()]).expect("PATH");
    assert_eq!(locate(Some(&face), Some(&nothing)), None);
}

#[test]
fn a_service_already_listening_is_left_alone() {
    let home = tempfile::tempdir().expect("temp");
    let never = home.path().join("never-run");
    let outcome = Autostart::new("anywhere", home.path().join("log"), &|_| true)
        .binary(never.clone())
        .run();
    assert_eq!(outcome, Outcome::AlreadyRunning);
    assert!(!never.exists(), "nothing should have been looked for");
}

#[test]
fn nothing_beside_and_nothing_on_the_path_says_so() {
    let home = tempfile::tempdir().expect("temp");
    let missing = home.path().join("no-such-scourd");
    let outcome = Autostart::new("anywhere", home.path().join("log"), &|_| false)
        .binary(missing)
        .within(Duration::from_millis(50))
        .run();
    match outcome {
        // The binary was named, so this is the spawn failing rather than the
        // search; either way the sentence has to name the program.
        Outcome::Failed(why) => assert!(why.contains("no-such-scourd"), "{why}"),
        other => panic!("{other:?}"),
    }
}

/// The ordinary case: the service takes a moment to open its socket.
#[cfg(unix)]
#[test]
fn a_socket_that_appears_late_is_waited_for() {
    let home = tempfile::tempdir().expect("temp");
    let exe = stub(
        home.path(),
        "#!/bin/sh\necho starting\nsleep 0.4\n: > \"$2\"\nsleep 30\n",
    );
    let addr = home.path().join("scour.sock");
    let log = home.path().join("scourd.log");
    let there = addr.clone();

    let outcome = Autostart::new(
        addr.to_str().expect("utf-8"),
        log.clone(),
        &move |_| there.exists(),
    )
    .binary(exe)
    .within(Duration::from_secs(5))
    .every(Duration::from_millis(50))
    .run();

    assert_eq!(outcome, Outcome::Started);
    assert!(outcome.running());
    let said = std::fs::read_to_string(&log).expect("the log");
    assert!(said.contains("starting"), "{said:?}");
}

/// The race a system unit wins: ours exits, and nothing answers either.
#[cfg(unix)]
#[test]
fn a_service_that_exits_is_reported_with_its_status() {
    let home = tempfile::tempdir().expect("temp");
    let exe = stub(
        home.path(),
        "#!/bin/sh\necho 'another writer holds the index' >&2\nexit 3\n",
    );
    let log = home.path().join("scourd.log");

    let outcome = Autostart::new("nowhere", log.clone(), &|_| false)
        .binary(exe)
        .within(Duration::from_millis(400))
        .every(Duration::from_millis(50))
        .run();

    let why = outcome.reason().unwrap_or_default().to_string();
    assert!(why.contains("status 3"), "{why}");
    assert!(why.contains(&log.display().to_string()), "{why}");
    let said = std::fs::read_to_string(&log).expect("the log");
    assert!(said.contains("another writer"), "{said:?}");
}

#[cfg(unix)]
#[test]
fn a_service_that_never_answers_gives_up_and_names_the_log() {
    let home = tempfile::tempdir().expect("temp");
    let exe = stub(home.path(), "#!/bin/sh\nsleep 30\n");
    let log = home.path().join("scourd.log");

    let outcome = Autostart::new("nowhere", log.clone(), &|_| false)
        .binary(exe)
        .within(Duration::from_millis(300))
        .every(Duration::from_millis(50))
        .run();

    let why = outcome.reason().unwrap_or_default().to_string();
    assert!(why.contains("did not answer within 300 ms"), "{why}");
    assert!(why.contains(&log.display().to_string()), "{why}");
    assert!(!outcome.running());
}

/// The service is started with the address the face resolved, not with
/// whatever the configuration would have given it.
#[cfg(unix)]
#[test]
fn the_address_the_face_used_is_the_one_the_service_is_given() {
    let home = tempfile::tempdir().expect("temp");
    let exe = stub(home.path(), "#!/bin/sh\necho \"args: $@\"\nexit 1\n");
    let log = home.path().join("scourd.log");

    let _ = Autostart::new("/tmp/a-particular.sock", log.clone(), &|_| false)
        .binary(exe)
        .within(Duration::from_millis(200))
        .every(Duration::from_millis(50))
        .run();

    let said = std::fs::read_to_string(&log).expect("the log");
    assert!(
        said.contains("args: --socket /tmp/a-particular.sock"),
        "{said:?}"
    );
}
