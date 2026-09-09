//! Watching for changes.
//!
//! The contract is deliberately weak: **an event is a hint to look again, never
//! a description of what happened.** Every path that arrives is re-examined
//! against the filesystem; a backend that lost track sends [`Change::Rescan`].

use std::sync::Arc;
#[cfg(not(target_os = "linux"))]
use std::sync::RwLock;
#[cfg(not(target_os = "linux"))]
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(not(target_os = "linux"))]
use notify::{Event, EventKind, RecursiveMode, Watcher};
use scour_core::{Change, ChangeSink, Error, Result, ScanOptions, WatchHandle};

#[cfg(not(target_os = "linux"))]
use crate::path;
#[cfg(not(target_os = "linux"))]
use crate::rules::Rules;
use crate::scan::FsSource;

/// The rule set the watcher filters by, swappable while it runs.
/// The outer lock lets [`WatchHandle::retune`] replace it; the inner `Arc` lets
/// an event copy the set and release the lock before filtering.
#[cfg(not(target_os = "linux"))]
type SharedRules = Arc<RwLock<Arc<Rules>>>;

/// Watch a source for changes: on Linux a fanotify mark, or nothing at all.
/// There is no inotify fallback — one watch per directory out of a *session*
/// budget (524,288 here for ~609,000 directories) starves the rest of the desktop.
#[cfg(target_os = "linux")]
pub fn start(
    source: FsSource,
    opts: &ScanOptions,
    sink: Box<dyn ChangeSink>,
) -> Result<Box<dyn WatchHandle>> {
    let sink: Arc<dyn ChangeSink> = Arc::from(sink);
    // The mark needs `CAP_SYS_ADMIN`, which this process must not hold: a helper
    // places it and passes the descriptor in. Unwatched is not unattended — a
    // root with a block device is walked when its pulse moves; a FUSE one is not.
    if let Some(started) = crate::fanotify::try_start(&source, opts, Arc::clone(&sink)) {
        return started;
    }
    let name = scour_core::Source::describe(&source).name;
    scour_core::note!(
        "scourd: no fanotify mark for {name} — it will not be watched live. On a root with \
         a block device behind it, changes are still found by walking when the source's \
         pulse moves, which is slower to notice; on a network or FUSE mount there is no \
         pulse to read and it will not be reconciled at all. \
         To watch it properly: `sudo scour-watch -- scourd`.",
    );
    Err(Error::unsupported(
        "no fanotify descriptor — run `sudo scour-watch -- scourd`",
    ))
}

