//! Watching a whole filesystem with one mark.
//!
//! inotify costs a watch a directory. On the machine this was built for that is
//! 296,711 directories under the home alone against a limit of 268,593, and
//! 152,529 for the NTFS volume — which is why that volume was not being watched
//! at all. `FAN_MARK_FILESYSTEM` costs **one mark a superblock**, measured at
//! 0.005 ms and no measurable kernel memory, against 26.4 ms and 3.1 MB for
//! 30,000 inode marks that still miss every directory created afterwards.
//!
//! Three things about this mechanism decide the shape of everything below, and
//! each was measured rather than assumed:
//!
//! * **Events merge.** A file created, written, closed and deleted between two
//!   reads arrives as a single event with `mask = 0x30a` — CREATE, MODIFY,
//!   CLOSE_WRITE and DELETE together, in no order. Nothing in the event says
//!   whether the file is there now. So the mask is never read as a description;
//!   the path is looked at instead, which is the contract [`crate::watch`]
//!   already states.
//! * **The cost is the wake, not the event.** Per event the reader spends
//!   11.4 µs at 20 events a second and 1.1 µs at 230,000 — the difference is a
//!   fixed cost amortised over the batch. Draining on a timer instead of on
//!   arrival took a 2,120-event-a-second load from 0.84% of a core to 0.078%,
//!   and idle from 143 wakes a second to 5. Hence [`WINDOW`].
//! * **A file handle is not a path.** The event carries the *parent directory*
//!   as an opaque handle plus the entry name. Turning that into a path takes
//!   either `open_by_handle_at` — which needs `CAP_DAC_READ_SEARCH`, measured
//!   EPERM as an ordinary user — or a map this process builds itself. It builds
//!   it: the handle's bytes carry the inode number, and the walk already stats
//!   every directory.
//!
//! What this module does *not* do is decide anything. Every path it resolves
//! goes through the same [`crate::watch::look`] as an inotify event, so the
//! rules, the metadata read and the "a new directory is a subtree" step are
//! shared and cannot drift apart.

use std::collections::{HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use scour_core::{Change, ChangeSink, Result, ScanOptions, SourceId, WatchHandle};

use crate::path;
use crate::rules::Rules;
use crate::scan::FsSource;

/// Where the privileged helper leaves the already-marked descriptor.
///
/// The marks need `CAP_SYS_ADMIN` and this process does not have it and must
/// not: measured, an unprivileged process that is handed the descriptor can
/// read every event on both disks while `fanotify_mark` and `fanotify_init`
/// both return EPERM to it. So privilege is spent once, before this process
/// starts, and what crosses over is a descriptor that cannot be used to widen
/// the watch.
const FD_ENV: &str = "SCOUR_FANOTIFY_FD";

/// How long to let events pile up before draining them.
///
/// Two hundred milliseconds, and the number is the whole reason this is
/// affordable. Measured against a steady load, wakes a second and the share of
/// one core they cost:
///
/// | window | ~212 events/s | ~2120 events/s |
/// |---|---|---|
/// | 0 ms | 143 wakes, 0.274% | 1417 wakes, 0.838% |
/// | 50 ms | 17 wakes, 0.073% | 20 wakes, 0.157% |
/// | **200 ms** | **5 wakes, 0.030%** | **5 wakes, 0.078%** |
/// | 500 ms | 2 wakes, 0.019% | 2 wakes, 0.062% |
/// | 1000 ms | 1 wake, 0.014% | 1 wake, **0.075%** |
///
/// Past 500 ms it stops paying and at 1000 ms it reverses: the batch reaches
/// 2,600 events and stops fitting in cache. Two hundred is where the idle cost
/// crosses under a thirtieth of a percent, which is the requirement, and the
/// price is that the index is at most this far behind — well under the time it
/// takes to type a query.
const WINDOW: Duration = Duration::from_millis(200);

/// Read buffer.
///
/// 256 KiB against 64 KiB carries 3.98× as many events a call and is 1.04×
/// faster, so the syscall is not the cost and this is sized for the batch
/// rather than for throughput. At 71 bytes an event it holds about 3,600.
const BUF: usize = 256 * 1024;

// Not in `libc` at the time of writing, and their numeric values are kernel
// ABI, so they are written out rather than derived.
const FAN_REPORT_DIR_FID: u32 = 0x0000_0400;
const FAN_REPORT_NAME: u32 = 0x0000_0800;
/// What the helper has to have opened the group with, or nothing below works.
///
/// Both bits, not either: `DIR_FID` puts the parent directory's handle in the
/// event and `NAME` puts the entry name next to it, and this module needs the
/// pair to build a path. Without them the events still arrive and still parse —
/// they just carry the object's own handle and no name, so every path would be
/// wrong rather than absent, which is the failure worth spending a check on.
const REQUIRED_FLAGS: u32 = FAN_REPORT_DIR_FID | FAN_REPORT_NAME;
const FAN_Q_OVERFLOW: u64 = 0x0000_4000;
const FAN_EVENT_INFO_TYPE_DFID_NAME: u8 = 2;
const FAN_ONDIR: u64 = 0x4000_0000;
const FAN_CREATE: u64 = 0x0000_0100;
const FAN_MOVED_TO: u64 = 0x0000_0080;

/// What identifies a directory across a reboot, a remount and a rename.
///
/// The device number rather than btrfs's subvolume id, because that is what
/// `stat` hands the walk. The event carries the subvolume id instead, and the
/// two are matched through a table with one row a mounted subvolume — seven on
/// this machine — rather than one a directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct DirKey {
    dev: u64,
    ino: u64,
}

