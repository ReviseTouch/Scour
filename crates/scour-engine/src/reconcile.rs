//! Recovery scheduling for sources whose change feed is incomplete.

use std::time::{Duration, Instant};

/// What the tick decided a source needs.
#[derive(Debug)]
pub(crate) enum Nudge {
    /// Nobody is watching it and it moved.
    Reconcile,
    /// Somebody is watching it, it keeps moving, and nothing is arriving.
    Blind,
    /// Somebody is watching it and nothing is wrong: the slow check that finds
    /// what a watcher cannot see, on a quiet machine and gently.
    Check,
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
    /// Each source's own least wait, [`scour_core::Source::patience`].
    patience: Vec<Duration>,
    /// When a change last arrived for each source, so a watcher that has gone
    /// quiet can be told from a disk that is quiet.
    moved_since_event: Vec<u32>,
    checked: Instant,
    poll: Duration,
    reconcile: Duration,
    fallback: Vec<Instant>,
    safety: Vec<Instant>,
    /// For a watched source: how long between its checks, and when its last whole
    /// walk was — the check is counted from any walk, not only from a check.
    watched_reconcile: Duration,
    whole_at: Vec<Instant>,
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

    pub(crate) fn new(
        n: usize,
        poll: Duration,
        reconcile: Duration,
        watched_reconcile: Duration,
    ) -> Pulses {
        Pulses {
            last: vec![None; n],
            pending: vec![false; n],
            // Back-dated: the first movement is acted on, not held by an unearned floor.
            walked: vec![Instant::now() - Self::FLOOR; n],
            pulse_floor: vec![Self::FLOOR; n],
            scan_rest: vec![Duration::ZERO; n],
            patience: vec![Duration::ZERO; n],
            moved_since_event: vec![0; n],
            checked: Instant::now(),
            poll,
            reconcile,
            fallback: vec![Instant::now() + poll; n],
            safety: vec![Instant::now() + reconcile; n],
            watched_reconcile,
            whole_at: vec![Instant::now(); n],
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

    /// A source that waits longer than its cost alone says: see
    /// [`scour_core::Source::patience`].
    pub(crate) fn be_patient(&mut self, source: usize, least: Duration) {
        self.patience[source] = least;
        self.pulse_floor[source] = self.pulse_floor[source].max(least);
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
        self.whole_at[source] = now;
        self.pending[source] = false;
        let rest = self.scan_rest[source];
        self.pulse_floor[source] = Self::FLOOR.max(rest).max(self.patience[source]);
        // Patience bounds the timer too: a share nothing watches has no pulse,
        // and was walked every minute where ten were meant (PC2, 17 November).
        self.fallback[source] = now + self.poll.max(rest).max(self.patience[source]);
        self.safety[source] = now + self.reconcile.max(rest);
    }

    /// A failed pass costs the same I/O as a successful one. Start the wait
    /// after it finishes, including when the pass exceeded the retry delay.
    pub(crate) fn retry_at(&self, source: usize, delay: Duration) -> Instant {
        Instant::now() + delay.max(self.scan_rest[source])
    }

    /// The rules, with the reading already taken. `quiet` says whether the machine
    /// has room for a watched source's check; asked only when one is due.
    pub(crate) fn decide(
        &mut self,
        readings: &[Option<u64>],
        watched: &[usize],
        quiet: &dyn Fn() -> bool,
    ) -> Vec<(usize, Nudge)> {
        self.checked = Instant::now();
        let trace = std::env::var_os("SCOUR_PULSE_TRACE").is_some();
        let mut out = Vec::new();
        for (i, reading) in readings.iter().enumerate() {
            let now = Instant::now();
            // A watched source is checked on its own, much slower clock, once the
            // machine is quiet — or regardless, a whole interval late.
            if watched.contains(&i) {
                let due = self.whole_at[i] + self.watched_reconcile;
                if now >= due && (now >= due + self.watched_reconcile || quiet()) {
                    self.defer(i);
                    self.last[i] = *reading;
                    self.moved_since_event[i] = 0;
                    out.push((i, Nudge::Check));
                    continue;
                }
            }
            // A pulse is a hint: a watcher can miss one subtree while reporting
            // another. Every thirty minutes, for a watched source, was 5 GB of
            // reads a round that found only files written through a mapping.
            let safety_due = !watched.contains(&i) && now >= self.safety[i];
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

/// Whether the machine has room for a slow check: over the last minute, tasks
/// waited on the processor and on the disk for under a tenth of the time. The
/// kernel's pressure figures; elsewhere nothing says, and the answer is yes.
pub(crate) fn machine_quiet() -> bool {
    #[cfg(target_os = "linux")]
    {
        let calm = |what: &str| {
            std::fs::read_to_string(format!("/proc/pressure/{what}"))
                .ok()
                .and_then(|text| waited(&text))
                // A kernel without the figures cannot say it is busy.
                .is_none_or(|share| share < QUIET_BELOW)
        };
        calm("cpu") && calm("io")
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

/// The share of the last minute, in percent, that counts as quiet.
#[cfg(target_os = "linux")]
const QUIET_BELOW: f64 = 10.0;

/// How much of the last minute some task spent waiting, from a pressure file:
/// the `avg60` of its `some` line.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn waited(pressure: &str) -> Option<f64> {
    let some = pressure.lines().find(|l| l.starts_with("some"))?;
    some.split_whitespace()
        .find_map(|f| f.strip_prefix("avg60="))?
        .parse()
        .ok()
}

#[cfg(test)]
mod pulse_tests;
