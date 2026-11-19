//! Watching a network share as the person who added it. Windows file servers
//! send change notifications over SMB (`ReadDirectoryChangesW` on a directory
//! of the share, the whole tree below it); the directory has to be opened
//! with that person's identity, since the share does not know SYSTEM. So the
//! watching thread takes the identity for its whole life, as the walk does.
//!
//! A notification is a hint to look again, as everywhere. A buffer the server
//! could not fill in time comes back empty — then the root is walked. A
//! connection that drops ends the read: the share is opened again after a
//! pause and walked, since what happened meanwhile is unknown.

use crate::usn::{Act, merge};

/// One `FILE_NOTIFY_INFORMATION` chain: the action and the name relative to
/// the watched directory, with `\` between components.
pub(crate) fn parse(buf: &[u8]) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while let Some(head) = buf.get(at..at + 12) {
        let next = u32::from_le_bytes(head[0..4].try_into().unwrap()) as usize;
        let action = u32::from_le_bytes(head[4..8].try_into().unwrap());
        let len = u32::from_le_bytes(head[8..12].try_into().unwrap()) as usize;
        let Some(raw) = buf.get(at + 12..at + 12 + len) else {
            break;
        };
        let units: Vec<u16> = raw
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        out.push((action, String::from_utf16_lossy(&units)));
        if next == 0 {
            break;
        }
        at += next;
    }
    out
}

/// A chain as one action per path under `root`, in the order they first came.
pub(crate) fn settle(root: &str, records: &[(u32, String)]) -> Vec<(String, Act)> {
    let mut out: Vec<(String, Act)> = Vec::new();
    for (action, name) in records {
        let act = match action {
            // FILE_ACTION_ADDED, FILE_ACTION_RENAMED_NEW_NAME
            1 | 5 => Act::Fresh,
            // FILE_ACTION_REMOVED, FILE_ACTION_RENAMED_OLD_NAME
            2 | 4 => Act::Gone,
            _ => Act::Changed,
        };
        let rel = name.replace('\\', "/");
        let path = if root.ends_with('/') {
            format!("{root}{rel}")
        } else {
            format!("{root}/{rel}")
        };
        match out.iter_mut().find(|(p, _)| *p == path) {
            Some(slot) => slot.1 = merge(slot.1, act),
            None => out.push((path, act)),
        }
    }
    out
}