/// The inode number a file handle carries, by filesystem.
///
/// The layouts are not documented as stable and are read here anyway, because
/// the alternative — `open_by_handle_at` — needs a capability this process
/// deliberately does not hold. Each was confirmed against `stat` on 5,000 real
/// directories, and the pair `(type, length)` is what distinguishes them:
/// tmpfs and ntfs3 both report type 1 with the fields the other way round, so
/// the type alone is not enough and reading it as though it were puts a
/// generation number where an inode belongs.
fn handle_ino(fh_type: i32, bytes: &[u8]) -> Option<u64> {
    let le64 = |at: usize| -> Option<u64> {
        bytes
            .get(at..at + 8)?
            .try_into()
            .ok()
            .map(u64::from_le_bytes)
    };
    let le32 = |at: usize| -> Option<u64> {
        bytes
            .get(at..at + 4)?
            .try_into()
            .ok()
            .map(|b| u32::from_le_bytes(b) as u64)
    };
    match (fh_type, bytes.len()) {
        // btrfs: ino u64 @0, subvolume id u64 @8, generation u32 @16.
        (0x4d..=0x4f, 20..) => le64(0),
        // tmpfs: generation u32 @0, ino u64 @4. Twelve bytes, and the reason
        // the length is part of the match.
        (1, 12) => le64(4),
        // FILEID_INO32_GEN, which is what ntfs3 and most others use: ino u32
        // @0, generation u32 @4.
        (1, 8) => le32(0),
        _ => None,
    }
}

/// Directory identity to path, for the directories this source covers.
///
/// Held in memory rather than in the index. The index used to carry an inode a
/// row and it was removed for a measured reason — 19 MB of a 200 MB index, on
/// 2.09 million rows, to answer a question the path already answered. What is
/// needed here is not that: the event names a *directory*, and directories are
/// 8.6 files apart, so the same information costs about 6 MB of memory and
/// nothing on disk. If the cost of rebuilding it at startup ever shows up in a
/// measurement, that is the moment to reconsider — not before.
#[derive(Debug, Default)]
struct DirMap {
    by_key: HashMap<DirKey, String>,
}

impl DirMap {
    /// Walk the roots and record what each directory is.
    ///
    /// The order matters and was measured: with the mark placed **after** the
    /// walk, ten files created during it were lost; with the mark placed first,
    /// none were, because everything that happens during the walk is sitting in
    /// the queue when it finishes. The mark here is always already in place —
    /// the helper set it before this process existed — so the walk is safe by
    /// construction.
    fn build(roots: &[std::path::PathBuf], rules: &Rules) -> DirMap {
        let mut map = DirMap::default();
        let mut stack: Vec<std::path::PathBuf> = roots.to_vec();
        while let Some(dir) = stack.pop() {
            let text = path::from_path(&dir);
            if rules.excludes_path(&text) {
                continue;
            }
            let Ok(md) = std::fs::symlink_metadata(&dir) else {
                continue;
            };
            if !md.is_dir() {
                continue;
            }
            map.insert(&md, text);
            let Ok(rd) = std::fs::read_dir(&dir) else {
                continue;
            };
            for e in rd.flatten() {
                if e.file_type().is_ok_and(|t| t.is_dir()) {
                    stack.push(e.path());
                }
            }
        }
        map
    }

