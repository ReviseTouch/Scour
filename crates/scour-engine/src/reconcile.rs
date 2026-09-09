//! Recovery scheduling for sources whose change feed is incomplete.

use std::time::{Duration, Instant};

/// What the tick decided a source needs.
#[derive(Debug)]
pub(crate) enum Nudge {
    /// Nobody is watching it and it moved.
    Reconcile,
    /// Somebody is watching it, it keeps moving, and nothing is arriving.
    Blind,
}

/// One pulse reading per source, and the rules that turn it into work. A moving
/// pulse says the partition moved, not this source, so it licenses a look and
/// never a rescan; the floors below decide how often.
pub(crate) struct Pulses {
    last: Vec<Option<u64>>,
    /// The pulse moved and nothing has been done about it yet. Kept across the
    /// floor: the floor bounds how often a source is walked, not which movements count.
    pending: Vec<bool>,
    /// When each source was last walked because of a pulse.
    walked: Vec<Instant>,
    pulse_floor: Vec<Duration>,
    scan_rest: Vec<Duration>,
    /// When a change last arrived for each source, so a watcher that has gone
    /// quiet can be told from a disk that is quiet.
    moved_since_event: Vec<u32>,
    checked: Instant,
    poll: Duration,
    reconcile: Duration,
    fallback: Vec<Instant>,
    safety: Vec<Instant>,
}

impl Pulses {
    /// How often the pulses are read. They cost microseconds; this is about
    /// not calling `scan` in a tight loop, not about the reading.
    const EVERY: Duration = Duration::from_secs(2);
    /// The least time between two walks of the same unwatched source.
    const FLOOR: Duration = Duration::from_secs(15);
    /// How many consecutive moved readings with nothing arriving before a watched
    /// source is called blind. At `EVERY` apart, five minutes.
    const PATIENCE: u32 = 150;

    pub(crate) fn new(n: usize, poll: Duration, reconcile: Duration) -> Pulses {
        Pulses {
            last: vec![None; n],
            pending: vec![false; n],
            // Back-dated: the first movement is acted on, not held by an unearned floor.
            walked: vec![Instant::now() - Self::FLOOR; n],
            pulse_floor: vec![Self::FLOOR; n],
            scan_rest: vec![Duration::ZERO; n],
            moved_since_event: vec![0; n],
            checked: Instant::now(),
            poll,
            reconcile,
            fallback: vec![Instant::now() + poll; n],
            safety: vec![Instant::now() + reconcile; n],
        }
    }

    /// When the pulses will next be worth reading — on an idle machine the only
    /// deadline left, so it sets how often a service with nothing to do wakes.
    pub(crate) fn next_due(&self) -> Instant {
        self.checked + Self::EVERY
    }

    pub(crate) fn is_due(&self) -> bool {
        self.checked.elapsed() >= Self::EVERY
    }

    /// Record a completed full pass, including its cost. Subtree scans cannot
    /// vouch for the rest of a source and must not postpone these deadlines.
    pub(crate) fn scanned(&mut self, source: usize, cost: Duration) {
        self.scan_rest[source] = cost.saturating_mul(20);
        self.defer(source);
    }

    fn defer(&mut self, source: usize) {
        let now = Instant::now();
        self.walked[source] = now;
        self.pending[source] = false;
        let rest = self.scan_rest[source];
        self.pulse_floor[source] = Self::FLOOR.max(rest);
        self.fallback[source] = now + self.poll.max(rest);
        self.safety[source] = now + self.reconcile.max(rest);
    }

    /// A failed pass costs the same I/O as a successful one. Start the wait
    /// after it finishes, including when the pass exceeded the retry delay.
    pub(crate) fn retry_at(&self, source: usize, delay: Duration) -> Instant {
        Instant::now() + delay.max(self.scan_rest[source])
    }

    /// The rules, with the reading already taken.
    pub(crate) fn decide(
        &mut self,
        readings: &[Option<u64>],
        watched: &[usize],
    ) -> Vec<(usize, Nudge)> {
        self.checked = Instant::now();
        let trace = std::env::var_os("SCOUR_PULSE_TRACE").is_some();
        let mut out = Vec::new();
        for (i, reading) in readings.iter().enumerate() {
            let now = Instant::now();
            // A pulse is a hint: a watcher can miss one subtree while reporting another.
            let safety_due = now >= self.safety[i];
            let polling_due = !watched.contains(&i) && now >= self.fallback[i];
            if safety_due || (reading.is_none() && polling_due) {
                self.defer(i);
                self.last[i] = *reading;
                self.moved_since_event[i] = 0;
                out.push((i, Nudge::Reconcile));
                continue;
            }
            let Some(now) = *reading else {
                if trace {
                    scour_core::note!("scourd: source {i} has no pulse to read");
                }
                continue;
            };
            let moved = self.last[i].is_some_and(|was| was != now);
            if trace {
                scour_core::note!(
                    "scourd: source {i} pulse {now} (was {:?}), moved={moved}, watched={}, since walk {:?}",
                    self.last[i],
                    watched.contains(&i),
                    self.walked[i].elapsed(),
                );
            }
            self.last[i] = Some(now);
            if moved {
                self.pending[i] = true;
            }
            if watched.contains(&i) {
                if moved {
                    self.moved_since_event[i] = self.moved_since_event[i].saturating_add(1);
                }
                if self.moved_since_event[i] >= Self::PATIENCE
                    && self.walked[i].elapsed() >= self.pulse_floor[i]
                {
                    self.moved_since_event[i] = 0;
                    self.pending[i] = false;
                    self.walked[i] = Instant::now();
                    out.push((i, Nudge::Blind));
                }
            } else if self.pending[i] && self.walked[i].elapsed() >= self.pulse_floor[i] {
                self.pending[i] = false;
                self.walked[i] = Instant::now();
                out.push((i, Nudge::Reconcile));
            }
        }
        out
    }

