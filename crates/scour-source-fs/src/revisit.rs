//! Looking again at what stopped announcing itself.
//!
//! **A file written through a shared mapping produces no event.** The write is
//! a page fault rather than a system call, so nothing reaches `fsnotify` and no
//! watcher of any kind reports it — this is not an fanotify gap, inotify is
//! just as blind. Measured: content changed from one byte to another while
//! `mtime`, `ctime`, `size` and `blocks` stayed identical through `msync`,
//! fifty-five seconds of writeback, `munmap` and `close`.
//!
//! What saves it is that such a file was announced *once*. A database is
//! created, its `-wal` and `-shm` beside it, and those creations are ordinary
//! events. So the path is known; it is only that nobody looks again.
//!
//! This looks again. Every path an event arrived for is remembered and
//! re-examined on a widening clock, and dropped when the kernel says the
//! content is final.
//!
//! **The cheap part is what it does not do.** Enumerating which files are
//! currently mapped means reading `/proc/*/maps` for every process, measured at
//! **1.3 seconds a pass** on this machine — the kernel walks each process's
//! whole VMA list and resolves every path. Spreading that over time does not
//! make it cost less, only later: five processes a second is 1.1% of a core,
//! which is more than everything else the service does put together. A `statx`
//! is 0.55 µs, so revisiting five hundred paths every thirty seconds is 275 µs
//! a minute, and no `/proc` at all.
//!
//! And nothing is emitted unless something moved. Re-announcing five hundred
//! files on a clock would put a commit where there was none, which costs 87 ms
//! on disk — worse than the staleness it set out to fix.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use scour_core::{ChangeSink, SourceId};

/// How long to wait before each look, and there are only ever four.
///
/// The first is short because the common case is a file still being written;
/// the last is the floor a long-lived mapping settles onto. A database mapped
/// for a week is looked at six times an hour for ever, which is six `statx`
/// calls — the reason the list does not need to expire is that staying on it
/// costs nothing.
const TIERS: [Duration; 4] = [
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(120),
    Duration::from_secs(600),
];

/// How many paths may be remembered at once.
///
/// A bound rather than a policy: the list holds what recently changed, which
/// on an idle machine is a handful and during a build is unbounded. Four
/// thousand entries is about 400 KB, and the oldest goes when a new one
/// arrives — losing the least recently touched is losing the one least likely
/// to still be open.
const CAP: usize = 4_096;

/// What a path looked like when it was last examined, and when to look next.
struct Seen {
    id: SourceId,
    real_modes: bool,
    sink: Arc<dyn ChangeSink>,
    next: Instant,
    tier: usize,
    /// The two fields a mapped write can move. Compared rather than trusted:
    /// **the point of the list is to emit nothing when nothing changed.**
    size: u64,
    mtime: i64,
    /// Roughly when this was last touched, for choosing what to forget.
    stamp: Instant,
}

#[derive(Default)]
struct Shared {
    paths: Mutex<HashMap<String, Seen>>,
    wake: Condvar,
    stop: AtomicBool,
}

/// What this module compares a file against: its length and when it changed.
///
/// **One reader rather than `MetadataExt` at four call sites.** The two facts
/// are portable — every filesystem has a length and a modification time — but
/// the cheap way to read them is not: on Unix they are already in the `statx`
/// this `Metadata` came from, and off it they arrive as a `SystemTime` that
/// has to be converted. Written once, so the four places that ask cannot
/// disagree and the crate compiles for the platforms the README claims.
fn shape(md: &std::fs::Metadata) -> (u64, i64) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (md.size(), md.mtime())
    }
    #[cfg(not(unix))]
    {
        let mtime = md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        (md.len(), mtime)
    }
}

fn shared() -> &'static Arc<Shared> {
    static IT: OnceLock<Arc<Shared>> = OnceLock::new();
    IT.get_or_init(|| {
        let it: Arc<Shared> = Arc::default();
        let theirs = Arc::clone(&it);
        // One thread, whatever the watcher is and however many sources there
        // are. It sleeps on a condition variable until the earliest deadline,
        // so an idle list costs no wake-ups at all.
        let _ = std::thread::Builder::new()
            .name("scour-revisit".into())
            .spawn(move || run(&theirs));
        it
    })
}

