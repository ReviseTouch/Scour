//! How much one question may read, shared by every thread answering it.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

/// A ceiling on bytes and on time, and a way to stop from outside. A file
/// already being read is finished, so the ceiling is passed by at most one
/// file a thread.
#[derive(Debug)]
pub struct Budget {
    bytes: u64,
    until: Option<Instant>,
    spent: AtomicU64,
    stopped: AtomicBool,
}

impl Budget {
    /// `bytes` of zero is no ceiling on bytes.
    pub fn new(bytes: u64, until: Option<Instant>) -> Budget {
        Budget {
            bytes,
            until,
            spent: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
        }
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
        self.spent.fetch_add(bytes, Ordering::Relaxed);
    }
}
