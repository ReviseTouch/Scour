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
    /// Stop the walk. Used for cancellation and for bounded queries — a caller
    /// asking for the first hundred entries under a directory should not pay
    /// for a million.
    Stop,
}

impl Flow {
    pub fn is_stop(self) -> bool {
        self == Flow::Stop
    }
}

/// Where a scan delivers what it finds.
///
/// A sink rather than a returned `Vec` because a scan of a home directory
/// produces hundreds of thousands of entries and there is no reason for all of
/// them to exist at once. It also gives the caller backpressure and
/// cancellation for free, through the returned [`Flow`].
pub trait EntrySink {
    fn push(&mut self, entry: Entry) -> Flow;

    /// A directory could not be read. Reported rather than swallowed so that
    /// "the index is missing things" is visible instead of mysterious.
    fn unreadable(&mut self, _path: &str, _error: &crate::types::Error) {}
}

/// Where a watcher delivers changes. Called from the watcher's own thread.
pub trait ChangeSink: Send + Sync + Debug {
    fn emit(&self, change: Change);
}

/// Keeps a watch alive. Dropping it stops the watch.
pub trait WatchHandle: Send + Debug {
    /// Subtrees the watcher could not cover, and so is not reporting on.
    ///
    /// Empty is the ordinary answer. When it is not, live updates are partial,
    /// and the caller has to decide what to do about that — which it cannot do
    /// if the only thing it is told is a count. One unreadable directory used
    /// to take a whole home directory's watching down and report `watching 0`
    /// with no reason attached, so this exists to carry the reason.
    fn unwatched(&self) -> Vec<String> {
        Vec::new()
    }

    /// Start watching a subtree that appeared after the watcher did.
    ///
    /// A default that does nothing, because only some mechanisms need it: a
    /// recursive watch covers whatever is created below it, so Windows and
    /// macOS have nothing to do here. Linux does. When inotify refuses a whole
    /// tree — one unreadable directory is enough — the cover is rebuilt as a
    /// shallow watch on the parent and a recursive watch on each child that
    /// existed *then*. A directory created in that parent afterwards is
    /// reported once and never watched, and everything inside it stays
    /// invisible until somebody rescans by hand.
    ///
    /// The engine calls this after walking a subtree, which is the moment it
    /// already knows the path is real and worth the syscalls.
    fn cover(&self, _path: &str) {}

    /// Stop watching. Dropping does the same; this exists so a caller can wait
    /// for the watcher's threads to finish.
    fn stop(self: Box<Self>);
}

/// A place entries come from: a filesystem, an object store, an archive.
pub trait Source: Send + Sync + Debug {
    fn id(&self) -> SourceId;

    fn describe(&self) -> SourceInfo;

    fn caps(&self) -> Caps;

    /// Walk everything and push it into the sink.
    ///
    /// Returns when the walk is done or the sink asked it to stop; the report
    /// says which.
    fn scan(&self, opts: &ScanOptions, sink: &mut dyn EntrySink) -> Result<ScanReport>;

    /// Start reporting changes.
    ///
    /// Takes the **same options as [`Source::scan`]**, and that is the point
    /// rather than a convenience: watching and scanning have to agree about
    /// what is inside the source. A watcher that ignores the exclusions
    /// reports changes for files the walk deliberately skipped, and every one
    /// of them is an entry the next walk will not renew. Measured: with a
    /// build directory watched but not scanned, one `cargo test` took a query
    /// from 8 ms to 13 seconds.
    ///
    /// Sources without [`Caps::WATCH`] return [`Error::Unsupported`]. Sources
    /// that have it are still allowed to give up: when a watching mechanism
    /// loses track — inotify running out of watches, a Windows buffer
    /// overflowing, a cloud poll missing a window — it emits
    /// [`Change::Rescan`] rather than pretending nothing happened.
    ///
    /// [`Error::Unsupported`]: crate::types::Error::Unsupported
    /// [`Change::Rescan`]: crate::types::Change::Rescan
    fn watch(&self, opts: &ScanOptions, sink: Box<dyn ChangeSink>) -> Result<Box<dyn WatchHandle>>;

    /// Read an entry's bytes, for content extraction.
    ///
    /// Sources without [`Caps::CONTENT`] return [`Error::Unsupported`].
    ///
    /// [`Error::Unsupported`]: crate::types::Error::Unsupported
    fn open(&self, id: &EntryId) -> Result<Box<dyn Read + Send>>;

    /// Look up one entry by path, without walking.
    fn stat(&self, path: &str) -> Result<Entry>;
}