#[cfg(windows)]
pub(crate) use imp::start;

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use scour_core::{Change, ChangeSink, Error, Result, ScanOptions, WatchHandle};
    use windows_sys::Win32::Foundation::{
        CloseHandle, HANDLE, INVALID_HANDLE_VALUE, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OVERLAPPED, FILE_LIST_DIRECTORY,
        FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_CREATION, FILE_NOTIFY_CHANGE_DIR_NAME,
        FILE_NOTIFY_CHANGE_FILE_NAME, FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadDirectoryChangesW,
    };
    use windows_sys::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
    use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};

    use super::{parse, settle};
    use crate::rules::Rules;
    use crate::scan::{FsSource, WalkAs};
    use crate::usn::Act;
    use crate::watch::{SharedRules, translate};

    /// The most a server sends in one notification over the network.
    const BUFFER: usize = 64 * 1024;
    /// How long a dropped share is left before it is opened again.
    const AGAIN: Duration = Duration::from_secs(30);

    /// Watch every root of `source` from a thread of its own that holds the
    /// owner's identity. Fails when the first root cannot be opened at all,
    /// so the engine walks it on a timer instead.
    pub(crate) fn start(
        source: &FsSource,
        walk_as: WalkAs,
        rules: SharedRules,
        sink: Arc<dyn ChangeSink>,
    ) -> Result<Box<dyn WatchHandle>> {
        let stopped = Arc::new(AtomicBool::new(false));
        let mut opened_any = false;
        for root in source.roots() {
            let (tx, rx) = std::sync::mpsc::channel::<bool>();
            let watcher = Watcher {
                root: crate::path::from_path(root),
                native: root.as_os_str().encode_wide().chain([0]).collect(),
                id: source.source_id(),
                real_modes: source.real_modes(),
                rules: Arc::clone(&rules),
                sink: Arc::clone(&sink),
                stopped: Arc::clone(&stopped),
                walk_as: walk_as.clone(),
            };
            std::thread::Builder::new()
                .name("share watch".into())
                .spawn(move || watcher.run(tx))
                .map_err(|e| Error::Io {
                    detail: format!("cannot start a share watcher: {e}"),
                })?;
            opened_any |= rx.recv_timeout(Duration::from_secs(20)).unwrap_or(false);
        }
        if !opened_any {
            stopped.store(true, Ordering::Relaxed);
            return Err(Error::Io {
                detail: "the share could not be opened for watching".into(),
            });
        }
        let name = scour_core::Source::describe(source).name;
        scour_core::note!("scourd: {name} is watched through the server's change notifications");
        Ok(Box::new(Share { stopped, rules }))
    }

    struct Watcher {
        root: String,
        native: Vec<u16>,
        id: scour_core::SourceId,
        real_modes: bool,
        rules: SharedRules,
        sink: Arc<dyn ChangeSink>,
        stopped: Arc<AtomicBool>,
        walk_as: WalkAs,
    }

    impl Watcher {
        fn run(self, first: std::sync::mpsc::Sender<bool>) {
            // For as long as this thread watches: the handle is opened, and
            // read, as the owner.
            let _as = (self.walk_as.0)();
            let mut told = false;
            let mut buf = vec![0u8; BUFFER];
            while !self.stopped.load(Ordering::Relaxed) {
                let dir = self.open();
                if !told {
                    let _ = first.send(dir.is_some());
                    told = true;
                }
                if let Some(dir) = dir {
                    self.read(dir, &mut buf);
                    // SAFETY: the handle opened above, closed once.
                    unsafe { CloseHandle(dir) };
                }
                if self.stopped.load(Ordering::Relaxed) {
                    break;
                }
                // Dropped or never there: after a pause, open again and walk,
                // since what happened in between is not known.
                let mut waited = Duration::ZERO;
                while waited < AGAIN && !self.stopped.load(Ordering::Relaxed) {
                    std::thread::sleep(Duration::from_millis(500));
                    waited += Duration::from_millis(500);
                }
                self.sink.emit(Change::Rescan {
                    path: self.root.clone(),
                });
            }
        }

        fn open(&self) -> Option<HANDLE> {
            // SAFETY: a null-terminated path; the handle is closed by the caller.
            let h = unsafe {
                CreateFileW(
                    self.native.as_ptr(),
                    FILE_LIST_DIRECTORY,
                    FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                    std::ptr::null(),
                    OPEN_EXISTING,
                    FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OVERLAPPED,
                    std::ptr::null_mut(),
                )
            };
            (h != INVALID_HANDLE_VALUE).then_some(h)
        }

        /// Read notifications until the share goes away or the watch stops.
        fn read(&self, dir: HANDLE, buf: &mut [u8]) {
            // SAFETY: an unnamed manual-reset event, closed below.
            let event = unsafe { CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
            if event.is_null() {
                return;
            }
            let filter = FILE_NOTIFY_CHANGE_FILE_NAME
                | FILE_NOTIFY_CHANGE_DIR_NAME
                | FILE_NOTIFY_CHANGE_SIZE
                | FILE_NOTIFY_CHANGE_LAST_WRITE
                | FILE_NOTIFY_CHANGE_CREATION
                | FILE_NOTIFY_CHANGE_ATTRIBUTES;
            loop {
                // SAFETY: zeroed is a valid OVERLAPPED; it and `buf` outlive the
                // read, which is waited for or cancelled before either goes.
                let mut ov: OVERLAPPED = unsafe { std::mem::zeroed() };
                ov.hEvent = event;
                let issued = unsafe {
                    ReadDirectoryChangesW(
                        dir,
                        buf.as_mut_ptr() as *mut _,
                        buf.len() as u32,
                        1,
                        filter,
                        std::ptr::null_mut(),
                        &mut ov,
                        None,
                    )
                };
                if issued == 0 {
                    break;
                }
                // Woken once a second to see whether the watch has stopped.
                let done = loop {
                    if self.stopped.load(Ordering::Relaxed) {
                        // SAFETY: the read issued above, then waited out.
                        unsafe {
                            CancelIoEx(dir, &ov);
                            let mut got = 0u32;
                            GetOverlappedResult(dir, &ov, &mut got, 1);
                        }
                        break None;
                    }
                    // SAFETY: an event this function owns.
                    if unsafe { WaitForSingleObject(event, 1000) } == WAIT_OBJECT_0 {
                        let mut got = 0u32;
                        // SAFETY: the read has completed.
                        let ok = unsafe { GetOverlappedResult(dir, &ov, &mut got, 0) };
                        break Some((ok != 0, got as usize));
                    }
                };
                let Some((ok, got)) = done else { break };
                if !ok {
                    // The connection went: the caller opens it again.
                    break;
                }
                if got == 0 {
                    // More happened than the buffer held.
                    self.sink.emit(Change::Rescan {
                        path: self.root.clone(),
                    });
                    continue;
                }
                self.apply(&parse(&buf[..got]));
            }
            // SAFETY: the event created above.
            unsafe { CloseHandle(event) };
        }

        fn apply(&self, records: &[(u32, String)]) {
            let rules: Arc<Rules> = match self.rules.read() {
                Ok(r) => Arc::clone(&r),
                Err(p) => Arc::clone(&p.into_inner()),
            };
            for (path, act) in settle(&self.root, records) {
                let kind = match act {
                    Act::Gone => notify::EventKind::Remove(notify::event::RemoveKind::Any),
                    Act::Fresh => notify::EventKind::Create(notify::event::CreateKind::Any),
                    Act::Changed => notify::EventKind::Modify(notify::event::ModifyKind::Any),
                };
                let event = notify::Event::new(kind).add_path(std::path::PathBuf::from(path));
                // The look that follows is a `stat` of the share: as the owner,
                // which this thread is.
                translate(self.id, self.real_modes, &rules, &event, &self.sink);
            }
        }
    }

    /// What the engine holds: the watching threads stop within a second.
    struct Share {
        stopped: Arc<AtomicBool>,
        rules: SharedRules,
    }

    impl std::fmt::Debug for Share {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Share")
        }
    }

    impl WatchHandle for Share {
        fn retune(&self, opts: &ScanOptions) {
            let fresh = Arc::new(Rules::from_options(opts));
            match self.rules.write() {
                Ok(mut held) => *held = fresh,
                Err(poisoned) => *poisoned.into_inner() = fresh,
            }
        }

        fn stop(self: Box<Self>) {
            self.stopped.store(true, Ordering::Relaxed);
        }
    }

    impl Drop for Share {
        fn drop(&mut self) {
            self.stopped.store(true, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(next: u32, action: u32, name: &str) -> Vec<u8> {
        let units: Vec<u8> = name.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let mut r = Vec::new();
        r.extend(next.to_le_bytes());
        r.extend(action.to_le_bytes());
        r.extend((units.len() as u32).to_le_bytes());
        r.extend(units);
        while r.len() % 4 != 0 {
            r.push(0);
        }
        r
    }

    #[test]
    fn a_chain_of_notifications_reads_as_the_server_sends_it() {
        let a = record(0, 1, r"Dava\dilekçe.docx");
        let mut buf = record(0, 3, "notlar.txt");
        // The first record's `next` is its own length, padded.
        let first_len = buf.len() as u32;
        buf[0..4].copy_from_slice(&first_len.to_le_bytes());
        buf.extend(a);
        assert_eq!(
            parse(&buf),
            vec![(3, "notlar.txt".into()), (1, r"Dava\dilekçe.docx".into())]
        );
    }

    #[test]
    fn a_batch_settles_to_one_look_per_path_under_the_root() {
        let got = settle(
            "//sunucu/paylasim/HASAN",
            &[
                (1, r"Dava\a.docx".into()),
                (3, r"Dava\a.docx".into()),
                (4, "eski".into()),
                (5, "yeni".into()),
                (2, "b.txt".into()),
            ],
        );
        assert_eq!(
            got,
            vec![
                ("//sunucu/paylasim/HASAN/Dava/a.docx".into(), Act::Fresh),
                ("//sunucu/paylasim/HASAN/eski".into(), Act::Gone),
                ("//sunucu/paylasim/HASAN/yeni".into(), Act::Fresh),
                ("//sunucu/paylasim/HASAN/b.txt".into(), Act::Gone),
            ]
        );
    }
}
