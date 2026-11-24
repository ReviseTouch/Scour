//! Mounts: which are under a watched root, and marking one that came later.

use super::*;
use scour_core::{Change, ChangeSink, SourceId};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Every filesystem with a block device behind it, as `mountinfo` sees it.
/// Keyed by mount id, not device: the mark survives the unmount of the path it
/// was placed through, but a remount of the filesystem does not bring it back.
pub(super) fn mounts(text: &str) -> HashSet<(u32, String)> {
    let mut out = HashSet::new();
    for line in text.lines() {
        let Some((left, right)) = line.split_once(" - ") else {
            continue;
        };
        let mut r = right.split_whitespace();
        let (Some(_fs), Some(source)) = (r.next(), r.next()) else {
            continue;
        };
        if !source.starts_with("/dev/") {
            continue;
        }
        let mut l = left.split_whitespace();
        let (Some(id), Some(at)) = (l.next(), l.nth(3)) else {
            continue;
        };
        if let Ok(id) = id.parse::<u32>() {
            out.insert((id, at.replace("\\040", " ")));
        }
    }
    out
}

/// Open `/proc/self/mountinfo` for watching rather than for reading.
/// `poll` returns `POLLERR | POLLPRI` when the mount table changes, 300 ms from
/// the mount. A new disk is a new superblock, and its events simply never come.
pub(super) fn mountinfo() -> Option<std::fs::File> {
    std::fs::File::open("/proc/self/mountinfo").ok()
}

/// One path a window has to look at, carried out of the lock with what looking needs.
/// `settled` is whether the kernel said the content is final; `None` for a
/// directory looked at only for its times.
pub(super) struct Look {
    pub(super) sink: Arc<dyn ChangeSink>,
    pub(super) id: SourceId,
    pub(super) real_modes: bool,
    pub(super) path: String,
    pub(super) fresh: bool,
    pub(super) settled: Option<bool>,
}

/// A filesystem that appeared after the marks were set.
/// Its contents can still be indexed, but nothing here can watch it: a new
/// filesystem is a new superblock and a mark needs a privilege this process lacks.
/// A mount that went away is covered again — the directory under it is on the
/// marked filesystem — and is looked at again, since its contents changed back.
///
/// Only mounts under a source's roots are that source's business. Every new
/// mount on the machine used to count against every source: systemd mounting
/// `/efi` when something read it left all three unwatched until a restart, and
/// each was walked whole whenever its write counter moved — every minute.
pub(super) fn note_mounts(fresh: &[String], gone: &[String]) {
    // Sent after the lock, as the reader's window is: see there.
    let mut rescans: Vec<(Arc<dyn ChangeSink>, String)> = Vec::new();
    {
        let Ok(subs) = SUBS.lock() else { return };
        for s in subs.iter().filter(|s| s.live.load(Ordering::Relaxed)) {
            let fresh: Vec<String> = fresh
                .iter()
                .filter(|at| !s.rules.excludes_path(at))
                .cloned()
                .collect();
            let look = match s.uncovered.lock() {
                Ok(mut u) => cover_mounts(&mut u, &s.roots, &fresh, gone),
                Err(_) => continue,
            };
            rescans.extend(look.into_iter().map(|at| (Arc::clone(&s.sink), at)));
        }
    }
    for (sink, at) in rescans {
        sink.emit(Change::Rescan { path: at });
    }
}

/// What one source makes of mounts that came and went: the new ones under its
/// roots join what it cannot watch, the vanished ones leave, and both are to be
/// walked again. Returns the paths to walk.
pub(super) fn cover_mounts(
    uncovered: &mut Vec<String>,
    roots: &[std::path::PathBuf],
    fresh: &[String],
    gone: &[String],
) -> Vec<String> {
    let under = |at: &str| {
        roots
            .iter()
            .any(|r| std::path::Path::new(at).starts_with(r))
    };
    let mut look = Vec::new();
    for at in fresh.iter().filter(|at| under(at)) {
        if !uncovered.contains(at) {
            uncovered.push(at.clone());
        }
        look.push(at.clone());
    }
    for at in gone {
        let was = uncovered.len();
        uncovered.retain(|u| u != at);
        if uncovered.len() != was || under(at) {
            look.push(at.clone());
        }
    }
    look
}