    fn insert(&mut self, md: &std::fs::Metadata, path: String) {
        use std::os::unix::fs::MetadataExt;
        self.by_key.insert(
            DirKey {
                dev: md.dev(),
                ino: md.ino(),
            },
            path,
        );
    }

    /// Record a directory that appeared after the map was built.
    ///
    /// Without this a `mkdir` is seen once — the create in its parent, which
    /// does resolve — and then everything inside it arrives against a handle
    /// nothing knows, so a `git clone` would be a stream of unresolvable
    /// events. The walk that [`crate::watch::look`] already queues for a fresh
    /// directory covers the entries; this covers the *events*.
    fn learn(&mut self, path: &str) {
        if let Ok(md) = std::fs::symlink_metadata(path)
            && md.is_dir()
        {
            self.insert(&md, path.to_owned());
        }
    }

    fn path_of(&self, ino: u64, dev_candidates: &[u64]) -> Option<&str> {
        dev_candidates
            .iter()
            .find_map(|&dev| self.by_key.get(&DirKey { dev, ino }))
            .map(String::as_str)
    }

    /// Every device number the walk saw.
    ///
    /// The event gives an inode but not a device — btrfs reports the
    /// superblock's fsid on every event whatever subvolume it came from, which
    /// was measured and is the reason the fsid cannot be used to tell them
    /// apart. So the inode is looked up against each device this source
    /// actually covers, which is seven numbers here, not two million.
    fn devices(&self) -> Vec<u64> {
        let mut v: Vec<u64> = self.by_key.keys().map(|k| k.dev).collect();
        v.sort_unstable();
        v.dedup();
        v
    }
}

