//! Reading the descriptor: a buffer of events parsed, named, and held for a
//! window so a burst becomes one change per path.

use super::*;
use crate::path;
use scour_core::{Change, ChangeSink};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

/// How long to let events pile up before draining them.
/// At 2,120 events a second: 5 wakes and 0.078% of a core here, against 1,417
/// wakes and 0.838% at 0 ms. Past 500 ms the batch stops fitting in cache.
const WINDOW: Duration = Duration::from_millis(200);

/// Read buffer. 256 KiB carries 3.98× as many events a call as 64 KiB for 1.04×
/// the speed, so it is sized for the batch: about 3,600 events at 71 bytes each.
pub(super) const BUF: usize = 256 * 1024;

/// The most events one window collects before it stops; the rest are read by the
/// next. The kernel queue is unlimited, so this is the memory bound the deadline
/// is not: about 20 MB of [`Seen`], reached to within one buffer — [`parse`]
/// empties one.
pub(super) const MAX_SEEN: usize = 262_144;

/// The capacity one window may leave behind for the next.
/// `clear` keeps capacity, so without this a burst's peak becomes the floor.
/// Ten thousand events is about 800 KB.
pub(super) const KEEP_SEEN: usize = 10_000;

pub(super) const FAN_Q_OVERFLOW: u64 = 0x0000_4000;

pub(super) const FAN_EVENT_INFO_TYPE_DFID_NAME: u8 = 2;

const FAN_ONDIR: u64 = 0x4000_0000;

pub(super) const FAN_CREATE: u64 = 0x0000_0100;

const FAN_DELETE: u64 = 0x0000_0200;

const FAN_MOVED_FROM: u64 = 0x0000_0040;

const FAN_MOVED_TO: u64 = 0x0000_0080;

const FAN_CLOSE_WRITE: u64 = 0x0000_0008;

/// One event, as far as this module cares: which directory, what name, and
/// whether the entry might be new.
pub(super) struct Seen {
    pub(super) parent_ino: u64,
    /// Where the parent is: an inode number means nothing without it.
    pub(super) parent_fs: FsTag,
    pub(super) name: String,
    pub(super) fresh: bool,
    pub(super) is_dir: bool,
    /// A name came or went in the directory, which changes the directory's own
    /// times — and the kernel says nothing about the directory.
    pub(super) entries: bool,
    /// The kernel says the content is final: a descriptor opened for writing was
    /// closed — `munmap` for a mapping. See [`crate::revisit::forget`].
    pub(super) settled: bool,
}

/// Collect one window's events, and say whether anything was lost.
/// Only the kernel can say that, with its overflow record. [`MAX_SEEN`] and the
/// two-second deadline end a window, and lose nothing: the group's queue is
/// unlimited, so what was not read waits there for the next window. They used
/// to report lost, and a `cargo clean` — 850,000 events, most under an excluded
/// `target` and counted before any rule — sent every root of every source to
/// be walked again.
pub(super) fn fill(
    buf: &mut [u8],
    seen: &mut Vec<Seen>,
    mut read: impl FnMut(&mut [u8]) -> isize,
) -> bool {
    seen.clear();
    // Before the window, so the last burst's peak does not become the floor.
    if seen.capacity() > KEEP_SEEN {
        seen.shrink_to(KEEP_SEEN);
    }
    let mut lost = false;
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let n = read(buf);
        if n <= 0 {
            break;
        }
        if !parse(&buf[..n as usize], seen) {
            lost = true;
        }
        // The memory bound, checked before the time one — see [`MAX_SEEN`]. The
        // rest stays queued in the kernel and opens the next window.
        if seen.len() >= MAX_SEEN {
            break;
        }
        // A burst larger than the buffer is read out here, but not forever: at
        // 4.6 million events a second, two seconds is far past any real batch.
        if Instant::now() > deadline {
            break;
        }
    }
    lost
}