/// Remember a path, or move one already remembered back to the first tier.
///
/// Called for every event a backend resolves, with the metadata the look that
/// followed it already read — so this costs a hash lookup and no syscall.
pub(crate) fn note(
    path: &str,
    id: SourceId,
    real_modes: bool,
    sink: &Arc<dyn ChangeSink>,
    md: Option<&std::fs::Metadata>,
) {
    // A directory has no content to be written behind our back.
    if md.is_none_or(|m| m.is_dir()) {
        return;
    }
    let md = md.expect("checked");
    let now = Instant::now();
    let it = shared();
    let Ok(mut paths) = it.paths.lock() else {
        return;
    };
    if let Some(seen) = paths.get_mut(path) {
        seen.tier = 0;
        seen.next = now + TIERS[0];
        let (size, mtime) = shape(md);
        seen.size = size;
        seen.mtime = mtime;
        seen.stamp = now;
    } else {
        if paths.len() >= CAP
            && let Some(oldest) = paths
                .iter()
                .min_by_key(|(_, s)| s.stamp)
                .map(|(p, _)| p.clone())
        {
            paths.remove(&oldest);
        }
        paths.insert(
            path.to_owned(),
            Seen {
                id,
                real_modes,
                sink: Arc::clone(sink),
                next: now + TIERS[0],
                tier: 0,
                size: shape(md).0,
                mtime: shape(md).1,
                stamp: now,
            },
        );
    }
    it.wake.notify_one();
}

// Only the fanotify reader calls this, and that is Linux.
#[cfg(target_os = "linux")]
/// Stop looking at a path: the kernel has said its content is final.
///
/// `FAN_CLOSE_WRITE` is that statement, and for a mapping it arrives at
/// `munmap` rather than at `close` — measured, because `MAP_SHARED` holds a
/// reference to the open file and `__fput` runs when the last one goes. So the
/// event that says "the mapping is gone" is the event that takes the path off
/// this list, and no `/proc` was consulted to learn it.
pub(crate) fn forget(path: &str) {
    // Nothing has ever been noted: do not start the thread to say so.
    let Some(it) = started() else { return };
    if let Ok(mut paths) = it.paths.lock() {
        paths.remove(path);
    }
}

// Only the fanotify reader calls this, and that is Linux.
#[cfg(target_os = "linux")]
fn started() -> Option<&'static Arc<Shared>> {
    static PROBE: OnceLock<()> = OnceLock::new();
    let _ = &PROBE;
    // `shared()` starts the thread, so only touch it once something is on the
    // list — which `note` guarantees before any `forget` can matter.
    if LIVE.load(Ordering::Relaxed) {
        Some(shared())
    } else {
        None
    }
}

static LIVE: AtomicBool = AtomicBool::new(false);