/// Watch a source for changes, through whatever the platform offers.
/// Not Linux: there the only mechanism is fanotify — see the other `start`.
#[cfg(not(target_os = "linux"))]
pub fn start(
    source: FsSource,
    opts: &ScanOptions,
    sink: Box<dyn ChangeSink>,
) -> Result<Box<dyn WatchHandle>> {
    let sink: Arc<dyn ChangeSink> = Arc::from(sink);

    let roots: Vec<_> = source.roots().to_vec();
    let id = source.source_id();
    let real_modes = source.real_modes();
    // The same rules the walk uses, or the watcher indexes what the walk skips.
    // Behind a lock: [`WatchHandle::retune`] can replace them while this runs.
    let rules: SharedRules = Arc::new(RwLock::new(Arc::new(Rules::from_options(opts))));

    let handler = {
        let sink = Arc::clone(&sink);
        let rules = Arc::clone(&rules);
        move |res: notify::Result<Event>| match res {
            Ok(event) => {
                // Copied, not held: filtering under the lock queues events behind a `retune`.
                let held = match rules.read() {
                    Ok(r) => Arc::clone(&r),
                    Err(p) => Arc::clone(&p.into_inner()),
                };
                translate(id, real_modes, &held, &event, &sink)
            }
            Err(e) => {
                // "I stopped seeing things" — watches exhausted, a buffer overflow.
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

    // The watcher must follow the same links the walk does: a recursive watch
    // through a link to `/` reports paths no walk will ever produce.
    let mut watcher = notify::RecommendedWatcher::new(
        handler,
        notify::Config::default().with_follow_symlinks(opts.follow_symlinks),
    )
    .map_err(|e| Error::Io {
        detail: format!("cannot start a watcher: {e}"),
    })?;

    let mut watched = 0usize;
    let mut skipped = Vec::new();
    // Carry out why the last root was refused: `notify` names it, usually the watch budget.
    let mut refused: Option<String> = None;
    for r in &roots {
        match cover(
            &mut watcher,
            r,
            sink.as_ref(),
            &mut skipped,
            0,
            opts.follow_symlinks,
        ) {
            Covered::Yes => watched += 1,
            Covered::No(why) => {
                if let Some(why) = why {
                    refused = Some(why);
                }
            }
        }
    }
    if watched == 0 && !roots.is_empty() {
        return Err(Error::Io {
            detail: match refused {
                Some(why) => format!("no root could be watched: {why}"),
                None => "no root could be watched".into(),
            },
        });
    }
    Ok(Box::new(FsWatch {
        watcher: std::sync::Mutex::new(watcher),
        stopped: AtomicBool::new(false),
        skipped: std::sync::Mutex::new(skipped),
        follow_symlinks: opts.follow_symlinks,
        rules,
    }))
}

/// How far down to keep splitting a refused directory.
/// Each level costs a listing and a re-walk; six is deeper than any real accident.
#[cfg(not(target_os = "linux"))]
const SPLIT_DEPTH: u32 = 6;

/// How many uncovered subtrees are named before the list stops growing.
/// [`WatchHandle::cover`] appends one per directory the engine discovers, and the
/// paths are distinct, so the `dedup` beside it is no bound; this is.
#[cfg(not(target_os = "linux"))]
const MAX_SKIPPED: usize = 1_024;

/// Remember an uncovered subtree, up to [`MAX_SKIPPED`] of them.
#[cfg(not(target_os = "linux"))]
fn remember(skipped: &mut Vec<String>, path: String) {
    if skipped.len() < MAX_SKIPPED {
        skipped.push(path);
    }
}

/// What happened to one attempt at covering a directory.
/// The `String` is the watcher's own words — `OS file watch limit reached`, most often.
#[cfg(not(target_os = "linux"))]
enum Covered {
    Yes,
    No(Option<String>),
}

/// Watch `dir` and everything under it, going around what cannot be watched.
/// `notify`'s recursive mode is all or nothing — one unreadable directory
/// abandons the whole watch — so a refused subtree is split child by child.
#[cfg(not(target_os = "linux"))]
fn cover(
    watcher: &mut notify::RecommendedWatcher,
    dir: &std::path::Path,
    sink: &dyn ChangeSink,
    skipped: &mut Vec<String>,
    depth: u32,
    follow_symlinks: bool,
) -> Covered {
    // A link is covered by whoever owns its target, exactly as in the walk.
    if !follow_symlinks
        && depth > 0
        && std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink())
    {
        return Covered::No(None);
    }
    let refused = match watcher.watch(dir, RecursiveMode::Recursive) {
        Ok(()) => return Covered::Yes,
        Err(e) => e.to_string(),
    };
    // Out of patience. Readable, so a walk can still cover what a watch cannot.
    if depth >= SPLIT_DEPTH {
        remember(skipped, path::from_path(dir));
        if std::fs::read_dir(dir).is_ok() {
            sink.emit(Change::Rescan {
                path: path::from_path(dir),
            });
        }
        return Covered::No(Some(refused));
    }
    // Unreadable, the ordinary reason a watch is refused. **No `Rescan`**: a walk
    // needs the permission the watch just lacked, so it is not pending work.
    let Ok(children) = std::fs::read_dir(dir) else {
        remember(skipped, path::from_path(dir));
        return Covered::No(Some(refused));
    };
    let mut any = watcher.watch(dir, RecursiveMode::NonRecursive).is_ok();
    for child in children.flatten() {
        // A file is covered by the watch on its parent; a link, by its target.
        if child.file_type().is_ok_and(|t| t.is_dir())
            && matches!(
                cover(
                    watcher,
                    &child.path(),
                    sink,
                    skipped,
                    depth + 1,
                    follow_symlinks,
                ),
                Covered::Yes
            )
        {
            any = true;
        }
    }
    if any {
        Covered::Yes
    } else {
        Covered::No(Some(refused))
    }
}

/// Look at one path and say what changed there; `None` if it is unreadable.
/// Both backends end at this `stat`: neither an event nor a fanotify mask says
/// what happened. `fresh` — a create or a rename's destination — also walks.
pub(crate) fn look(
    id: scour_core::SourceId,
    real_modes: bool,
    path: &str,
    fresh: bool,
    sink: &dyn ChangeSink,
) -> Option<std::fs::Metadata> {
    match std::fs::symlink_metadata(path) {
        Ok(md) => {
            sink.emit(Change::Upsert(crate::scan::entry_of(
                id,
                path,
                Some(&md),
                md.is_dir(),
                real_modes,
            )));
            // A directory that has just appeared is a subtree, not a row: what is
            // created inside it before a watch exists produces no event, and a
            // btrfs snapshot exposes a whole tree behind one (1 event, 201 files).
            if fresh && md.is_dir() {
                sink.emit(Change::Rescan {
                    path: path.to_owned(),
                });
            }
            return Some(md);
        }
        // Gone between the event and the look: a removal, and the common case.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => sink.emit(Change::RemoveSubtree {
            path: path.to_owned(),
        }),
        // Everything else is "I could not look", not a deletion: removing on that
        // evidence hides a tree that is still there. Ask for a walk instead.
        Err(_) => sink.emit(Change::Rescan {
            path: path.to_owned(),
        }),
    }
    None
}

