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

    /// The rules changed; watch by these from now on.
    ///
    /// **A watcher filters, and until this existed it filtered by whatever it
    /// was told at start-up.** That was invisible while the rules could only
    /// be edited in a file the service reads once. Now a window can add one,
    /// and a watcher still holding the old set is the exact failure the rule
    /// was added to prevent: somebody excludes `target`, the index is swept
    /// clean of it, the next build writes two million files, and the watcher
    /// puts every one of them back.
    ///
    /// A default that does nothing, because a mechanism that does no filtering
    /// of its own has nothing to re-tune. Whatever a backend rebuilds here has
    /// to be safe to rebuild while events are arriving.
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

    /// A number that changes when this source may have changed — cheaply.
    ///
    /// **What it is for.** Watching costs one inotify watch per directory on
    /// Linux and the budget is shared with every other program the user runs;
    /// a source that is mostly still does not deserve that, but something has
    /// to notice when it moves. This is that something: a token to compare
    /// against the last one, costing microseconds, that says *whether* to look
    /// rather than *what* changed.
    ///
    /// It is also a way to catch a watcher that has gone quiet when it should
    /// not have. inotify's one dangerous failure is silent — the budget fills,
    /// the directory is never watched, and nothing reports it. A pulse that
    /// keeps moving while no change arrives is that failure, visible.
    ///
    /// `None` when the source has no cheap way to answer, which is not an
    /// error: the caller falls back to asking on a timer.
    fn pulse(&self) -> Option<u64> {
        None
    }

    /// Walk everything and push it into the sink.
    ///
    /// Returns when the walk is done or the sink asked it to stop; the report
    /// says which.
    fn scan(&self, opts: &ScanOptions, sink: &mut dyn EntrySink) -> Result<ScanReport>;

    /// A test for "would a walk under these options have skipped this path".
    ///
    /// **So that a new rule can be applied without walking the disk.** Adding
    /// an exclusion only ever *removes* entries, and the index already holds
    /// every path the answer is about — so the whole job is a pass over rows
    /// that are already in memory, against a test only the source can perform.
    /// A walk of two volumes to delete rows nobody had to go and look at is
    /// the wrong price by an order of magnitude.
    ///
    /// Returned as a closure because building the test is the expensive part —
    /// a rule set is compiled once and then asked millions of times — and
    /// because it keeps whatever a source uses to answer inside that source.
    /// The engine calls this and nothing else; it has never named a rule type
    /// and does not start here.
    ///
    /// **`is_dir` is not optional information, it is the answer.** A rule that
    /// names a directory does not name a file that happens to share the name,
    /// and the difference is not hypothetical: `node_modules` is a *symlink*
    /// to `nodejs` in 36 places under this machine's container storage. The
    /// walk indexes those — a `dir:` rule is not about them — and a test that
    /// left `is_dir` out called every one of them excluded, so every rule
    /// change tried to delete rows the next walk would put straight back.
    ///
    /// The default says "nothing is skipped", which is the honest answer for a
    /// source with no notion of exclusions: it means the caller walks instead,
    /// rather than quietly deleting nothing and calling the index reconciled.
    fn excluder(
        &self,
        _opts: &ScanOptions,
    ) -> Option<Box<dyn Fn(&str, bool) -> bool + Send + Sync>> {
        None
    }

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

    /// Look at these paths again, now, and report what they are.
    ///
    /// **For the moment something outside the index changes a file.** A watcher
    /// finds that out on its own schedule — on this machine three to eight
    /// seconds later, most of it the index's own write interval — and that is
    /// the right price for a change nobody is waiting on. It is the wrong price
    /// for a change somebody just made from inside this program: a row that
    /// stays on screen for six seconds after being sent to the trash reads as a
    /// deletion that failed.
    ///
    /// So this is the narrow version of a walk: one `stat` a path, no directory
    /// listed, no tree descended — the same look the watcher takes when it is
    /// told about a single file. Gone means gone, present means updated.
    ///
    /// Returns how many paths were looked at. **The default is zero**, which is
    /// the honest answer for a source with no cheap way to check one path: it
    /// tells the caller to fall back to a walk rather than believing the index
    /// has been reconciled when nothing happened.
    fn recheck(&self, _paths: &[String], _sink: &dyn ChangeSink) -> usize {
        0
    }

    /// Read an entry's bytes, for content extraction.
    ///
    /// Sources without [`Caps::CONTENT`] return [`Error::Unsupported`].
    ///
    /// [`Error::Unsupported`]: crate::types::Error::Unsupported
    fn open(&self, id: &EntryId) -> Result<Box<dyn Read + Send>>;

    /// Look up one entry by path, without walking.
    fn stat(&self, path: &str) -> Result<Entry>;
}
