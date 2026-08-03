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

use notify::{Event, EventKind, RecursiveMode, Watcher, event::RemoveKind};
use scour_core::{Change, ChangeSink, Error, Result, WatchHandle};

use crate::path;
use crate::scan::FsSource;

pub fn start(source: FsSource, sink: Box<dyn ChangeSink>) -> Result<Box<dyn WatchHandle>> {
    let sink: Arc<dyn ChangeSink> = Arc::from(sink);
    let roots: Vec<_> = source.roots().to_vec();
    let id = source.source_id();
    let stable_ids = source.stable_ids();

    let handler = {
        let sink = Arc::clone(&sink);
        move |res: notify::Result<Event>| match res {
            Ok(event) => translate(id, stable_ids, &event, sink.as_ref()),
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
    for r in &roots {
        match watcher.watch(r, RecursiveMode::Recursive) {
            Ok(()) => watched += 1,
            Err(e) => {
                // One unwatchable root does not make the rest unwatchable. Say
                // it needs rescanning and carry on.
                sink.emit(Change::Rescan {
                    path: path::from_path(r),
                });
                let _ = e;
            }
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
    }))
}

/// Turn one `notify` event into changes.
///
/// A create or a modify becomes an upsert *after re-examining the path*, never
/// from the event's own description: by the time this runs the file may have
/// been changed again, or removed, and the event says only where to look.
fn translate(id: scour_core::SourceId, stable_ids: bool, event: &Event, sink: &dyn ChangeSink) {
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
            for p in &event.paths {
                upsert(p);
            }
        }
        // A rename arrives as one event with two paths, or as two events. Both
        // are handled by looking at every path mentioned: the one that no
        // longer exists is removed, the one that does is upserted.
        EventKind::Modify(_) => {
            for p in &event.paths {
                upsert(p);
            }
        }
        EventKind::Remove(kind) => {
            for p in &event.paths {
                let path = path::from_path(p);
                // Removing a path also removes everything under it. For a file
                // that is the file alone; for a directory it is one term in the
                // index rather than a walk. The event does not always say
                // which, and it does not need to.
                let _ = kind == RemoveKind::Folder;
                sink.emit(Change::RemoveSubtree { path });
            }
        }
        // Backends emit these when they have lost track — a queue overflowed,
        // a watch was dropped. Look again rather than guess.
        EventKind::Any | EventKind::Other => {
            for p in &event.paths {
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
}

impl std::fmt::Debug for FsWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsWatch")
            .field("stopped", &self.stopped.load(Ordering::Relaxed))
            .finish()
    }
}

impl WatchHandle for FsWatch {
    fn stop(self: Box<Self>) {
        self.stopped.store(true, Ordering::Relaxed);
        drop(self.watcher);
    }
}
