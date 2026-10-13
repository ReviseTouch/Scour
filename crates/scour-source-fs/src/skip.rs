//! The places a source does not go into: other sources' roots under its own.
//!
//! The walk crosses mounts, so `/` walked as one disk would walk `/home` and
//! `/mnt/data` too, which are disks of their own and indexed as such.

use std::borrow::Cow;

use scour_core::{ScanOptions, WatchHandle};

/// The scan options with this source's skips added to what everybody skips.
pub fn own<'a>(opts: &'a ScanOptions, skip: &[String]) -> Cow<'a, ScanOptions> {
    if skip.is_empty() {
        return Cow::Borrowed(opts);
    }
    let mut mine = opts.clone();
    mine.deny.extend(skip.iter().cloned());
    Cow::Owned(mine)
}

/// A watch that keeps the skips when the rules are edited: `retune` is handed
/// the shared options, which know nothing of them.
#[derive(Debug)]
pub struct Skipping {
    pub inner: Box<dyn WatchHandle>,
    pub skip: Vec<String>,
}

impl WatchHandle for Skipping {
    fn unwatched(&self) -> Vec<String> {
        self.inner.unwatched()
    }

    fn cover(&self, path: &str) {
        self.inner.cover(path);
    }

    fn retune(&self, opts: &ScanOptions) {
        self.inner.retune(&own(opts, &self.skip));
    }

    fn stop(self: Box<Self>) {
        self.inner.stop();
    }
}
