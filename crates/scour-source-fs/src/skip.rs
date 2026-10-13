//! The rules as one source reads them.
//!
//! It does not go into other sources' roots under its own: the walk crosses
//! mounts, so `/` walked as one disk would walk `/home` and `/mnt/data` too,
//! which are disks of their own and indexed as such.
//!
//! And a path rule over the whole of it is not its rule: `/run` is skipped
//! everywhere, and a stick a desktop mounts under `/run/media` is a source
//! of its own.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use scour_core::{ScanOptions, WatchHandle};

/// The scan options with this source's skips added to what everybody skips,
/// and without the path rules that would hide all of it.
pub fn own<'a>(opts: &'a ScanOptions, roots: &[PathBuf], skip: &[String]) -> Cow<'a, ScanOptions> {
    let covers = |rule: &String| roots.iter().any(|r| r.starts_with(Path::new(rule)));
    if skip.is_empty() && !opts.exclude_paths.iter().any(covers) {
        return Cow::Borrowed(opts);
    }
    let mut mine = opts.clone();
    mine.exclude_paths.retain(|rule| !covers(rule));
    mine.deny.extend(skip.iter().cloned());
    Cow::Owned(mine)
}

/// A watch that keeps reading the rules this way when they are edited:
/// `retune` is handed the shared options, which know nothing of the source.
#[derive(Debug)]
pub struct Skipping {
    pub inner: Box<dyn WatchHandle>,
    pub roots: Vec<PathBuf>,
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
        self.inner.retune(&own(opts, &self.roots, &self.skip));
    }

    fn stop(self: Box<Self>) {
        self.inner.stop();
    }
}
