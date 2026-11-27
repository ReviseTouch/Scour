//! The watch itself: the descriptor the helper handed down, the sources
//! subscribed to it, and the one thread that reads it.

use super::*;
use crate::path;
use crate::rules::Rules;
use crate::scan::FsSource;
use scour_core::{ChangeSink, Result, ScanOptions, SourceId, WatchHandle};
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Where the privileged helper leaves the already-marked descriptor.
/// The marks need `CAP_SYS_ADMIN`: a process handed the descriptor reads every
/// event while `fanotify_init` and `fanotify_mark` still return EPERM to it.
const FD_ENV: &str = "SCOUR_FANOTIFY_FD";

// Not in `libc` at the time of writing, and their values are kernel ABI.
const FAN_REPORT_DIR_FID: u32 = 0x0000_0400;

const FAN_REPORT_NAME: u32 = 0x0000_0800;

/// What the helper has to have opened the group with, or nothing below works.
/// Both bits: `DIR_FID` puts the parent's handle in the event and `NAME` the
/// entry name. Without them events still parse, so paths would be wrong, not absent.
const REQUIRED_FLAGS: u32 = FAN_REPORT_DIR_FID | FAN_REPORT_NAME;

/// Take the descriptor the helper left, if it left one.
/// Absent is the ordinary case and not an error: [`try_start`] returns `None`.
fn inherited() -> Option<OwnedFd> {
    let raw = handed()?;
    // SAFETY: the descriptor is open — `fdinfo` for it was just read — and the
    // environment variable is the helper's contract for handing over ownership,
    // so nothing else in this process holds it.
    Some(unsafe { OwnedFd::from_raw_fd(raw) })
}

/// Close the helper's descriptor on every exec from here on. It crossed one
/// exec to get here and must cross no other: a thumbnailer started on a
/// downloaded file would otherwise read every name created on the disk.
/// Called first thing, since a service that watches nothing never takes it.
pub fn keep_from_children() {
    if let Some(raw) = handed() {
        // SAFETY: `handed` checked that the descriptor is open; only its flags change.
        unsafe {
            let f = libc::fcntl(raw, libc::F_GETFD);
            if f >= 0 {
                libc::fcntl(raw, libc::F_SETFD, f | libc::FD_CLOEXEC);
            }
        }
    }
}

/// The descriptor the variable names, once it is known to be a group opened
/// the way the reader needs.
fn handed() -> Option<RawFd> {
    let raw: RawFd = std::env::var(FD_ENV).ok()?.trim().parse().ok()?;
    if raw < 0 {
        return None;
    }
    // **Check what it is before reading it.** A stale variable naming another open
    // file would be read as events, and a group without the report flags would parse
    // into wrong paths. `fdinfo`'s first line answers both.
    let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{raw}")).ok()?;
    let flags = info.lines().find_map(|l| {
        let rest = l.strip_prefix("fanotify flags:")?;
        let hex = rest.split_whitespace().next()?;
        u32::from_str_radix(hex.trim_start_matches("0x"), 16).ok()
    })?;
    if flags & REQUIRED_FLAGS != REQUIRED_FLAGS {
        return None;
    }
    Some(raw)
}

/// One source's share of the one reader: a single group that cannot be split,
/// since two readers of one queue each silently drop the other's half. Routing is
/// recognition — the event's parent handle is offered to each subscriber in turn.
pub(super) struct Sub {
    pub(super) id: SourceId,
    pub(super) real_modes: bool,
    pub(super) rules: Arc<Rules>,
    pub(super) sink: Arc<dyn ChangeSink>,
    pub(super) roots: Vec<std::path::PathBuf>,
    pub(super) map: DirMap,
    /// What [`walk_threads`] answered when the map was built, kept so that rebuilding
    /// it in [`WatchHandle::retune`] need not ask an `FsSource` out of reach.
    pub(super) threads: usize,
    /// Cleared when the handle is dropped, and the entry taken out with it. Also
    /// what a handle finds its own entry by: a source keeps its id across an
    /// engine swap, so after one there were two entries under it, and `cover`
    /// and `retune` found the first — the dead one.
    pub(super) live: Arc<AtomicBool>,
    pub(super) uncovered: Arc<Mutex<Vec<String>>>,
}

/// The subscribers, and whether the reader has been started.
pub(super) static SUBS: Mutex<Vec<Sub>> = Mutex::new(Vec::new());

static READER: std::sync::OnceLock<()> = std::sync::OnceLock::new();

impl std::fmt::Debug for Sub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sub")
            .field("id", &self.id)
            .field("roots", &self.roots)
            .field("dirs", &self.map.by_key.len())
            .finish()
    }
}

#[derive(Debug)]
pub(super) struct FanWatch {
    /// Which source this is, for the record; the entry is found by `live`.
    #[allow(dead_code)]
    pub(super) id: SourceId,
    /// Shared with this handle's entry in [`SUBS`], and how it is found there.
    pub(super) live: Arc<AtomicBool>,
    /// Filesystems that appeared after the marks were set: not covered, and not
    /// coverable from here, so they are named for the layer above to report.
    pub(super) uncovered: Arc<Mutex<Vec<String>>>,
}