    /// A change arrived, so whatever the pulse has been saying, the watcher is
    /// awake.
    pub(crate) fn saw_event(&mut self, source: usize) {
        if let Some(n) = self.moved_since_event.get_mut(source) {
            *n = 0;
        }
    }
}

#[cfg(test)]
mod pulse_tests {
    use super::*;

    fn quiet(n: usize) -> Pulses {
        let mut p = Pulses::new(n, Duration::from_secs(60), Duration::from_secs(1800));
        // The first reading only establishes a baseline; start from there.
        p.decide(&[Some(1)], &[]);
        p
    }

    #[test]
    fn a_movement_inside_the_floor_is_remembered_not_dropped() {
        let mut p = quiet(1);
        // Just walked, so the floor is closed.
        p.walked[0] = Instant::now();
        assert!(
            p.decide(&[Some(2)], &[]).is_empty(),
            "the floor holds it back"
        );
        assert!(p.pending[0], "but the movement is remembered");
        // The floor opens, and nothing has moved since.
        p.walked[0] = Instant::now() - Pulses::FLOOR;
        let out = p.decide(&[Some(2)], &[]);
        assert!(
            matches!(out.as_slice(), [(0, Nudge::Reconcile)]),
            "a movement that arrived early is still acted on: {out:?}"
        );
        assert!(!p.pending[0], "and only once");
    }

    #[test]
    fn a_still_pulse_asks_for_nothing() {
        let mut p = quiet(1);
        p.walked[0] = Instant::now() - Pulses::FLOOR;
        assert!(p.decide(&[Some(1)], &[]).is_empty());
        assert!(p.decide(&[Some(1)], &[]).is_empty());
    }

    #[test]
    fn a_watched_source_is_called_blind_only_after_patience() {
        let mut p = quiet(1);
        for step in 0..Pulses::PATIENCE - 1 {
            let out = p.decide(&[Some(step as u64 + 2)], &[0]);
            assert!(out.is_empty(), "not yet at step {step}");
        }
        let out = p.decide(&[Some(9_999)], &[0]);
        assert!(matches!(out.as_slice(), [(0, Nudge::Blind)]), "{out:?}");
    }

    #[test]
    fn an_event_clears_the_suspicion() {
        let mut p = quiet(1);
        for step in 0..Pulses::PATIENCE - 1 {
            p.decide(&[Some(step as u64 + 2)], &[0]);
        }
        p.saw_event(0);
        let out = p.decide(&[Some(9_999)], &[0]);
        assert!(out.is_empty(), "a watcher that spoke is not blind: {out:?}");
    }

    #[test]
    fn a_source_with_no_pulse_is_left_alone() {
        let mut p = Pulses::new(1, Duration::from_secs(60), Duration::from_secs(1800));
        assert!(p.decide(&[None], &[]).is_empty());
        assert!(p.decide(&[None], &[]).is_empty());
    }
    #[test]
    fn a_source_without_a_pulse_is_eventually_polled() {
        let mut p = quiet(1);
        p.fallback[0] = Instant::now();
        assert!(matches!(
            p.decide(&[None], &[]).as_slice(),
            [(0, Nudge::Reconcile)]
        ));
        assert!(p.decide(&[None], &[]).is_empty());
    }

    #[test]
    fn events_in_one_subtree_do_not_postpone_a_safety_pass() {
        let mut p = quiet(1);
        p.safety[0] = Instant::now();
        p.saw_event(0);
        assert!(matches!(
            p.decide(&[Some(1)], &[0]).as_slice(),
            [(0, Nudge::Reconcile)]
        ));
        assert!(p.decide(&[Some(1)], &[0]).is_empty());
    }

    #[test]
    fn expensive_passes_rest_before_the_next_poll() {
        let mut p = quiet(1);
        p.scanned(0, Duration::from_secs(10));
        assert!(p.fallback[0].duration_since(p.walked[0]) >= Duration::from_secs(200));
        assert!(p.decide(&[None], &[]).is_empty());
        p.walked[0] = Instant::now() - Pulses::FLOOR;
        assert!(p.decide(&[Some(2)], &[]).is_empty());
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
        assert!(p.decide(&[Some(2)], &[0]).is_empty());
        p.walked[0] = Instant::now() - Duration::from_secs(1200);
        assert!(matches!(
            p.decide(&[Some(3)], &[0]).as_slice(),
            [(0, Nudge::Blind)]
        ));
    }

    #[test]
    fn retries_pay_for_the_pass_even_when_another_deadline_is_due() {
        let mut p = quiet(1);
        p.scanned(0, Duration::from_secs(60));
        p.safety[0] = Instant::now();
        assert!(!p.decide(&[None], &[]).is_empty());
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
}