/// Every filesystem with a block device behind it, as `mountinfo` sees it.
///
/// The key is the mount id — the first field, unique for the life of a mount —
/// rather than the device, because a disk that is unmounted and mounted again
/// is a *new* mount of the same device and the two have to be told apart: the
/// mark does not survive it. Measured, and it is the surprising half of the
/// pair: the mark does survive the unmount of the path it was placed through,
/// but a remount of the filesystem itself does not bring it back. A fresh
/// group saw the same write the old one had gone silent for.
fn mounts(text: &str) -> HashSet<(u32, String)> {
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
///
/// `poll` on it returns `POLLERR | POLLPRI` when the mount table changes and
/// nothing otherwise — measured at 300 ms from the mount to the wake, with a
/// negative control that stayed quiet. It is the only way this process learns a
/// disk was plugged in, because a filesystem mark covers one superblock and a
/// new disk is a new superblock: the events simply never come, measured, and
/// nothing about that is visible from inside the fanotify descriptor.
fn mountinfo() -> Option<std::fs::File> {
    std::fs::File::open("/proc/self/mountinfo").ok()
}

/// One event, as far as this module cares: which directory, what name, and
/// whether the entry might be new.
struct Seen {
    parent_ino: u64,
    name: String,
    fresh: bool,
    is_dir: bool,
}

/// Pull every event out of one buffer.
///
/// Returns `None` for the whole batch if the kernel reported that it dropped
/// events. The overflow record carries no information at all — `event_len`
/// equals `metadata_len`, the descriptor is `FAN_NOFD` and there is no info
/// record, all measured — so there is nothing to be selective about and the
/// only honest response is to say the subtree was lost.
fn parse(buf: &[u8], out: &mut Vec<Seen>) -> bool {
    let mut at = 0usize;
    let mut ok = true;
    while at + 24 <= buf.len() {
        let event_len = u32::from_le_bytes(buf[at..at + 4].try_into().unwrap()) as usize;
        if event_len < 24 || at + event_len > buf.len() {
            break;
        }
        let metadata_len = u16::from_le_bytes(buf[at + 6..at + 8].try_into().unwrap()) as usize;
        let mask = u64::from_le_bytes(buf[at + 8..at + 16].try_into().unwrap());
        if mask & FAN_Q_OVERFLOW != 0 {
            ok = false;
            at += event_len;
            continue;
        }
        let mut p = at + metadata_len.max(24);
        let end = at + event_len;
        while p + 4 <= end {
            let info_type = buf[p];
            let info_len = u16::from_le_bytes(buf[p + 2..p + 4].try_into().unwrap()) as usize;
            if info_len < 4 || p + info_len > end {
                break;
            }
            if info_type == FAN_EVENT_INFO_TYPE_DFID_NAME {
                // header 4 + fsid 8, then a `struct file_handle`: bytes u32,
                // type i32, then the handle, then the NUL-terminated name.
                let fh = p + 12;
                if fh + 8 <= p + info_len {
                    let hb = u32::from_le_bytes(buf[fh..fh + 4].try_into().unwrap()) as usize;
                    let ht = i32::from_le_bytes(buf[fh + 4..fh + 8].try_into().unwrap());
                    let body = fh + 8;
                    if body + hb <= p + info_len
                        && let Some(ino) = handle_ino(ht, &buf[body..body + hb])
                    {
                        let tail = &buf[body + hb..p + info_len];
                        let n = tail.iter().position(|&c| c == 0).unwrap_or(tail.len());
                        if n > 0
                            && let Ok(name) = std::str::from_utf8(&tail[..n])
                        {
                            out.push(Seen {
                                parent_ino: ino,
                                name: name.to_owned(),
                                fresh: mask & (FAN_CREATE | FAN_MOVED_TO) != 0,
                                is_dir: mask & FAN_ONDIR != 0,
                            });
                        }
                    }
                }
                break;
            }
            p += info_len;
        }
        at += event_len;
    }
    ok
}

/// Take the descriptor the helper left, if it left one.
///
/// Absent is the ordinary case — nothing is installed, or the helper is not
/// being used — and it is not an error: [`try_start`] returns `None` and the
/// caller falls back to inotify.
fn inherited() -> Option<OwnedFd> {
    let raw: RawFd = std::env::var(FD_ENV).ok()?.trim().parse().ok()?;
    if raw < 0 {
        return None;
    }
    // **Check what it is before reading it.** A stale environment variable
    // pointing at some other open file would otherwise be read as a stream of
    // events, and a group opened without the two report flags would parse into
    // paths that are wrong rather than missing. `fdinfo` answers both: the
    // first line of a fanotify descriptor is `fanotify flags:%x event-flags:%x`
    // and nothing else produces it.
    let info = std::fs::read_to_string(format!("/proc/self/fdinfo/{raw}")).ok()?;
    let flags = info.lines().find_map(|l| {
        let rest = l.strip_prefix("fanotify flags:")?;
        let hex = rest.split_whitespace().next()?;
        u32::from_str_radix(hex.trim_start_matches("0x"), 16).ok()
    })?;
    if flags & REQUIRED_FLAGS != REQUIRED_FLAGS {
        return None;
    }
    // SAFETY: the descriptor is open — `fdinfo` for it was just read — and the
    // environment variable is the helper's contract for handing over ownership,
    // so nothing else in this process holds it.
    Some(unsafe { OwnedFd::from_raw_fd(raw) })
}

#[derive(Debug)]
struct FanWatch {
    stopped: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Filesystems that appeared after the marks were set.
    ///
    /// They are not covered and cannot be from here: placing a mark needs
    /// `CAP_SYS_ADMIN` and this process deliberately has none, so the honest
    /// thing is to name them and let the layer above say so. That is what this
    /// side of [`WatchHandle`] is for, and `scourd` already prints it.
    uncovered: Arc<Mutex<Vec<String>>>,
}

impl WatchHandle for FanWatch {
    fn unwatched(&self) -> Vec<String> {
        self.uncovered.lock().map(|v| v.clone()).unwrap_or_default()
    }

    /// Nothing to do, and that is the point of this backend.
    ///
    /// inotify needs to be told about a subtree that appeared after the watch
    /// did, because it holds a watch per directory and a new directory has
    /// none. A filesystem mark covers the superblock, so a directory created a
    /// moment ago is already watched — measured: a file written inside a
    /// subvolume created after the mark produced its event like any other.
    fn cover(&self, _path: &str) {}

    fn stop(mut self: Box<Self>) {
        self.stopped.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

impl Drop for FanWatch {
    /// Ask the reader to stop but do not wait for it.
    ///
    /// `poll` runs on a half-second timeout, so this is bounded; `stop` is the
    /// one that waits, which is what its documentation promises and what a
    /// `Drop` cannot offer without blocking whoever let the handle go.
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
    }
}

/// Start watching, or say this mechanism is not available here.
///
/// `None` means "not this one" rather than "no watching": the caller falls back
/// to inotify, which is why nothing in here panics or reports a failure for the
/// ordinary case of a machine where the helper was never installed.
pub fn try_start(
    source: &FsSource,
    opts: &ScanOptions,
    sink: Arc<dyn ChangeSink>,
) -> Option<Result<Box<dyn WatchHandle>>> {
    let fd = inherited()?;
    let roots: Vec<std::path::PathBuf> = source.roots().to_vec();
    let id = source.source_id();
    let real_modes = source.real_modes();
    let rules = Arc::new(Rules::from_options(opts));
    let stopped = Arc::new(AtomicBool::new(false));
    let handle_stop = Arc::clone(&stopped);

    // **Which mechanism is running has to be visible.** The fallback to inotify
    // is silent by design — a machine without the helper is the ordinary case,
    // not a failure — and that is exactly what makes the successful case worth
    // one line: otherwise the only way to tell a filesystem-wide watch from
    // 296,711 individual ones is to count watches in `/proc`.
    eprintln!("scourd: watching with fanotify (one mark a filesystem)");

    let uncovered: Arc<Mutex<Vec<String>>> = Arc::default();
    let theirs = Arc::clone(&uncovered);
    let thread = std::thread::Builder::new()
        .name("scour-fanotify".into())
        .spawn(move || drain(fd, roots, id, real_modes, rules, sink, stopped, theirs))
        .ok()?;
    Some(Ok(Box::new(FanWatch {
        stopped: Arc::clone(&handle_stop),
        thread: Some(thread),
        uncovered,
    })))
}

/// The reader.
///
/// Wakes at most five times a second by construction: `poll` returns as soon as
/// the first event lands, and then the window runs before anything is read, so
/// what arrives during it is taken in one call. Everything the window collected
/// is reduced to distinct paths before a single `stat` is made — the same file
/// written a hundred times in the window is one look, which is where the second
/// saving is, and it does not show up in an event count.
fn drain(
    fd: OwnedFd,
    roots: Vec<std::path::PathBuf>,
    id: SourceId,
    real_modes: bool,
    rules: Arc<Rules>,
    sink: Arc<dyn ChangeSink>,
    stopped: Arc<AtomicBool>,
    uncovered: Arc<Mutex<Vec<String>>>,
) {
    let mut map = DirMap::build(&roots, &rules);
    let mut devices = map.devices();
    let mut buf = vec![0u8; BUF];
    let mut seen: Vec<Seen> = Vec::new();
    let raw = fd.as_raw_fd();

    // The mount table, watched beside the events rather than on a thread of its
    // own: one `poll` over two descriptors costs nothing extra and keeps the
    // whole watcher a single place that can be stopped.
    let mut mi = mountinfo();
    let mut known = mi
        .as_mut()
        .and_then(|f| {
            let mut s = String::new();
            f.read_to_string(&mut s).ok()?;
            Some(mounts(&s))
        })
        .unwrap_or_default();

    while !stopped.load(Ordering::Relaxed) {
        let mut pfds = [
            libc::pollfd {
                fd: raw,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: mi.as_ref().map_or(-1, |f| f.as_raw_fd()),
                // `POLLERR` is not requested — it always arrives — but `POLLPRI`
                // is what the mount table signals a change with.
                events: libc::POLLPRI,
                revents: 0,
            },
        ];
        // A bounded wait so that dropping the handle is noticed even on a
        // filesystem where nothing ever happens.
        let pr = unsafe { libc::poll(pfds.as_mut_ptr(), 2, 500) };
        if pr <= 0 {
            continue;
        }

        if pfds[1].revents != 0 {
            // Re-read from the start, or `poll` keeps reporting the same change
            // and the loop spins.
            if let Some(f) = mi.as_mut() {
                let mut s = String::new();
                if f.seek(SeekFrom::Start(0)).is_ok() && f.read_to_string(&mut s).is_ok() {
                    let now = mounts(&s);
                    let fresh: Vec<String> = now
                        .difference(&known)
                        .map(|(_, at)| at.clone())
                        .filter(|at| !rules.excludes_path(at))
                        .collect();
                    known = now;
                    if !fresh.is_empty() {
                        // Its contents can still be indexed — a walk reads what
                        // is there. What cannot happen from here is watching it:
                        // a new filesystem is a new superblock and a mark needs
                        // a privilege this process does not have.
                        if let Ok(mut u) = uncovered.lock() {
                            for at in &fresh {
                                if !u.contains(at) {
                                    u.push(at.clone());
                                }
                            }
                        }
                        for at in fresh {
                            sink.emit(Change::Rescan { path: at });
                        }
                    }
                }
            }
        }

        if pfds[0].revents & libc::POLLIN == 0 {
            continue;
        }
        std::thread::sleep(WINDOW);

        seen.clear();
        let mut lost = false;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let n = unsafe { libc::read(raw, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
            if !parse(&buf[..n as usize], &mut seen) {
                lost = true;
            }
            // A burst larger than the buffer is read out in this loop rather
            // than left for the next window, but not forever: the drain rate
            // is 4.6 million events a second, so two seconds is far past any
            // real batch and exists only so a pathological producer cannot
            // hold the thread.
            if Instant::now() > deadline {
                lost = true;
                break;
            }
        }

        if lost {
            // Nothing in the overflow record says what was missed, so the
            // subtree is the only unit available.
            for r in &roots {
                sink.emit(Change::Rescan {
                    path: path::from_path(r),
                });
            }
            continue;
        }

        // Distinct paths, keeping "this might be new" if any event said so.
        let mut batch: HashMap<String, (bool, bool)> = HashMap::new();
        let mut unresolved = false;
        for s in seen.drain(..) {
            let Some(dir) = map.path_of(s.parent_ino, &devices) else {
                unresolved = true;
                continue;
            };
            let full = if dir.ends_with('/') {
                format!("{dir}{}", s.name)
            } else {
                format!("{dir}/{}", s.name)
            };
            if rules.excludes_path(&full) {
                continue;
            }
            let e = batch.entry(full).or_insert((false, false));
            e.0 |= s.fresh;
            e.1 |= s.is_dir;
        }

        for (full, (fresh, is_dir)) in batch {
            if fresh && is_dir {
                map.learn(&full);
                devices = map.devices();
            }
            crate::watch::look(id, real_modes, &full, fresh, sink.as_ref());
        }

        // A handle nothing knows is a directory that appeared without its
        // creation being seen — a btrfs snapshot, which was measured to make
        // 201 files visible behind a single event, or a tree moved in from
        // outside the roots. Neither can be resolved from the event, and both
        // are exactly what `Rescan` is for.
        if unresolved {
            for r in &roots {
                sink.emit(Change::Rescan {
                    path: path::from_path(r),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_handle_is_read_by_its_type_and_its_length_together() {
        // btrfs: inode first, then the subvolume id.
        let mut btrfs = Vec::new();
        btrfs.extend_from_slice(&430u64.to_le_bytes());
        btrfs.extend_from_slice(&257u64.to_le_bytes());
        btrfs.extend_from_slice(&0x90u32.to_le_bytes());
        assert_eq!(handle_ino(0x4d, &btrfs), Some(430));

        // ntfs3 and friends: 32-bit inode, then a generation.
        let mut ntfs = Vec::new();
        ntfs.extend_from_slice(&0x0003_0f45u32.to_le_bytes());
        ntfs.extend_from_slice(&0x27u32.to_le_bytes());
        assert_eq!(handle_ino(1, &ntfs), Some(0x0003_0f45));

        // tmpfs uses the same type number with the fields the other way round,
        // which is why the length is part of the decision.
        let mut tmp = Vec::new();
        tmp.extend_from_slice(&0x8f45_42b1u32.to_le_bytes());
        tmp.extend_from_slice(&99u64.to_le_bytes());
        assert_eq!(handle_ino(1, &tmp), Some(99));
    }

    #[test]
    fn an_unknown_handle_is_refused_rather_than_guessed() {
        assert_eq!(handle_ino(0x63, &[1, 2, 3, 4]), None);
        assert_eq!(
            handle_ino(1, &[1, 2, 3]),
            None,
            "too short to hold an inode"
        );
        assert_eq!(handle_ino(0x4d, &[0; 12]), None, "btrfs handles are longer");
    }

    #[test]
    fn a_truncated_buffer_does_not_take_the_reader_down() {
        let mut out = Vec::new();
        for len in 0..40usize {
            let buf = vec![0u8; len];
            assert!(parse(&buf, &mut out), "len {len}");
        }
        // An event whose length runs past the buffer is dropped, not read.
        let mut buf = vec![0u8; 24];
        buf[0..4].copy_from_slice(&9999u32.to_le_bytes());
        assert!(parse(&buf, &mut out));
        assert!(out.is_empty());
    }

    #[test]
    fn only_a_filesystem_with_a_disk_behind_it_counts_as_a_mount() {
        let text = "\
25 1 0:23 / /proc rw - proc proc rw
26 1 0:5 / /sys rw - sysfs sysfs rw
31 1 259:5 /@ / rw - btrfs /dev/nvme0n1p5 rw,subvolid=256
48 1 259:5 /@home /home rw - btrfs /dev/nvme0n1p5 rw,subvolid=257
60 1 259:9 / /mnt/depo rw - ntfs3 /dev/nvme1n1p2 rw
61 1 0:44 / /run/user/1000 rw - tmpfs tmpfs rw
";
        let m = mounts(text);
        let mut at: Vec<&str> = m.iter().map(|(_, p)| p.as_str()).collect();
        at.sort_unstable();
        assert_eq!(at, ["/", "/home", "/mnt/depo"]);
    }

    #[test]
    fn two_mounts_of_one_disk_are_two_mounts_because_a_remount_loses_the_mark() {
        // Both lines are `/dev/nvme0n1p5`, and they are deliberately *not*
        // folded together: what matters here is not which superblock it is but
        // whether this particular mount is one the marks were placed before.
        let text = "\
31 1 259:5 /@ / rw - btrfs /dev/nvme0n1p5 rw,subvolid=256
48 1 259:5 /@home /home rw - btrfs /dev/nvme0n1p5 rw,subvolid=257
";
        assert_eq!(mounts(text).len(), 2);

        // The same filesystem unmounted and mounted again comes back with a
        // different mount id, which is what makes it visible as new.
        let after = "49 1 259:5 /@home /home rw - btrfs /dev/nvme0n1p5 rw,subvolid=257\n";
        let before = mounts(text);
        let now = mounts(after);
        assert_eq!(now.difference(&before).count(), 1);
    }

    #[test]
    fn a_mount_point_with_a_space_in_it_survives_being_read() {
        let text = "70 1 259:9 / /mnt/My\\040Disk rw - ntfs3 /dev/sdb1 rw\n";
        let m = mounts(text);
        assert_eq!(
            m.iter().next().map(|(_, p)| p.as_str()),
            Some("/mnt/My Disk")
        );
    }

    #[test]
    fn a_dropped_batch_is_reported_rather_than_silently_shortened() {
        let mut buf = vec![0u8; 24];
        buf[0..4].copy_from_slice(&24u32.to_le_bytes());
        buf[6..8].copy_from_slice(&24u16.to_le_bytes());
        buf[8..16].copy_from_slice(&FAN_Q_OVERFLOW.to_le_bytes());
        let mut out = Vec::new();
        assert!(!parse(&buf, &mut out), "overflow has to be visible");
        assert!(out.is_empty(), "and it carries nothing to act on");
    }
}
