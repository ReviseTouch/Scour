//! Watching for changes.
//!
//! Every watching mechanism gives up somewhere, and each gives up differently.
//! inotify needs one watch per directory and a home directory can exhaust the
//! per-user limit. Windows keeps a fixed kernel buffer per handle and drops
//! events when it overflows. macOS coalesces, and hides events for files the
//! process does not own. `notify` papers over the API differences but cannot
//! paper over that.
//!
//! So the contract here is deliberately weak, and stated rather than implied:
//! **an event is a hint to look again, never a description of what happened.**
//! Every path that arrives is re-examined against the filesystem. When the
//! mechanism reports that it lost track, [`Change::Rescan`] says so and the
//! engine walks the subtree again. That single variant is what lets the layer
//! above stay free of platform knowledge.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use notify::{Event, EventKind, RecursiveMode, Watcher};
use scour_core::{Change, ChangeSink, Error, Result, ScanOptions, WatchHandle};

use crate::path;
use crate::rules::Rules;
use crate::scan::FsSource;

pub fn start(
    source: FsSource,
    opts: &ScanOptions,
    sink: Box<dyn ChangeSink>,
) -> Result<Box<dyn WatchHandle>> {
    let sink: Arc<dyn ChangeSink> = Arc::from(sink);
    let roots: Vec<_> = source.roots().to_vec();
    let id = source.source_id();
    let stable_ids = source.stable_ids();
    // The same rules the walk uses. Without them the watcher reports changes
    // for files the walk skips, and every one is an entry that exists until
    // something else removes it — a `cargo test` under a watched but unscanned
    // build directory took a query here from 8 ms to 13 seconds.
    let rules = Arc::new(Rules::from_options(opts));

    let handler = {
        let sink = Arc::clone(&sink);
        let rules = Arc::clone(&rules);
        move |res: notify::Result<Event>| match res {
            Ok(event) => translate(id, stable_ids, &rules, &event, sink.as_ref()),
            Err(e) => {
                // The interesting failures are the ones that mean "I stopped
                // seeing things": inotify running out of watches, a Windows
                // buffer overflow. `notify` reports them here, and the honest
                // answer to all of them is the same.
                for p in &e.paths {
                    sink.emit(Change::Rescan {
                        path: path::from_path(p),
                    });
                }
                if e.paths.is_empty() {
                    sink.emit(Change::Rescan {
                        path: String::new(),
                    });
                }
            }
        }
    };

    let mut watcher = notify::recommended_watcher(handler).map_err(|e| Error::Io {
        detail: format!("cannot start a watcher: {e}"),
    })?;

    let mut watched = 0usize;
    let mut skipped = Vec::new();
    for r in &roots {
        if cover(&mut watcher, r, sink.as_ref(), &mut skipped, 0) {
            watched += 1;
        }
    }
    if watched == 0 && !roots.is_empty() {
        return Err(Error::Io {
            detail: "no root could be watched".into(),
        });
    }
    Ok(Box::new(FsWatch {
        watcher,
        stopped: AtomicBool::new(false),
        skipped,
    }))
}

/// How far down to keep splitting a refused directory.
///
/// Each level costs a directory listing and a re-walk of the branches that
/// still work, so this bounds the repair rather than the tree. Six is deeper
/// than any real accident — `~/.local/share/waydroid/data/vendor`, the one that
/// prompted all of this, is five.
const SPLIT_DEPTH: u32 = 6;

/// Watch `dir` and everything under it, going around what cannot be watched.
///
/// Returns whether anything at all was watched beneath it.
///
/// **`notify`'s recursive mode is all or nothing.** It walks the tree itself
/// and abandons the whole watch on the first directory it cannot read — so on
/// this machine a single root-owned `~/.local/share/waydroid/data/vendor` left
/// the entire home directory unwatched, and the status line said `watching 0`
/// with no reason attached to it. The recorded diagnosis was that inotify had
/// run out of watches; the limit here is 524,288 against 342,000 directories,
/// so it never had.
///
/// So: ask for the whole subtree, and only if that is refused ask for each
/// child separately. The branch that is really unreadable is the only one that
/// gets split, and it gets split down to itself rather than costing its
/// siblings anything.
fn cover(
    watcher: &mut notify::RecommendedWatcher,
    dir: &std::path::Path,
    sink: &dyn ChangeSink,
    skipped: &mut Vec<String>,
    depth: u32,
) -> bool {
    if watcher.watch(dir, RecursiveMode::Recursive).is_ok() {
        return true;
    }
    // Out of patience. Readable, so a walk can still cover what a watch will
    // not — say so and let the engine schedule it.
    if depth >= SPLIT_DEPTH {
        skipped.push(path::from_path(dir));
        if std::fs::read_dir(dir).is_ok() {
            sink.emit(Change::Rescan {
                path: path::from_path(dir),
            });
        }
        return false;
    }
    // Not readable at all, which is the ordinary reason a watch is refused —
    // 191 root-owned Waydroid directories, here.
    //
    // **No `Rescan` for these**, and getting that wrong was instructive: a
    // walk needs exactly the permission the watch just did not have, so each
    // one queued a scan that could not read anything, and 191 of them behind
    // one worker thread took a query from 14 ms to 51 seconds. A subtree
    // nobody can read is not pending work. It is recorded and left alone.
    let Ok(children) = std::fs::read_dir(dir) else {
        skipped.push(path::from_path(dir));
        return false;
    };
    // The directory itself, without its contents, so that a file created
    // directly in it is still seen.
    let mut any = watcher.watch(dir, RecursiveMode::NonRecursive).is_ok();
    for child in children.flatten() {
        // Only directories: a file is covered by the watch on its parent, and
        // a symlink is followed by whoever owns the target.
        if child.file_type().is_ok_and(|t| t.is_dir())
            && cover(watcher, &child.path(), sink, skipped, depth + 1)
        {
            any = true;
        }
    }
    any
}