/// Turn one `notify` event into changes, re-examining every path it names.
#[cfg(not(target_os = "linux"))]
fn translate(
    id: scour_core::SourceId,
    real_modes: bool,
    rules: &Rules,
    event: &Event,
    sink: &Arc<dyn ChangeSink>,
) {
    // What the walk would not look at, this does not report — from the **path
    // alone**: an `is_dir()` here cost 74% of a core while a build ran.
    let watched = |p: &std::path::Path| -> bool { !rules.excludes_path(&path::from_path(p)) };
    let upsert = |p: &std::path::Path, fresh: bool| {
        let text = path::from_path(p);
        // A write through a shared mapping produces no event here either. See [`crate::revisit`].
        let md = look(id, real_modes, &text, fresh, sink.as_ref());
        let _ = &md;
        crate::revisit::note(&text, id, real_modes, sink, md.as_ref());
    };

    // The backend lost track. It arrives as `EventKind::Other` with **no paths**,
    // and an empty path is how the engine is told to walk everything.
    if event.need_rescan() {
        let mut any = false;
        for p in event.paths.iter().filter(|p| watched(p)) {
            sink.emit(Change::Rescan {
                path: path::from_path(p),
            });
            any = true;
        }
        if !any {
            sink.emit(Change::Rescan {
                path: String::new(),
            });
        }
        return;
    }

    match event.kind {
        EventKind::Create(_) => {
            for p in event.paths.iter().filter(|p| watched(p)) {
                upsert(p, true);
            }
        }
        // A rename arrives as one event with two paths, or as two events; either
        // way its destination is fresh — a populated directory can arrive that way.
        EventKind::Modify(notify::event::ModifyKind::Name(_)) => {
            for p in event.paths.iter().filter(|p| watched(p)) {
                upsert(p, true);
            }
        }
        EventKind::Modify(_) => {
            for p in event.paths.iter().filter(|p| watched(p)) {
                upsert(p, false);
            }
        }
        EventKind::Remove(_) => {
            // Removals are **not** filtered: the rules see a path that no longer
            // exists, so `is_dir` is false whatever it was.
            for p in &event.paths {
                let path = path::from_path(p);
                // Removing a path removes everything under it: one index term, not a walk.
                sink.emit(Change::RemoveSubtree { path });
            }
        }
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

#[cfg(not(target_os = "linux"))]
struct FsWatch {
    /// Behind a lock because [`WatchHandle::cover`] extends the cover after the fact.
    watcher: std::sync::Mutex<notify::RecommendedWatcher>,
    stopped: AtomicBool,
    /// Subtrees nothing is watching, because they could not be read.
    /// Kept rather than counted, and behind a lock because a later `cover` adds
    /// to it: "live updates are off somewhere" is not actionable, a path is.
    skipped: std::sync::Mutex<Vec<String>>,
    follow_symlinks: bool,
    /// Shared with the event handler, so [`WatchHandle::retune`] can replace the filter in place.
    rules: SharedRules,
}

#[cfg(not(target_os = "linux"))]
impl std::fmt::Debug for FsWatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsWatch")
            .field("stopped", &self.stopped.load(Ordering::Relaxed))
            .field("skipped", &self.skipped)
            .finish()
    }
}

#[cfg(not(target_os = "linux"))]
impl WatchHandle for FsWatch {
    fn unwatched(&self) -> Vec<String> {
        self.skipped.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn cover(&self, path: &str) {
        if self.stopped.load(Ordering::Relaxed) {
            return;
        }
        let Ok(mut watcher) = self.watcher.lock() else {
            return;
        };
        // Watching a path twice is not a second watch: the backend keys by inode.
        let mut skipped = Vec::new();
        cover(
            &mut watcher,
            std::path::Path::new(path),
            &Discard,
            &mut skipped,
            0,
            self.follow_symlinks,
        );
        if !skipped.is_empty()
            && let Ok(mut held) = self.skipped.lock()
        {
            held.extend(skipped);
            held.sort_unstable();
            held.dedup();
            // After the dedup, because distinct paths make the dedup no bound.
            held.truncate(MAX_SKIPPED);
        }
    }

    /// Filter by the new rules from the next event on.
    /// The cover is left alone: rebuilding it means re-installing one watch per
    /// directory, and the filter is where the rule has to be right.
    fn retune(&self, opts: &ScanOptions) {
        let fresh = Arc::new(Rules::from_options(opts));
        match self.rules.write() {
            Ok(mut held) => *held = fresh,
            Err(poisoned) => *poisoned.into_inner() = fresh,
        }
    }

    fn stop(self: Box<Self>) {
        self.stopped.store(true, Ordering::Relaxed);
        drop(self.watcher);
    }
}

/// A sink for the one `cover` call that must not queue more work.
/// Extending the cover happens because a walk just ran; asking for another is a loop.
#[cfg(not(target_os = "linux"))]
#[derive(Debug)]
struct Discard;

#[cfg(not(target_os = "linux"))]
impl ChangeSink for Discard {
    fn emit(&self, _change: Change) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as PlMutex;

