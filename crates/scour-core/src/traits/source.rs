//! Anything that can list entries and say when they change.

use std::fmt::Debug;
use std::io::Read;

use crate::types::{
    Caps, Change, Entry, EntryId, Result, ScanOptions, ScanReport, SourceId, SourceInfo,
};

/// Whether a walk should keep going.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flow {
    Continue,
    /// Stop the walk: cancellation, and bounded queries that must not pay for a
    /// million entries to answer with a hundred.
    Stop,
}

impl Flow {
    pub fn is_stop(self) -> bool {
        self == Flow::Stop
    }
}

/// Where a scan delivers what it finds: a sink rather than a returned `Vec`, so a
/// home directory's hundreds of thousands of entries never exist at once, and the
/// caller gets backpressure and cancellation through [`Flow`].
pub trait EntrySink {
    fn push(&mut self, entry: Entry) -> Flow;

    /// A directory could not be read. Reported rather than swallowed, so that a gap
    /// in the index is visible.
    fn unreadable(&mut self, _path: &str, _error: &crate::types::Error) {}
}

/// Where a watcher delivers changes. Called from the watcher's own thread.
pub trait ChangeSink: Send + Sync + Debug {
    fn emit(&self, change: Change);
}

/// Keeps a watch alive. Dropping it stops the watch.
pub trait WatchHandle: Send + Debug {
    /// Subtrees the watcher could not cover, and so is not reporting on. Empty is the
    /// ordinary answer; when it is not, live updates are partial and the caller needs
    /// the paths rather than a count to say what is missing.
    fn unwatched(&self) -> Vec<String> {
        Vec::new()
    }

    /// Start watching a subtree that appeared after the watcher did. A no-op where the
    /// watch is recursive; a shallow watch rebuilt around an unreadable directory does
    /// not cover children created later. Called once a walk has reached the path.
    fn cover(&self, _path: &str) {}

    /// The rules changed; watch by these from now on. A watcher still filtering by the
    /// start-up rules puts back everything a new exclusion just swept. Must be safe to
    /// rebuild while events are arriving; a no-op for a backend that filters nothing.
    fn retune(&self, _opts: &ScanOptions) {}

    /// Stop watching. Dropping does the same; this exists so a caller can wait
    /// for the watcher's threads to finish.
    fn stop(self: Box<Self>);
}

/// A place entries come from: a filesystem, an object store, an archive.
pub trait Source: Send + Sync + Debug {
    fn id(&self) -> SourceId;

    fn describe(&self) -> SourceInfo;

    fn caps(&self) -> Caps;

    /// A microsecond-cost number that changes when this source may have changed: it
    /// says *whether* to look, not what, and a pulse moving while no change arrives
    /// exposes a silent watcher. `None` means no cheap answer — poll on a timer.
    fn pulse(&self) -> Option<u64> {
        None
    }

    /// Walk everything and push it into the sink. Returns when the walk is done or
    /// the sink asked it to stop; the report says which.
    fn scan(&self, opts: &ScanOptions, sink: &mut dyn EntrySink) -> Result<ScanReport>;

    /// Would a walk under these options have skipped this path? Lets a rule change be
    /// applied to rows already indexed instead of by walking. `is_dir` is part of the
    /// answer: a `dir:` rule must not exclude a symlink that shares the name.
    fn excluder(
        &self,
        _opts: &ScanOptions,
    ) -> Option<Box<dyn Fn(&str, bool) -> bool + Send + Sync>> {
        None
    }

    /// Start reporting changes, under the **same options as [`Source::scan`]**: a
    /// watcher that ignores the exclusions reports files the walk skipped. Unsupported
    /// without `Caps::WATCH`; a watcher that loses track emits `Change::Rescan`.
    fn watch(&self, opts: &ScanOptions, sink: Box<dyn ChangeSink>) -> Result<Box<dyn WatchHandle>>;

    /// Look at these paths again now: one `stat` each, no directory listed, no tree
    /// descended. For a change this program just made and must not wait on the
    /// watcher to see. Returns how many were looked at; zero means nothing was.
    fn recheck(&self, _paths: &[String], _sink: &dyn ChangeSink) -> usize {
        0
    }

    /// Read an entry's bytes, for content extraction. Sources without `Caps::CONTENT`
    /// return [`Error::Unsupported`](crate::types::Error::Unsupported).
    fn open(&self, id: &EntryId) -> Result<Box<dyn Read + Send>>;

    /// Look up one entry by path, without walking.
    fn stat(&self, path: &str) -> Result<Entry>;
}