impl WatchHandle for FanWatch {
    fn unwatched(&self) -> Vec<String> {
        self.uncovered.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// The mark already covers a directory created a moment ago; the map does not
    /// know it. A tree made in a burst — `mkdir -p`, an archive opened, a clone —
    /// reached the map as its top directory alone: the events naming the ones inside
    /// were read before the top was learned, and dropped. Everything that happened
    /// in those directories afterwards was dropped as well, until a walk of the
    /// whole source: 73,759 rows in a day here, found by the daily check. So every
    /// directory under a subtree about to be walked is learned first, by a walk of
    /// its own under no lock.
    fn cover(&self, path: &str) {
        let Some(rules) = SUBS.lock().ok().and_then(|subs| {
            subs.iter()
                .find(|s| self.owns(s))
                .map(|s| Arc::clone(&s.rules))
        }) else {
            return;
        };
        let found = directories_under(path, &rules);
        if found.is_empty() {
            return;
        }
        if let Ok(mut subs) = SUBS.lock()
            && let Some(s) = subs.iter_mut().find(|s| self.owns(s))
        {
            for (key, parent, at) in &found {
                s.map.insert_key(*key, *parent, at);
            }
        }
    }

    /// Take the new rules, and rebuild the directory map behind them: the map is how
    /// an event gets a name, and a rule switched off re-opens a subtree it never heard
    /// of. Built before the lock — walking both roots is 0.7 s warm, 0.38 s cold on NTFS.
    fn retune(&self, opts: &ScanOptions) {
        let rules = Arc::new(Rules::from_options(opts));
        let Some((roots, threads)) = SUBS.lock().ok().and_then(|subs| {
            subs.iter()
                .find(|s| self.owns(s))
                .map(|s| (s.roots.clone(), s.threads))
        }) else {
            return;
        };
        let map = DirMap::build(&roots, &rules, threads);
        if let Ok(mut subs) = SUBS.lock()
            && let Some(s) = subs.iter_mut().find(|s| self.owns(s))
        {
            s.rules = rules;
            s.map = map;
        }
    }

    /// Leave the reader running: it serves every source, so one stopping is no reason
    /// to take it down. The subscription goes, and the thread stays.
    fn stop(self: Box<Self>) {
        self.leave();
    }
}

impl FanWatch {
    fn owns(&self, sub: &Sub) -> bool {
        Arc::ptr_eq(&sub.live, &self.live)
    }

    /// Go quiet and take this entry out, map and all: a swapped-out engine's
    /// entry held a whole source's directory map, tens of megabytes, for good.
    /// The reader uses positions in [`SUBS`] only while it holds the lock.
    fn leave(&self) {
        self.live.store(false, Ordering::Relaxed);
        if let Ok(mut subs) = SUBS.lock() {
            subs.retain(|s| !self.owns(s));
        }
    }
}

impl Drop for FanWatch {
    fn drop(&mut self) {
        self.leave();
    }
}

/// How many threads to walk the tree for the directory map on.
/// The scan's answer, because the question is about the device: a spinning disk
/// turns every extra reader into a seek, whoever is asking.
pub(super) fn walk_threads(source: &FsSource, opts: &ScanOptions) -> usize {
    if opts.threads != 0 {
        return opts.threads;
    }
    source.medium().walk_threads(
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4),
    )
}

/// Subscribe to the one reader, or say this mechanism is not available here.
/// `None` means no descriptor was handed over, which is the ordinary case on a
/// machine where the helper was never installed.
pub fn try_start(
    source: &FsSource,
    opts: &ScanOptions,
    sink: Arc<dyn ChangeSink>,
) -> Option<Result<Box<dyn WatchHandle>>> {
    // Root by root: one the group cannot hear — a `tmpfs` beside a marked disk —
    // is walked instead, and the rest are still heard. All or nothing made a whole
    // source unwatched for one small root (`/tmp` in "sistem", 2026-11-27).
    let raw = handed()?;
    let (roots, deaf): (Vec<std::path::PathBuf>, Vec<std::path::PathBuf>) = source
        .roots()
        .iter()
        .cloned()
        .partition(|r| crate::marks::covered(raw, std::slice::from_ref(r)) && decodable(r));
    if roots.is_empty() {
        return None;
    }
    for r in &deaf {
        eprintln!(
            "scourd: {} cannot be watched by fanotify; its changes are found by walking it",
            path::from_path(r)
        );
    }
    // The descriptor is taken once: two owners is a double close, and two readers
    // of one group is half the events each.
    let first = READER.get().is_none();
    let fd = if first { Some(inherited()?) } else { None };

    let rules = Arc::new(Rules::from_options(opts));
    let threads = walk_threads(source, opts);
    let map = DirMap::build(&roots, &rules, threads);
    let live = Arc::new(AtomicBool::new(true));
    // What no event will report, and the engine walks on its pulse instead.
    let uncovered: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(
        deaf.iter().map(|r| path::from_path(r)).collect(),
    ));

    eprintln!(
        "scourd: watching with fanotify — {} director{} under {}",
        map.by_key.len(),
        if map.by_key.len() == 1 { "y" } else { "ies" },
        roots
            .first()
            .map(|r| path::from_path(r))
            .unwrap_or_default()
    );

    SUBS.lock().ok()?.push(Sub {
        id: source.source_id(),
        real_modes: source.real_modes(),
        rules,
        sink,
        roots,
        map,
        threads,
        live: Arc::clone(&live),
        uncovered: Arc::clone(&uncovered),
    });

    if let Some(fd) = fd {
        READER.get_or_init(|| {
            let _ = std::thread::Builder::new()
                .name("scour-fanotify".into())
                .spawn(move || drain(fd));
        });
    }

    Some(Ok(Box::new(FanWatch {
        id: source.source_id(),
        live,
        uncovered,
    })))
}