    /// Everything a translation emitted, in order.
    #[derive(Debug, Default)]
    struct Collect(PlMutex<Vec<Change>>);

    impl ChangeSink for Collect {
        fn emit(&self, change: Change) {
            self.0.lock().expect("the collector").push(change);
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn translated(event: Event) -> Vec<Change> {
        let collector = Arc::new(Collect::default());
        let sink: Arc<dyn ChangeSink> = Arc::clone(&collector) as Arc<dyn ChangeSink>;
        translate(
            scour_core::SourceId(0),
            true,
            &Rules::from_options(&ScanOptions::default()),
            &event,
            &sink,
        );
        collector.0.lock().expect("the collector").clone()
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn losing_track_asks_for_a_walk_even_when_it_cannot_say_where() {
        // A queue overflow arrives as `EventKind::Other` with the rescan flag and *no paths*.
        let overflow = Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan);
        assert_eq!(
            translated(overflow),
            vec![Change::Rescan {
                path: String::new()
            }],
            "an empty path is how the engine is told to walk everything"
        );

        let somewhere = Event::new(EventKind::Other)
            .set_flag(notify::event::Flag::Rescan)
            .add_path("/home/u/Projeler".into());
        assert_eq!(
            translated(somewhere),
            vec![Change::Rescan {
                path: "/home/u/Projeler".into()
            }]
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn the_uncovered_list_stops_growing_instead_of_growing_forever() {
        // The paths are distinct, so nothing but the truncation bounds the list.
        let held: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        for round in 0..40 {
            let batch: Vec<String> = (0..200)
                .map(|i| format!("/home/u/proje/paket-{round:03}/altdizin-{i:03}"))
                .collect();
            let mut guard = held.lock().expect("the list");
            guard.extend(batch);
            guard.sort_unstable();
            guard.dedup();
            guard.truncate(MAX_SKIPPED);
        }
        let n = held.lock().expect("the list").len();
        assert_eq!(
            n, MAX_SKIPPED,
            "8,000 distinct uncovered subtrees were kept as {n}, not {MAX_SKIPPED}"
        );
    }

    #[cfg(not(target_os = "linux"))]
    #[test]
    fn a_refused_subtree_is_still_named_when_there_are_few_of_them() {
        // 191 refused directories is the real report this list exists for.
        let mut skipped = Vec::new();
        for i in 0..191 {
            remember(&mut skipped, format!("/home/u/.local/share/waydroid/{i}"));
        }
        assert_eq!(skipped.len(), 191, "an ordinary report was truncated");

        for i in 0..MAX_SKIPPED * 2 {
            remember(&mut skipped, format!("/home/u/başka/{i}"));
        }
        assert_eq!(skipped.len(), MAX_SKIPPED);
    }

    #[test]
    fn a_path_that_cannot_be_read_is_not_a_path_that_is_gone() {
        // A directory with no execute bit deterministically gives `EACCES`.
        let tmp = tempfile::tempdir().expect("tmpdir");
        let dir = tmp.path().join("kapali");
        std::fs::create_dir(&dir).expect("mkdir");
        std::fs::write(dir.join("dosya.txt"), b"x").expect("write");
        let mut mode = std::fs::metadata(&dir).expect("stat").permissions();
        let was = std::os::unix::fs::PermissionsExt::mode(&mode);
        std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o600);
        std::fs::set_permissions(&dir, mode.clone()).expect("chmod");

        let inside = crate::path::from_path(&dir.join("dosya.txt"));
        let sink = Collect(PlMutex::new(Vec::new()));
        look(scour_core::SourceId(1), false, &inside, false, &sink);

        // Put the bit back before asserting, so a failure leaves nothing behind.
        std::os::unix::fs::PermissionsExt::set_mode(&mut mode, was);
        std::fs::set_permissions(&dir, mode).expect("chmod back");

        assert_eq!(
            sink.0.into_inner().unwrap(),
            vec![Change::Rescan {
                path: inside.clone()
            }],
            "an unreadable path was reported as deleted"
        );

        let missing = crate::path::from_path(&tmp.path().join("yok.txt"));
        let sink = Collect(PlMutex::new(Vec::new()));
        look(scour_core::SourceId(1), false, &missing, false, &sink);
        assert_eq!(
            sink.0.into_inner().unwrap(),
            vec![Change::RemoveSubtree { path: missing }]
        );
    }
}
