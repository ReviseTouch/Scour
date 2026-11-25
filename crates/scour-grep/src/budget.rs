//! How much one question may read, shared by every thread answering it.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// A ceiling on bytes and on time, and a way to stop from outside. A file
/// already being read is finished, so the ceiling is passed by at most one
/// file a thread.
#[derive(Debug)]
pub struct Budget {
    bytes: u64,
    until: Option<Instant>,
    spent: AtomicU64,
    stopped: AtomicBool,
    /// Bytes a second at most, zero for as fast as the disk goes.
    rate: u64,
    began: Instant,
}

impl Budget {
    /// `bytes` of zero is no ceiling on bytes.
    pub fn new(bytes: u64, until: Option<Instant>) -> Budget {
        Budget {
            bytes,
            until,
            spent: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
            rate: 0,
            began: Instant::now(),
        }
    }

    /// No faster than `bytes_per_second`, over the whole reading: a thread
    /// ahead of the pace waits before its next file.
    pub fn paced(mut self, bytes_per_second: u64) -> Budget {
        self.rate = bytes_per_second;
        self
    }

    pub fn unlimited() -> Budget {
        Budget::new(0, None)
    }

    /// Bytes read so far.
    pub fn spent(&self) -> u64 {
        self.spent.load(Ordering::Relaxed)
    }

    /// No file is started after this.
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
    }

    /// Whether another file may be started.
    pub fn exhausted(&self) -> bool {
        self.stopped.load(Ordering::Relaxed)
            || (self.bytes != 0 && self.spent() >= self.bytes)
            || self.until.is_some_and(|t| Instant::now() >= t)
    }

    pub(crate) fn charge(&self, bytes: u64) {
        let spent = self.spent.fetch_add(bytes, Ordering::Relaxed) + bytes;
        if self.rate == 0 {
            return;
        }
        let due = Duration::from_secs_f64(spent as f64 / self.rate as f64);
        let ahead = due.saturating_sub(self.began.elapsed());
        // Not past the deadline: a paced reading still ends when it must.
        let ahead = match self.until {
            Some(t) => ahead.min(t.saturating_duration_since(Instant::now())),
            None => ahead,
        };
        if !ahead.is_zero() {
            std::thread::sleep(ahead.min(Duration::from_secs(1)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paced_budget_holds_its_readers_back() {
        let budget = Budget::unlimited().paced(10 << 20);
        let began = Instant::now();
        budget.charge(2 << 20);
        // Two megabytes at ten a second: a fifth of a second, less what has passed.
        assert!(
            began.elapsed() >= Duration::from_millis(150),
            "{:?}",
            began.elapsed()
        );
        let fast = Budget::unlimited();
        let began = Instant::now();
        fast.charge(1 << 30);
        assert!(began.elapsed() < Duration::from_millis(50));
    }
}