/// Pull every event out of one buffer.
/// `None` for the whole batch when the kernel dropped events: the overflow record
/// carries nothing — `event_len` equals `metadata_len` and the descriptor is `FAN_NOFD`.
pub(super) fn parse(buf: &[u8], out: &mut Vec<Seen>) -> bool {
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
                        let half =
                            |at: usize| u32::from_le_bytes(buf[at..at + 4].try_into().unwrap());
                        let parent_fs = FsTag {
                            fsid: [half(p + 4), half(p + 8)],
                            root: btrfs_root(ht, &buf[body..body + hb]),
                        };
                        let tail = &buf[body + hb..p + info_len];
                        let n = tail.iter().position(|&c| c == 0).unwrap_or(tail.len());
                        if n > 0
                            && let Ok(name) = std::str::from_utf8(&tail[..n])
                        {
                            out.push(Seen {
                                parent_ino: ino,
                                parent_fs,
                                name: name.to_owned(),
                                fresh: mask & (FAN_CREATE | FAN_MOVED_TO) != 0,
                                is_dir: mask & FAN_ONDIR != 0,
                                entries: mask
                                    & (FAN_CREATE | FAN_DELETE | FAN_MOVED_FROM | FAN_MOVED_TO)
                                    != 0,
                                settled: mask & FAN_CLOSE_WRITE != 0,
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

/// The path an event is about. The kernel reports an event on a directory itself
/// — its attributes, its removal — with the directory's own handle and the name
/// `.`, which read as a child made a row called `.` in every directory whose mode
/// or times changed: seven of them, removed by every reconciling walk and made
/// again by the next such event. A root is not a row of its own, so its `.` is
/// nobody's business.
pub(super) fn event_path(dir: &str, name: &str, roots: &[std::path::PathBuf]) -> Option<String> {
    if name == "." {
        let own = if dir.len() > 1 {
            dir.trim_end_matches('/')
        } else {
            dir
        };
        let is_root = roots
            .iter()
            .any(|r| path::from_path(r).trim_end_matches('/') == own);
        return (!is_root).then(|| own.to_owned());
    }
    Some(if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    })
}

/// The reader. One thread, however many sources.
/// At most five wakes a second: `poll` returns on the first event and the window
/// runs before anything is read, then its events reduce to distinct paths.
pub(super) fn drain(fd: OwnedFd) {
    let mut buf = vec![0u8; BUF];
    let mut seen: Vec<Seen> = Vec::new();
    // One buffer for every path the reader resolves, for the life of the thread:
    // the allocation the caller used to make per event, moved rather than added.
    let mut dir = String::new();
    let raw = fd.as_raw_fd();

    // The mount table, watched beside the events: one `poll` over two descriptors
    // keeps the whole watcher a single place that can be stopped.
    let mut mi = mountinfo();
    let mut known = mi
        .as_mut()
        .and_then(|f| {
            let mut s = String::new();
            f.read_to_string(&mut s).ok()?;
            Some(mounts(&s))
        })
        .unwrap_or_default();

    loop {
        let mut pfds = [
            libc::pollfd {
                fd: raw,
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: mi.as_ref().map_or(-1, |f| f.as_raw_fd()),
                // `POLLPRI` is how the mount table signals a change; `POLLERR` always arrives.
                events: libc::POLLPRI,
                revents: 0,
            },
        ];
        let pr = unsafe { libc::poll(pfds.as_mut_ptr(), 2, 500) };
        if pr <= 0 {
            continue;
        }

        if pfds[1].revents != 0 {
            // Re-read from the start, or `poll` keeps reporting the same change.
            if let Some(f) = mi.as_mut() {
                let mut s = String::new();
                if f.seek(SeekFrom::Start(0)).is_ok() && f.read_to_string(&mut s).is_ok() {
                    let now = mounts(&s);
                    let fresh: Vec<String> =
                        now.difference(&known).map(|(_, at)| at.clone()).collect();
                    let gone: Vec<String> =
                        known.difference(&now).map(|(_, at)| at.clone()).collect();
                    known = now;
                    if !fresh.is_empty() || !gone.is_empty() {
                        note_mounts(&fresh, &gone);
                    }
                }
            }
        }

        if pfds[0].revents & libc::POLLIN == 0 {
            continue;
        }
        std::thread::sleep(WINDOW);

        let lost = fill(&mut buf, &mut seen, |b| unsafe {
            libc::read(raw, b.as_mut_ptr().cast(), b.len())
        });

        if !window(&mut seen, lost, &mut dir) {
            return;
        }
    }
}

/// One window's events: read and taught to the map under the lock, sent after it.
/// False when the lock is poisoned, which ends the reader.
pub(super) fn window(seen: &mut Vec<Seen>, lost: bool, dir: &mut String) -> bool {
    // **The lock is for the map, and nothing is sent while it is held.** A send
    // waits while the engine's queue is full, and the engine's worker takes this
    // lock to learn a subtree before walking it: the two waited on each other for
    // good, and nothing was indexed again until a restart. So the window is read
    // and the map taught under the lock, and what to look at is done after it.
    let mut looks: Vec<Look> = Vec::new();
    let mut rescans: Vec<(Arc<dyn ChangeSink>, String)> = Vec::new();
    {
        let Ok(mut subs) = SUBS.lock() else {
            return false;
        };

        if lost {
            // Nothing in the overflow record says what was missed, so the subtree is the
            // only unit available — for everyone, because the queue was shared.
            for s in subs.iter().filter(|s| s.live.load(Ordering::Relaxed)) {
                for r in &s.roots {
                    rescans.push((Arc::clone(&s.sink), path::from_path(r)));
                }
            }
        } else {
            // Distinct paths a subscriber, keeping "this might be new" if any event said so.
            let mut batch: HashMap<(usize, String), (bool, bool, bool)> = HashMap::new();
            // Directories whose names changed, looked at once each after their children.
            let mut parents: std::collections::HashSet<(usize, String)> = Default::default();
            for ev in seen.drain(..) {
                // The subscriber that walked this directory owns the path. An event nobody
                // recognises is **dropped, not escalated**: it is almost always another
                // source's tree, and a rescan of every root would turn traffic into a storm.
                let Some(i) = subs.iter().enumerate().find_map(|(i, s)| {
                    if !s.live.load(Ordering::Relaxed) {
                        return None;
                    }
                    s.map
                        .path_of(ev.parent_ino, &ev.parent_fs, dir)
                        .then_some(i)
                }) else {
                    continue;
                };
                let Some(full) = event_path(dir, &ev.name, &subs[i].roots) else {
                    continue;
                };
                if subs[i].rules.excludes_path(&full) {
                    continue;
                }
                // Its row's times moved with the name; a root has no row.
                if ev.entries
                    && ev.name != "."
                    && let Some(own) = event_path(dir, ".", &subs[i].roots)
                {
                    parents.insert((i, own));
                }
                let e = batch.entry((i, full)).or_insert((false, false, false));
                e.0 |= ev.fresh;
                e.1 |= ev.is_dir;
                e.2 |= ev.settled;
            }

            // A directory that also spoke for itself is looked at with its own event.
            parents.retain(|k| !batch.contains_key(k) && !subs[k.0].rules.excludes_path(&k.1));

            for ((i, full), (fresh, is_dir, settled)) in batch {
                let s = &mut subs[i];
                if fresh && is_dir {
                    s.map.learn(&full);
                }
                looks.push(Look {
                    sink: Arc::clone(&s.sink),
                    id: s.id,
                    real_modes: s.real_modes,
                    path: full,
                    fresh,
                    settled: Some(settled),
                });
            }

            // One `stat` a directory a window, for its times: a folder sorted by date
            // stayed where it was when its last child came or went, until a walk —
            // most of the rows a reconciling walk found changed were directories.
            for (i, dir) in parents {
                let s = &subs[i];
                looks.push(Look {
                    sink: Arc::clone(&s.sink),
                    id: s.id,
                    real_modes: s.real_modes,
                    path: dir,
                    fresh: false,
                    settled: None,
                });
            }
        }
    }

    for (sink, at) in rescans {
        sink.emit(Change::Rescan { path: at });
    }
    for l in looks {
        let md = crate::watch::look(l.id, l.real_modes, &l.path, l.fresh, l.sink.as_ref());
        // A write through a mapping produces no event, so a path that has just spoken
        // is worth another look — unless the kernel has said the content is final.
        match l.settled {
            Some(true) => crate::revisit::forget(&l.path),
            Some(false) => crate::revisit::note(&l.path, l.id, l.real_modes, &l.sink, md.as_ref()),
            None => {}
        }
    }
    true
}
