use super::*;

fn quiet(n: usize) -> Pulses {
    let mut p = Pulses::new(
        n,
        Duration::from_secs(60),
        Duration::from_secs(1800),
        Duration::from_secs(86_400),
    );
    // The first reading only establishes a baseline; start from there.
    p.decide(&[Some(1)], &[], &|| true);
    p
}

#[test]
fn a_movement_inside_the_floor_is_remembered_not_dropped() {
    let mut p = quiet(1);
    // Just walked, so the floor is closed.
    p.walked[0] = Instant::now();
    assert!(
        p.decide(&[Some(2)], &[], &|| true).is_empty(),
        "the floor holds it back"
    );
    assert!(p.pending[0], "but the movement is remembered");
    // The floor opens, and nothing has moved since.
    p.walked[0] = Instant::now() - Pulses::FLOOR;
    let out = p.decide(&[Some(2)], &[], &|| true);
    assert!(
        matches!(out.as_slice(), [(0, Nudge::Reconcile)]),
        "a movement that arrived early is still acted on: {out:?}"
    );
    assert!(!p.pending[0], "and only once");
}

/// The system disk unwatched: its pulse moves every two seconds, and its
/// own patience, not its cost, says how often that is worth a walk.
#[test]
fn a_patient_source_waits_out_its_patience_after_a_walk() {
    let mut p = quiet(1);
    p.be_patient(0, Duration::from_secs(600));
    p.scanned(0, Duration::from_millis(600));
    assert_eq!(p.pulse_floor[0], Duration::from_secs(600));
    p.walked[0] = Instant::now() - Duration::from_secs(60);
    assert!(
        p.decide(&[Some(2)], &[], &|| true).is_empty(),
        "a minute is not enough"
    );
    p.walked[0] = Instant::now() - Duration::from_secs(600);
    let out = p.decide(&[Some(3)], &[], &|| true);
    assert!(matches!(out.as_slice(), [(0, Nudge::Reconcile)]), "{out:?}");
}

#[test]
fn a_still_pulse_asks_for_nothing() {
    let mut p = quiet(1);
    p.walked[0] = Instant::now() - Pulses::FLOOR;
    assert!(p.decide(&[Some(1)], &[], &|| true).is_empty());
    assert!(p.decide(&[Some(1)], &[], &|| true).is_empty());
}

#[test]
fn a_watched_source_is_called_blind_only_after_patience() {
    let mut p = quiet(1);
    for step in 0..Pulses::PATIENCE - 1 {
        let out = p.decide(&[Some(step as u64 + 2)], &[0], &|| true);
        assert!(out.is_empty(), "not yet at step {step}");
    }
    let out = p.decide(&[Some(9_999)], &[0], &|| true);
    assert!(matches!(out.as_slice(), [(0, Nudge::Blind)]), "{out:?}");
}

#[test]
fn an_event_clears_the_suspicion() {
    let mut p = quiet(1);
    for step in 0..Pulses::PATIENCE - 1 {
        p.decide(&[Some(step as u64 + 2)], &[0], &|| true);
    }
    p.saw_event(0);
    let out = p.decide(&[Some(9_999)], &[0], &|| true);
    assert!(out.is_empty(), "a watcher that spoke is not blind: {out:?}");
}

#[test]
fn a_source_with_no_pulse_is_left_alone() {
    let mut p = Pulses::new(
        1,
        Duration::from_secs(60),
        Duration::from_secs(1800),
        Duration::from_secs(86_400),
    );
    assert!(p.decide(&[None], &[], &|| true).is_empty());
    assert!(p.decide(&[None], &[], &|| true).is_empty());
}
#[test]
fn a_source_without_a_pulse_is_eventually_polled() {
    let mut p = quiet(1);
    p.fallback[0] = Instant::now();
    assert!(matches!(
        p.decide(&[None], &[], &|| true).as_slice(),
        [(0, Nudge::Reconcile)]
    ));
    assert!(p.decide(&[None], &[], &|| true).is_empty());
}

/// A watched source on a ten-minute check, last walked `ago` seconds back.
fn watched(ago: u64) -> Pulses {
    let mut p = Pulses::new(
        1,
        Duration::from_secs(60),
        Duration::from_secs(1800),
        Duration::from_secs(600),
    );
    p.decide(&[Some(1)], &[0], &|| true);
    p.whole_at[0] = Instant::now() - Duration::from_secs(ago);
    p
}

#[test]
fn events_in_one_subtree_do_not_postpone_a_check() {
    let mut p = watched(601);
    p.saw_event(0);
    assert!(matches!(
        p.decide(&[Some(1)], &[0], &|| true).as_slice(),
        [(0, Nudge::Check)]
    ));
    assert!(p.decide(&[Some(1)], &[0], &|| true).is_empty());
}

