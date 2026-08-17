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

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use notify::{Event, EventKind, RecursiveMode, Watcher};
use scour_core::{Change, ChangeSink, Error, Result, ScanOptions, WatchHandle};

use crate::path;
use crate::rules::Rules;
use crate::scan::FsSource;

/// The rule set the watcher filters by, swappable while it runs.
///
/// Two layers, and both are load-bearing: the outer lock is what lets
/// [`WatchHandle::retune`] replace the set, and the inner `Arc` is what lets an
/// event take a copy and let go of the lock immediately rather than filtering
/// with it held.
type SharedRules = Arc<RwLock<Arc<Rules>>>;

pub fn start(
    source: FsSource,
    opts: &ScanOptions,
    sink: Box<dyn ChangeSink>,
) -> Result<Box<dyn WatchHandle>> {
    let sink: Arc<dyn ChangeSink> = Arc::from(sink);

    // **One mark a filesystem, when something has left us one.** This is not a
    // faster inotify, it is the only way two of the disks here get watched at
    // all: inotify wants 296,711 watches for the home directory against a limit
    // of 268,593, and 152,529 for the NTFS volume, which is why that volume was
    // not being watched. The marks need `CAP_SYS_ADMIN`, this process must not
    // have it, and the two are reconciled outside: a helper sets them and hands
    // the descriptor over. No helper, no descriptor, and inotify below is the
    // answer — which is the ordinary case and not a failure.
    #[cfg(target_os = "linux")]
    if let Some(started) = crate::fanotify::try_start(&source, opts, Arc::clone(&sink)) {
        return started;
    }

    let roots: Vec<_> = source.roots().to_vec();
    let id = source.source_id();
    let real_modes = source.real_modes();
    // The same rules the walk uses. Without them the watcher reports changes
    // for files the walk skips, and every one is an entry that exists until
    // something else removes it — a `cargo test` under a watched but unscanned
    // build directory took a query here from 8 ms to 13 seconds.
    //
    // **Behind a lock, because the rules can change while this is running.**
    // They used to be settable in a file read once at start-up; a window can
    // add one now, and a watcher still filtering by the old set would put back
    // everything a new rule had just swept out. See [`WatchHandle::retune`].
    let rules: SharedRules = Arc::new(RwLock::new(Arc::new(Rules::from_options(opts))));

    let handler = {
        let sink = Arc::clone(&sink);
        let rules = Arc::clone(&rules);
        move |res: notify::Result<Event>| match res {
            Ok(event) => {
                // Cloned out of the lock rather than held across the
                // translation: the read is a pointer copy, and holding it
                // would put every event in line behind a `retune` that is
                // rebuilding the set.
                let held = match rules.read() {
                    Ok(r) => Arc::clone(&r),
                    Err(p) => Arc::clone(&p.into_inner()),
                };
                translate(id, real_modes, &held, &event, &sink)
            }
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

    // **The watcher has to follow the same links the walk does, and by default
    // it does not.** `notify`'s `follow_symlinks` is on, so a recursive watch
    // descends through every symlink it meets while `WalkBuilder::follow_links`
    // here is off — and the two disagreeing is not a matter of taste.
    //
    // What it costs is not watches, it is **rows**. On this machine
    // `~/.wine-hukuk/dosdevices/z:` points at `/`, and a file created in the
    // home directory arrived as `/home/u/.wine-hukuk/dosdevices/z:/home/u/…`:
    // a path that does not exist, that no walk will ever produce, and that
    // *is* textually under the root — so every sweep killed those rows and the
    // watcher put them straight back. Reproduced by creating a directory and
    // finding its contents in the index under the alias and nowhere else.
    //
    // The watch *count* is not evidence of this, and was briefly taken for it:
    // 242,643 watches sounds like the whole filesystem and is what a home
    // directory of 242,242 directories costs on its own.
    let mut watcher = notify::RecommendedWatcher::new(
        handler,
        notify::Config::default().with_follow_symlinks(opts.follow_symlinks),
    )
    .map_err(|e| Error::Io {
        detail: format!("cannot start a watcher: {e}"),
    })?;

    let mut watched = 0usize;
    let mut skipped = Vec::new();
    for r in &roots {
        if cover(
            &mut watcher,
            r,
            sink.as_ref(),
            &mut skipped,
            0,
            opts.follow_symlinks,
        ) {
            watched += 1;
        }
    }
    if watched == 0 && !roots.is_empty() {
        return Err(Error::Io {
            detail: "no root could be watched".into(),
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
///
/// Each level costs a directory listing and a re-walk of the branches that
/// still work, so this bounds the repair rather than the tree. Six is deeper
/// than any real accident — `~/.local/share/waydroid/data/vendor`, the one that
/// prompted all of this, is five.
const SPLIT_DEPTH: u32 = 6;

/// How many uncovered subtrees are named before the list starts counting.
///
/// **This list has no natural end.** [`WatchHandle::cover`] is called for every
/// directory the engine discovers, and every call appends whatever could not be
/// watched — so on a machine that has run out of inotify watches, *each new
/// directory adds an entry that no later call removes*. The entries are
/// distinct paths, so the `dedup` below does nothing for them. Measured on this
/// machine: two sources want 511,116 watches against a per-user ceiling of
/// 524,288, which is 97.5% of the whole allowance — the failure mode is one
/// `git clone` away, and the list would then grow one path per directory
/// created for as long as the service runs.
///
/// What it is for survives the cap. The list is a diagnostic, read by
/// `scourd`'s start-up line, which reduces it to the one shared prefix a person
/// can act on — 191 Waydroid directories became `~/.local/share/waydroid/data`.
/// A thousand examples answer that question exactly as well as a million, and
/// cost about 100 KB instead of being unbounded.
const MAX_SKIPPED: usize = 1_024;

/// Remember an uncovered subtree, up to [`MAX_SKIPPED`] of them.
fn remember(skipped: &mut Vec<String>, path: String) {
    if skipped.len() < MAX_SKIPPED {
        skipped.push(path);
    }
}

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
    follow_symlinks: bool,
) -> bool {
    // A link is covered by whoever owns its target, exactly as in the walk.
    // Descending here would index the same files a second time under a path
    // nothing else in the system produces.
    if !follow_symlinks
        && depth > 0
        && std::fs::symlink_metadata(dir).is_ok_and(|m| m.file_type().is_symlink())
    {
        return false;
    }
    if watcher.watch(dir, RecursiveMode::Recursive).is_ok() {
        return true;
    }
    // Out of patience. Readable, so a walk can still cover what a watch will
    // not — say so and let the engine schedule it.
    if depth >= SPLIT_DEPTH {
        remember(skipped, path::from_path(dir));
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
        remember(skipped, path::from_path(dir));
        return false;
    };
    // The directory itself, without its contents, so that a file created
    // directly in it is still seen.
    let mut any = watcher.watch(dir, RecursiveMode::NonRecursive).is_ok();
    for child in children.flatten() {
        // Only directories: a file is covered by the watch on its parent, and
        // a symlink is followed by whoever owns the target.
        if child.file_type().is_ok_and(|t| t.is_dir())
            && cover(
                watcher,
                &child.path(),
                sink,
                skipped,
                depth + 1,
                follow_symlinks,
            )
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
/// Look at one path and say what changed there.
///
/// **This is where the contract at the top of the module is kept**, and it is
/// shared rather than duplicated because the two backends arrive at it from
/// opposite directions: inotify hands over a path and fanotify hands over a
/// directory handle and a name. What neither of them hands over is what
/// happened — a fanotify event merges a create, a write, a close and a delete
/// into one mask with no order, measured — so both end here, at a `stat`.
///
/// `fresh` means the path was not there a moment ago: a create, or the
/// destination of a rename. It is the only case that needs the walk below, and
/// separating it is what keeps a compile from queueing one for every directory
/// whose mtime moved.
/// Returns what the `stat` saw, so a caller that wants to remember this path
/// does not pay for a second one. `None` means the path is gone or unreadable.
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
            // **A directory that has just appeared is a subtree, not a row.**
            // Three different things are lost by treating it as one, and all
            // three were reproduced:
            //
            // * Between `mkdir a/b` and the moment an inotify backend has added
            //   a watch for `a/b`, anything created inside it produces no event
            //   at all. `mkdir d && echo > d/f` left `f` on disk and out of the
            //   index permanently; the same two commands eight seconds apart
            //   worked. That is `git clone`, `cargo new`, `unzip` and every
            //   installer.
            // * Where the recursive watch was refused and rebuilt shallow (see
            //   `cover`), a directory created in the shallow parent never gets
            //   a watch at all, so *nothing* inside it is ever seen.
            // * A btrfs snapshot makes a whole tree visible behind **one**
            //   event: measured at 1 event for 201 files, and a subvolume
            //   deletion at 1 for 202. No per-file event exists to be missed,
            //   on any mechanism, because the kernel never made one.
            //
            // A walk covers all three, because it reads what is there instead
            // of waiting to be told. `symlink_metadata` rather than `metadata`,
            // so a link to a directory is not descended.
            if fresh && md.is_dir() {
                sink.emit(Change::Rescan {
                    path: path.to_owned(),
                });
            }
            return Some(md);
        }
        // Gone between the event and the look. That is a removal, and it is the
        // common case under any kind of churn — it is also the cheap one:
        // a failing `statx` costs 0.6–2.3 µs against 98 µs for one that finds
        // something on a cold NTFS volume.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => sink.emit(Change::RemoveSubtree {
            path: path.to_owned(),
        }),
        // **Everything else is "I could not look", and that is not a
        // deletion.** Out of file descriptors, permission withdrawn, a network
        // mount gone stale, a transient read error: treating any of them as a
        // removal hides a tree that is still there, and the next commit makes
        // it durable. Ask for the path to be walked again instead — it is the
        // same message the backend sends when it loses track, and the engine
        // already knows what to do with it.
        Err(_) => sink.emit(Change::Rescan {
            path: path.to_owned(),
        }),
    }
    None
}

fn translate(
    id: scour_core::SourceId,
    real_modes: bool,
    rules: &Rules,
    event: &Event,
    sink: &Arc<dyn ChangeSink>,
) {
    // What the walk would not have looked at, this does not report. Checked
    // once here rather than in each arm, because every arm has the same answer
    // and a path that slips through is an index entry nobody asked for.
    //
    // From the **path alone**, with no `stat`. The first version asked
    // `is_dir()`, which is a syscall for every file a compiler writes — 74% of
    // a core while a build ran, spent deciding to discard the event.
    let watched = |p: &std::path::Path| -> bool { !rules.excludes_path(&path::from_path(p)) };
    // `fresh` means the path was not there a moment ago — a create, or the
    // destination of a rename. It is the only case that needs the extra
    // sentence below, and separating it is what keeps a compile from queueing a
    // walk for every directory whose mtime moved.
    let upsert = |p: &std::path::Path, fresh: bool| {
        let text = path::from_path(p);
        // Looked at now, and looked at again later: a write through a shared
        // mapping produces no event on this backend either. See
        // [`crate::revisit`].
        let md = look(id, real_modes, &text, fresh, sink.as_ref());
        let _ = &md;
        crate::revisit::note(&text, id, real_modes, sink, md.as_ref());
    };

    // **The backend has lost track.** inotify's queue overflowed, a watch was
    // dropped, a poll missed a window — every backend has its own way of
    // saying it and `notify` normalises all of them onto this flag. It arrives
    // as `EventKind::Other` **with no paths at all**, so the arm below that
    // loops over `event.paths` did exactly nothing with the one message whose
    // whole purpose is to say the index is drifting.
    //
    // An empty path is how the engine is told "and I cannot say where", which
    // it answers with a walk of everything. Expensive, and cheap next to an
    // index nobody knows is wrong.
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
        // A rename arrives as one event with two paths, or as two events. Both
        // are handled by looking at every path mentioned: the one that no
        // longer exists is removed, the one that does is upserted.
        //
        // A rename is also how a whole populated directory appears at once —
        // `mv ~/Downloads/project ~/src` produces no events for anything inside
        // it — so its destination counts as fresh for the same reason a create
        // does. Every other kind of modify does not: a directory's mtime moves
        // whenever a file in it is written, and treating that as fresh would
        // queue a walk per file during a build.
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
    /// Behind a lock because the cover can be extended after the fact — see
    /// [`WatchHandle::cover`]. Uncontended in practice: the engine's one worker
    /// thread is the only caller and it takes it after a walk, not during one.
    watcher: std::sync::Mutex<notify::RecommendedWatcher>,
    stopped: AtomicBool,
    /// Subtrees nothing is watching, because they could not be read.
    ///
    /// Kept rather than counted: "live updates are off somewhere" is not
    /// something a user can act on, and `~/.local/share/waydroid/data` is.
    ///
    /// **Behind a lock because it grows after the fact.** A subtree that fails
    /// to be covered later — a watch limit reached while a `git clone` is
    /// running, a directory whose permissions changed — is exactly as
    /// uncovered as one that failed at the start, and used to be pushed into a
    /// local vector that was dropped on the next line. The one message saying
    /// "nothing below here will be seen again" was thrown away.
    skipped: std::sync::Mutex<Vec<String>>,
    follow_symlinks: bool,
    /// Shared with the event handler, so [`WatchHandle::retune`] can replace
    /// what it filters by without taking the watch down and putting it back —
    /// which on this backend means re-installing one inotify watch per
    /// directory, 296,711 of them for a home directory here.
    rules: SharedRules,
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
        self.skipped.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn cover(&self, path: &str) {
        if self.stopped.load(Ordering::Relaxed) {
            return;
        }
        let Ok(mut watcher) = self.watcher.lock() else {
            return;
        };
        // Asking for a watch that is already there is not an error and not a
        // second watch — inotify keys them by inode, and `notify` replaces the
        // entry. So this does not have to know whether the recursive watch
        // above already covers the path, which it cannot know cheaply.
        //
        // A failure here is a hole in the live cover: everything below the
        // path will change without anyone hearing about it until the next full
        // scan. It joins the list the source reports rather than being
        // discarded, which is what used to happen.
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
            // **After the dedup, because the dedup is not a bound.** These are
            // distinct paths — one per directory that could not be watched — so
            // nothing here collapses them and the list only ever grew. See
            // [`MAX_SKIPPED`].
            held.truncate(MAX_SKIPPED);
        }
    }

    /// Filter by the new rules from the next event on.
    ///
    /// The cover is deliberately left alone. A watch that is now inside an
    /// excluded directory costs one inotify entry and reports events this
    /// throws away; taking it out would mean walking the whole cover, and
    /// putting it back when the rule goes would mean installing 296,711
    /// watches again. The filter is where the rule has to be right, and it is.
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
///
/// Extending the cover happens *because* a walk just ran; asking for another
/// one from inside it is how a walk becomes a loop.
#[derive(Debug)]
struct Discard;

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

    #[test]
    fn losing_track_asks_for_a_walk_even_when_it_cannot_say_where() {
        // **The message that used to be dropped.** inotify's queue overflowing
        // arrives as `EventKind::Other` carrying the rescan flag and *no
        // paths*; the arm that handled that kind looped over the paths, so the
        // one event whose entire purpose is to say "the index is drifting"
        // produced nothing at all. A `git clone` or an `rm -rf` large enough to
        // overflow the queue left permanent holes and permanent ghosts.
        //
        // Built the way the backend builds it — see `notify`'s inotify
        // backend, which sends exactly this before any path is attached.
        let overflow = Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan);
        assert_eq!(
            translated(overflow),
            vec![Change::Rescan {
                path: String::new()
            }],
            "an empty path is how the engine is told to walk everything"
        );

        // And when the backend can name the subtree it lost, that is what is
        // walked rather than the whole filesystem.
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

    #[test]
    fn the_uncovered_list_stops_growing_instead_of_growing_forever() {
        // **The list had no end.** `cover` is called once per directory the
        // engine discovers, and each call appends whatever could not be
        // watched. The entries are distinct paths, so the `dedup` beside them
        // collapses nothing; on a machine that has exhausted its inotify
        // watches — 511,116 wanted against 524,288 allowed here, 97.5% — every
        // new directory would add one and none would ever be removed.
        //
        // Driven through the retained list the way `WatchHandle::cover` fills
        // it, rather than through a real watcher, because the failure is about
        // what is kept and not about what the kernel said.
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

    #[test]
    fn a_refused_subtree_is_still_named_when_there_are_few_of_them() {
        // The cap must not cost the ordinary case anything: 191 Waydroid
        // directories is the real report this list exists for, and it is far
        // below the ceiling.
        let mut skipped = Vec::new();
        for i in 0..191 {
            remember(&mut skipped, format!("/home/u/.local/share/waydroid/{i}"));
        }
        assert_eq!(skipped.len(), 191, "an ordinary report was truncated");

        // And past the ceiling it stops rather than refusing or panicking.
        for i in 0..MAX_SKIPPED * 2 {
            remember(&mut skipped, format!("/home/u/başka/{i}"));
        }
        assert_eq!(skipped.len(), MAX_SKIPPED);
    }

    #[test]
    fn a_path_that_cannot_be_read_is_not_a_path_that_is_gone() {
        // A stat that fails with anything other than "it is not there" means
        // the watcher could not look: out of descriptors, permission
        // withdrawn, a stale network mount. Removing on that evidence hides a
        // tree that still exists, and the next commit makes it durable.
        //
        // A directory with no execute bit is the deterministic way to get
        // `EACCES` from `symlink_metadata` on a path inside it.
        let tmp = tempfile::tempdir().expect("tmpdir");
        let dir = tmp.path().join("kapali");
        std::fs::create_dir(&dir).expect("mkdir");
        std::fs::write(dir.join("dosya.txt"), b"x").expect("write");
        let mut mode = std::fs::metadata(&dir).expect("stat").permissions();
        let was = std::os::unix::fs::PermissionsExt::mode(&mode);
        std::os::unix::fs::PermissionsExt::set_mode(&mut mode, 0o600);
        std::fs::set_permissions(&dir, mode.clone()).expect("chmod");

        let inside = dir.join("dosya.txt");
        let got = translated(
            Event::new(EventKind::Modify(notify::event::ModifyKind::Any)).add_path(inside.clone()),
        );

        std::os::unix::fs::PermissionsExt::set_mode(&mut mode, was);
        std::fs::set_permissions(&dir, mode).expect("chmod back");

        let path = crate::path::from_path(&inside);
        assert_eq!(
            got,
            vec![Change::Rescan { path: path.clone() }],
            "an unreadable path was reported as deleted"
        );

        // Gone is still gone.
        let missing = tmp.path().join("yok.txt");
        assert_eq!(
            translated(
                Event::new(EventKind::Remove(notify::event::RemoveKind::Any))
                    .add_path(missing.clone())
            ),
            vec![Change::RemoveSubtree {
                path: crate::path::from_path(&missing)
            }]
        );
    }
}
