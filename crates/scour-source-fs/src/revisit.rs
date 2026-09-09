//! Looking again at what stopped announcing itself.
//!
//! **A write through a shared mapping produces no event**: it is a page fault, not
//! a syscall, so nothing reaches `fsnotify`. Paths an event arrived for are re-read
//! on a widening clock — 0.55 µs a `statx`, against 1.3 s a pass over `/proc/*/maps`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use scour_core::{ChangeSink, SourceId};

/// How long to wait before each look, and there are only ever four.
/// The first is short because the file is probably still being written; the last
/// is a floor, so a mapping held for a week costs six `statx` calls an hour.
const TIERS: [Duration; 4] = [
    Duration::from_secs(5),
    Duration::from_secs(30),
    Duration::from_secs(120),
    Duration::from_secs(600),
];

/// How many paths may be remembered at once — about 400 KB.
/// The oldest goes when a new one arrives: least recently touched is least likely
/// to still be open.
const CAP: usize = 4_096;

/// What a path looked like when it was last examined, and when to look next.
struct Seen {
    id: SourceId,
    real_modes: bool,
    sink: Arc<dyn ChangeSink>,
    next: Instant,
    tier: usize,
    /// The two fields a mapped write can move, compared rather than trusted.
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
/// One reader, because the cheap way to read those two is not portable.
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
        // One thread, whatever the watcher is and however many sources there are.
        // It sleeps on a condition variable, so an idle list costs no wake-ups.
        let _ = std::thread::Builder::new()
            .name("scour-revisit".into())
            .spawn(move || run(&theirs));
        it
    })
}

/// Remember a path, or move one already remembered back to the first tier.
/// Takes the metadata the look already read: a hash lookup and no syscall.
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
/// `FAN_CLOSE_WRITE` arrives at `munmap` for a mapping, because `MAP_SHARED` holds
/// a reference to the open file and `__fput` runs when the last one goes.
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
    // `shared()` starts the thread, so only touch it once something is on the list.
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
                // Sleep until the earliest deadline. An empty list waits for ever.
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
            // Advance the tier under the lock, so a slow look cannot start twice.
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
            // The lock is not held across the `stat` or the emit.
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
                // Gone. `look` turns that into a removal, as a watcher would.
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
        // Four looks and then a floor: a week-old mapping is still examined.
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
        // A directory has no body a mapping could change behind a watcher's back.
        let dir = tempfile::tempdir().expect("tempdir");
        let md = std::fs::symlink_metadata(dir.path()).expect("stat");
        assert!(md.is_dir());
        // `note` returns early, and the observable effect is that nothing started.
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
    /// The mapped write is simulated: what matters is that content moved silently.
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

        // Changed behind the watcher's back, the way a mapped write would move it.
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
    /// moved. A commit where there was none costs 87 ms on disk.
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