fn run(it: &Arc<Shared>) {
    LIVE.store(true, Ordering::Relaxed);
    let mut due: Vec<String> = Vec::new();
    while !it.stop.load(Ordering::Relaxed) {
        let wait = {
            let Ok(mut paths) = it.paths.lock() else {
                return;
            };
            let now = Instant::now();
            due.clear();
            due.extend(
                paths
                    .iter()
                    .filter(|(_, s)| s.next <= now)
                    .map(|(p, _)| p.clone()),
            );
            if due.is_empty() {
                // Sleep until the earliest deadline, or until something is
                // noted. An empty list waits for ever and costs nothing.
                let next = paths.values().map(|s| s.next).min();
                let wait = next.map_or(Duration::from_secs(3_600), |at| {
                    at.saturating_duration_since(now)
                        .max(Duration::from_millis(1))
                });
                let Ok((_g, _t)) = it.wake.wait_timeout(paths, wait) else {
                    return;
                };
                continue;
            }
            // Advance the tier before releasing the lock, so a look that takes
            // a while cannot be started twice.
            for p in &due {
                if let Some(s) = paths.get_mut(p) {
                    s.tier = (s.tier + 1).min(TIERS.len() - 1);
                    s.next = now + TIERS[s.tier];
                }
            }
            Duration::ZERO
        };
        let _ = wait;

        for p in &due {
            // The lock is not held across the `stat` or the emit: a look is
            // microseconds and a sink is somebody else's channel.
            let md = std::fs::symlink_metadata(p);
            let Ok(mut paths) = it.paths.lock() else {
                return;
            };
            let Some(seen) = paths.get_mut(p) else {
                continue;
            };
            match md {
                Ok(md) => {
                    let (size, mtime) = shape(&md);
                    if size == seen.size && mtime == seen.mtime {
                        continue;
                    }
                    seen.size = size;
                    seen.mtime = mtime;
                    let (id, real_modes, sink) = (seen.id, seen.real_modes, Arc::clone(&seen.sink));
                    drop(paths);
                    crate::watch::look(id, real_modes, p, false, sink.as_ref());
                }
                // Gone. `look` is what turns that into a removal, and it is the
                // same answer a watcher would have given.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let (id, real_modes, sink) = (seen.id, seen.real_modes, Arc::clone(&seen.sink));
                    paths.remove(p);
                    drop(paths);
                    crate::watch::look(id, real_modes, p, false, sink.as_ref());
                }
                Err(_) => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_clock_widens_and_then_stops_widening() {
        // Four looks and then a floor: a mapping held for a week is examined
        // six times an hour for ever, not once and forgotten.
        let mut tier = 0usize;
        let mut seen = Vec::new();
        for _ in 0..8 {
            seen.push(TIERS[tier]);
            tier = (tier + 1).min(TIERS.len() - 1);
        }
        assert_eq!(
            seen,
            [
                Duration::from_secs(5),
                Duration::from_secs(30),
                Duration::from_secs(120),
                Duration::from_secs(600),
                Duration::from_secs(600),
                Duration::from_secs(600),
                Duration::from_secs(600),
                Duration::from_secs(600),
            ]
        );
    }

    #[test]
    fn a_directory_is_never_remembered() {
        // Its contents announce themselves; the directory itself has no body
        // that a mapping could change behind a watcher's back.
        let dir = tempfile::tempdir().expect("tempdir");
        let md = std::fs::symlink_metadata(dir.path()).expect("stat");
        assert!(md.is_dir());
        // `note` returns early; the observable effect is that nothing was
        // started, which `started()` reports.
        let before = LIVE.load(Ordering::Relaxed);
        note(
            &dir.path().to_string_lossy(),
            SourceId(0),
            true,
            &(Arc::new(Discard) as Arc<dyn ChangeSink>),
            Some(&md),
        );
        assert_eq!(LIVE.load(Ordering::Relaxed), before);
    }

    /// The whole point, end to end: a file changes with no event, and it is
    /// noticed anyway.
    ///
    /// A mapped write is simulated rather than performed — what matters is the
    /// shape, which is that the content moved and nothing told us. If this
    /// stops passing, a database being written through a mapping goes back to
    /// carrying whatever timestamp it had when it was created.
    #[test]
    fn a_change_nobody_announced_is_found_on_the_next_look() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("db.sqlite");
        std::fs::write(&file, b"one").expect("write");
        let path = file.to_string_lossy().into_owned();

        let heard: Arc<Heard> = Arc::default();
        let sink: Arc<dyn ChangeSink> = Arc::clone(&heard) as Arc<dyn ChangeSink>;
        let md = std::fs::symlink_metadata(&file).expect("stat");
        note(&path, SourceId(0), true, &sink, Some(&md));

        // Changed behind the watcher's back: no event, and the size moves the
        // way a mapped write would move it.
        std::fs::write(&file, b"one and then some more").expect("rewrite");

        let deadline = Instant::now() + TIERS[0] + Duration::from_secs(4);
        while Instant::now() < deadline && heard.count() == 0 {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            heard.count() > 0,
            "a file that changed without an event has to be found by looking again"
        );
    }

    /// And the other half: looking again must not *say* anything when nothing
    /// moved. Five hundred paths re-announced on a clock would put a commit
    /// where there was none, which costs 87 ms on disk.
    #[test]
    fn a_look_that_finds_nothing_changed_says_nothing() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join("quiet.bin");
        std::fs::write(&file, b"unchanging").expect("write");
        let path = file.to_string_lossy().into_owned();

        let heard: Arc<Heard> = Arc::default();
        let sink: Arc<dyn ChangeSink> = Arc::clone(&heard) as Arc<dyn ChangeSink>;
        let md = std::fs::symlink_metadata(&file).expect("stat");
        note(&path, SourceId(0), true, &sink, Some(&md));

        std::thread::sleep(TIERS[0] + Duration::from_secs(2));
        assert_eq!(
            heard.count(),
            0,
            "nothing moved, so nothing may be emitted — the list is not a clock that re-announces"
        );
    }

    #[derive(Debug, Default)]
    struct Heard(Mutex<usize>);
    impl Heard {
        fn count(&self) -> usize {
            *self.0.lock().expect("heard")
        }
    }
    impl ChangeSink for Heard {
        fn emit(&self, _: scour_core::Change) {
            *self.0.lock().expect("heard") += 1;
        }
    }

    #[derive(Debug)]
    struct Discard;
    impl ChangeSink for Discard {
        fn emit(&self, _: scour_core::Change) {}
    }
}