/// Thirty minutes is the clock of a source nobody watches. A watched one
/// walked on it cost 5 GB of reads a round and found nothing but mapped writes.
#[test]
fn a_watched_source_is_not_walked_on_the_unwatched_clock() {
    let mut p = watched(599);
    p.safety[0] = Instant::now() - Duration::from_secs(1);
    assert!(p.decide(&[Some(1)], &[0], &|| true).is_empty());
    // Unwatched, the same deadline is a walk.
    assert!(matches!(
        p.decide(&[Some(1)], &[], &|| true).as_slice(),
        [(0, Nudge::Reconcile)]
    ));
}

#[test]
fn a_pressure_file_says_how_much_of_the_last_minute_was_spent_waiting() {
    let file = "some avg10=0.52 avg60=12.84 avg300=0.36 total=11296829001\n\
                    full avg10=0.00 avg60=0.00 avg300=0.00 total=0\n";
    assert_eq!(waited(file), Some(12.84));
    assert_eq!(
        waited("full avg10=1.00 avg60=2.00 avg300=3.00 total=4\n"),
        None
    );
    assert_eq!(waited(""), None);
}

#[test]
fn a_check_waits_for_a_quiet_machine() {
    let mut p = watched(601);
    assert!(p.decide(&[Some(1)], &[0], &|| false).is_empty());
    assert!(p.decide(&[Some(1)], &[0], &|| false).is_empty());
    assert!(matches!(
        p.decide(&[Some(1)], &[0], &|| true).as_slice(),
        [(0, Nudge::Check)]
    ));
}

#[test]
fn a_check_that_never_finds_quiet_runs_an_interval_late() {
    let mut p = watched(1_201);
    assert!(matches!(
        p.decide(&[Some(1)], &[0], &|| false).as_slice(),
        [(0, Nudge::Check)]
    ));
}

#[test]
fn a_walk_for_any_reason_restarts_the_check_clock() {
    let mut p = watched(601);
    p.scanned(0, Duration::from_millis(1));
    assert!(p.decide(&[Some(1)], &[0], &|| true).is_empty());
}

#[test]
fn quiet_is_asked_only_when_a_check_is_due() {
    let asked = std::cell::Cell::new(0);
    let count = || {
        asked.set(asked.get() + 1);
        true
    };
    let mut p = watched(10);
    p.decide(&[Some(1)], &[0], &count);
    assert_eq!(asked.get(), 0);
    p.whole_at[0] = Instant::now() - Duration::from_secs(601);
    p.decide(&[Some(1)], &[0], &count);
    assert_eq!(asked.get(), 1);
}

/// A share nothing watches has no pulse; its patience still decides how
/// often it is walked, not the one-minute poll.
#[test]
fn a_patient_source_without_a_pulse_waits_its_patience() {
    let mut p = quiet(1);
    p.be_patient(0, Duration::from_secs(600));
    p.scanned(0, Duration::from_secs(2));
    assert!(p.fallback[0].duration_since(p.walked[0]) >= Duration::from_secs(600));
    assert!(p.decide(&[None], &[], &|| true).is_empty());
}

#[test]
fn expensive_passes_rest_before_the_next_poll() {
    let mut p = quiet(1);
    p.scanned(0, Duration::from_secs(10));
    assert!(p.fallback[0].duration_since(p.walked[0]) >= Duration::from_secs(200));
    assert!(p.decide(&[None], &[], &|| true).is_empty());
    p.walked[0] = Instant::now() - Pulses::FLOOR;
    assert!(p.decide(&[Some(2)], &[], &|| true).is_empty());
    assert!(
        p.pending[0],
        "a busy pulse must wait for the scan's cost too"
    );
}

#[test]
fn blind_watch_recovery_also_respects_the_previous_scan_cost() {
    let mut p = quiet(1);
    p.scanned(0, Duration::from_secs(60));
    p.moved_since_event[0] = Pulses::PATIENCE;
    assert!(p.decide(&[Some(2)], &[0], &|| true).is_empty());
    p.walked[0] = Instant::now() - Duration::from_secs(1200);
    assert!(matches!(
        p.decide(&[Some(3)], &[0], &|| true).as_slice(),
        [(0, Nudge::Blind)]
    ));
}

#[test]
fn retries_pay_for_the_pass_even_when_another_deadline_is_due() {
    let mut p = quiet(1);
    p.scanned(0, Duration::from_secs(60));
    p.safety[0] = Instant::now();
    assert!(!p.decide(&[None], &[], &|| true).is_empty());
    let before = Instant::now();
    assert!(p.retry_at(0, Duration::from_secs(30)) >= before + Duration::from_secs(1200));
    assert!(p.retry_at(0, Duration::from_secs(3600)) >= before + Duration::from_secs(3600));
}

#[test]
fn cheap_failures_keep_the_short_retry_delay() {
    let mut p = quiet(1);
    p.scanned(0, Duration::from_millis(1));
    let before = Instant::now();
    let at = p.retry_at(0, Duration::from_secs(10));
    assert!(at >= before + Duration::from_secs(10));
    assert!(at < before + Duration::from_secs(11));
}
