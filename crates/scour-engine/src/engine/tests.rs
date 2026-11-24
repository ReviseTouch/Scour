use super::{PendingWalks, coalesce, holds_nothing};

/// A disk is walked "first" — its rows shown as they are found — when the
/// index holds nothing under its roots, not only when the index is empty.
#[test]
fn a_disk_with_no_rows_is_a_first_walk_beside_one_that_has_them() {
    use scour_core::{Change, Entry, EntryId, Index, Meta, SourceId};
    let dir = tempfile::tempdir().expect("temp");
    let index = scour_index_native::NativeIndex::open_or_create(dir.path()).expect("index");
    assert!(holds_nothing(&index, &["C:/".into()]), "an empty index");
    let row = |path: &str| {
        Change::Upsert(Entry {
            id: EntryId::path_hash(SourceId(0), path),
            path: path.into(),
            is_dir: false,
            meta: Meta::UNKNOWN,
        })
    };
    index
        .apply(&mut vec![row("C:/a.txt"), row("C:/b/c.txt")].into_iter())
        .expect("apply");
    index.publish().expect("publish");
    assert!(!holds_nothing(&index, &["C:/".into()]), "C: has rows");
    assert!(holds_nothing(&index, &["G:/".into()]), "G: has none yet");
    assert!(
        !holds_nothing(&index, &["G:/".into(), "C:/".into()]),
        "one root with rows is not a first walk"
    );
}
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