/// Turn one `notify` event into changes.
///
/// A create or a modify becomes an upsert *after re-examining the path*, never
/// from the event's own description: by the time this runs the file may have
/// been changed again, or removed, and the event says only where to look.
fn translate(
    id: scour_core::SourceId,
    stable_ids: bool,
    rules: &Rules,
    event: &Event,
    sink: &dyn ChangeSink,
) {
    // What the walk would not have looked at, this does not report. Checked
    // once here rather than in each arm, because every arm has the same answer
    // and a path that slips through is an index entry nobody asked for.
    let watched = |p: &std::path::Path| -> bool {
        let path = path::from_path(p);
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let is_dir = p.is_dir();
        !rules.excludes(&path, name, is_dir)
    };
    let upsert = |p: &std::path::Path| {
        let path = path::from_path(p);
        match std::fs::symlink_metadata(p) {
            Ok(md) => sink.emit(Change::Upsert(crate::scan::entry_of(
                id,
                &path,
                Some(&md),
                md.is_dir(),
                stable_ids,
            ))),
            // Gone between the event and the look. That is a removal, and it is
            // the common case under any kind of churn.
            Err(_) => sink.emit(Change::RemoveSubtree { path }),
        }
    };

    match event.kind {
        EventKind::Create(_) => {
            for p in event.paths.iter().filter(|p| watched(p)) {
                upsert(p);
            }
        }
        // A rename arrives as one event with two paths, or as two events. Both
        // are handled by looking at every path mentioned: the one that no
        // longer exists is removed, the one that does is upserted.
        EventKind::Modify(_) => {
            for p in event.paths.iter().filter(|p| watched(p)) {
                upsert(p);
            }
        }
        EventKind::Remove(_) => {
            // Removals are **not** filtered, and the asymmetry is deliberate:
            // the rules are checked against a path that no longer exists, so
            // `is_dir` is false whatever it was, and an excluded directory
            // would answer differently on the way out than on the way in.
            // Removing something the index never held costs one lookup that
            // finds nothing; keeping something that is gone is a wrong answer.
            for p in &event.paths {
                let path = path::from_path(p);
                // Removing a path also removes everything under it. For a file
                // that is the file alone; for a directory it is one term in the
                // index rather than a walk. The event does not always say
                // which, and it does not need to.
                sink.emit(Change::RemoveSubtree { path });
            }
        }
        // Backends emit these when they have lost track — a queue overflowed,
        // a watch was dropped. Look again rather than guess.
        EventKind::Any | EventKind::Other => {
            for p in event.paths.iter().filter(|p| watched(p)) {
                sink.emit(Change::Rescan {
                    path: path::from_path(p),
                });
            }
        }
        EventKind::Access(_) => {}
    }
}

struct FsWatch {
    watcher: notify::RecommendedWatcher,
    stopped: AtomicBool,
    /// Subtrees nothing is watching, because they could not be read.
    ///
    /// Kept rather than counted: "live updates are off somewhere" is not
    /// something a user can act on, and `~/.local/share/waydroid/data` is.
    skipped: Vec<String>,
}

impl std::fmt::Debug for FsWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsWatch")
            .field("stopped", &self.stopped.load(Ordering::Relaxed))
            .field("skipped", &self.skipped)
            .finish()
    }
}

impl WatchHandle for FsWatch {
    fn unwatched(&self) -> Vec<String> {
        self.skipped.clone()
    }

    fn stop(self: Box<Self>) {
        self.stopped.store(true, Ordering::Relaxed);
        drop(self.watcher);
    }
}
