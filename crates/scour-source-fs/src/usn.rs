//! Watching an NTFS volume through its change journal. Every change on the
//! volume is a record in `$UsnJrnl` whatever its size, so nothing overflows:
//! `ReadDirectoryChangesW` lost track under load and every loss was a walk of
//! the whole of C: (PC2, 17 November). Reading the journal needs the volume
//! opened, which needs an administrator — the service has one; a session
//! process falls back to the ordinary watcher.
//!
//! A record names a file by its reference number and its parent's; the
//! parent's path is asked of the volume (`OpenFileById` +
//! `GetFinalPathNameByHandleW`) and kept. What a record says is, as with every
//! watcher here, only a hint to look again — see [`crate::watch`].

/// What one journal record says, decoded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Record {
    pub parent: u64,
    pub reason: u32,
    pub dir: bool,
    pub name: String,
}

const FILE_CREATE: u32 = 0x0000_0100;
const FILE_DELETE: u32 = 0x0000_0200;
const RENAME_OLD_NAME: u32 = 0x0000_1000;
const RENAME_NEW_NAME: u32 = 0x0000_2000;
const ATTRIBUTE_DIRECTORY: u32 = 0x10;

/// A buffer `FSCTL_READ_USN_JOURNAL` filled: the next USN to ask from, then
/// version 2 records. A record of another version is stepped over.
pub(crate) fn parse(buf: &[u8]) -> Option<(i64, Vec<Record>)> {
    let next = i64::from_le_bytes(buf.get(..8)?.try_into().ok()?);
    let mut out = Vec::new();
    let mut at = 8usize;
    while at + 60 <= buf.len() {
        let rec = &buf[at..];
        let len = u32::from_le_bytes(rec[0..4].try_into().ok()?) as usize;
        if len < 60 || at + len > buf.len() {
            break;
        }
        let major = u16::from_le_bytes(rec[4..6].try_into().ok()?);
        if major == 2 {
            let parent = u64::from_le_bytes(rec[16..24].try_into().ok()?);
            let reason = u32::from_le_bytes(rec[40..44].try_into().ok()?);
            let attrs = u32::from_le_bytes(rec[52..56].try_into().ok()?);
            let name_len = u16::from_le_bytes(rec[56..58].try_into().ok()?) as usize;
            let name_at = u16::from_le_bytes(rec[58..60].try_into().ok()?) as usize;
            if name_at + name_len <= len {
                let units: Vec<u16> = rec[name_at..name_at + name_len]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_le_bytes(*c))
                    .collect();
                out.push(Record {
                    parent,
                    reason,
                    dir: attrs & ATTRIBUTE_DIRECTORY != 0,
                    name: String::from_utf16_lossy(&units),
                });
            }
        }
        // Records are 8-byte aligned; `RecordLength` already includes the padding.
        at += len;
    }
    Some((next, out))
}

/// What to do about a path once a batch of records is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Act {
    /// Removed, or renamed away from here.
    Gone,
    /// Created, or renamed to here: a directory is walked.
    Fresh,
    /// Written, or its attributes changed.
    Changed,
}

/// A batch of records as one action per path, in the order the paths first
/// appeared: a file written a thousand times in a batch is looked at once.
/// `resolve` gives a directory's path from its reference number; `forget` is
/// called when a directory moves or goes, since the paths kept are then stale.
pub(crate) fn settle(
    records: &[Record],
    mut resolve: impl FnMut(u64) -> Option<String>,
    mut forget: impl FnMut(),
) -> Vec<(String, Act)> {
    let mut out: Vec<(String, Act)> = Vec::new();
    let mut at: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for r in records {
        let Some(parent) = resolve(r.parent) else {
            continue;
        };
        let path = if parent.ends_with('/') {
            format!("{parent}{}", r.name)
        } else {
            format!("{parent}/{}", r.name)
        };
        let act = if r.reason & (FILE_DELETE | RENAME_OLD_NAME) != 0 {
            Act::Gone
        } else if r.reason & (FILE_CREATE | RENAME_NEW_NAME) != 0 {
            Act::Fresh
        } else {
            Act::Changed
        };
        match at.get(&path) {
            Some(&i) => out[i].1 = merge(out[i].1, act),
            None => {
                at.insert(path.clone(), out.len());
                out.push((path, act));
            }
        }
        if r.dir && r.reason & (FILE_DELETE | RENAME_OLD_NAME | RENAME_NEW_NAME) != 0 {
            forget();
        }
    }
    out
}

/// Two things said about one path in one batch, as one: the last removal
/// wins, a creation after it makes it fresh, a write keeps what was said.
pub(crate) fn merge(was: Act, act: Act) -> Act {
    match (was, act) {
        (_, Act::Gone) => Act::Gone,
        (_, Act::Fresh) | (Act::Gone, Act::Changed) => Act::Fresh,
        (was, Act::Changed) => was,
    }
}

/// Is `path` under one of `roots`? Both lower-cased: NTFS does not tell `C:/A`
/// from `c:/a`, and the volume and the configuration may spell them apart.
pub(crate) fn under(path: &str, roots: &[String]) -> bool {
    let p = path.to_lowercase();
    roots.iter().any(|r| {
        let r = r.trim_end_matches('/');
        p == r || (p.starts_with(r) && p[r.len()..].starts_with('/'))
    })
}

/// How old a journal position must be before it is written down: longer than
/// the index takes to keep what it was told (`persist_every`, thirty seconds).
const KEEP_LAG: std::time::Duration = std::time::Duration::from_secs(45);

/// `journal-id usn`, as [`write_kept`] leaves it.
fn read_kept(path: &std::path::Path) -> Option<(u64, i64)> {
    let text = std::fs::read_to_string(path).ok()?;
    let mut it = text.split_whitespace();
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

/// Replaced whole, through a rename: a half-written position is no position.
fn write_kept(path: &std::path::Path, journal: u64, usn: i64) {
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, format!("{journal} {usn}\n")).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// The longest a journal read on from the last run may stand in for a walk.
/// The journal records only what Windows writes: another system writing the
/// volume — Linux's ntfs3 on a machine that boots both — leaves no record, and
/// neither does a change whose parent the reader could not place.
const WALK_AT_LEAST: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);

/// Whether the source is owed a start walk whatever the journal says: no walk
/// noted within [`WALK_AT_LEAST`], by the clock on the wall — an uptime clock
/// starts again with every boot, and a machine shut down daily never got one.
fn walk_due(stamp: &std::path::Path) -> bool {
    std::fs::metadata(stamp)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_none_or(|age| age >= WALK_AT_LEAST)
}

/// Noted as the start walk is queued. A walk cut short by a shutdown still
/// counts, which bounds the cost to a day rather than closing the gap.
fn walked_now(stamp: &std::path::Path) {
    let _ = std::fs::write(stamp, b"");
}

#[cfg(windows)]
pub(crate) use imp::try_start;

#[cfg(windows)]
mod imp {
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use scour_core::{Change, ChangeSink, ScanOptions, WatchHandle};
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_ID_DESCRIPTOR, FILE_ID_DESCRIPTOR_0,
        FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FileIdType,
        GetFinalPathNameByHandleW, GetVolumeInformationW, OPEN_EXISTING, OpenFileById,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;
    use windows_sys::Win32::System::Ioctl::{
        FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_USN_JOURNAL, READ_USN_JOURNAL_DATA_V0,
        USN_JOURNAL_DATA_V0,
    };

    use super::{Act, parse, settle, under};
    use crate::rules::Rules;
    use crate::scan::FsSource;
    use crate::watch::{SharedRules, translate};

    const GENERIC_READ: u32 = 0x8000_0000;
    const FILE_READ_ATTRIBUTES: u32 = 0x80;
    /// A directory paths kept beyond this are dropped and asked again.
    const KEPT: usize = 100_000;
    /// How long the journal is left before it is read again, when it was empty.
    const REST: Duration = Duration::from_millis(500);

    /// A raw handle as an integer, so the reading thread may own it.
    struct Volume(usize);

    impl Volume {
        fn h(&self) -> HANDLE {
            self.0 as HANDLE
        }
    }

    impl Drop for Volume {
        fn drop(&mut self) {
            // SAFETY: a handle this owns, closed once.
            unsafe { CloseHandle(self.h()) };
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Journal-watch `source` when every root is on one NTFS volume this process
    /// may open. None sends the caller to `ReadDirectoryChangesW`.
    pub(crate) fn try_start(
        source: &FsSource,
        rules: SharedRules,
        sink: Arc<dyn ChangeSink>,
    ) -> Option<Box<dyn WatchHandle>> {
        let roots: Vec<String> = source
            .roots()
            .iter()
            .map(|r| crate::path::from_path(r))
            .collect();
        let letter = roots.first()?.get(..2)?.to_ascii_uppercase();
        if !letter.ends_with(':')
            || roots
                .iter()
                .any(|r| r.get(..2).map(str::to_ascii_uppercase).as_deref() != Some(&letter))
        {
            return None;
        }
        if !is_ntfs(&letter) {
            return None;
        }
        let path = wide(&format!(r"\\.\{letter}"));
        // SAFETY: a null-terminated name; the handle is owned by `Volume`.
        let h = unsafe {
            CreateFileW(
                path.as_ptr(),
                GENERIC_READ,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if h == INVALID_HANDLE_VALUE {
            // The ordinary answer for a process without an administrator.
            return None;
        }
        let volume = Volume(h as usize);
        let journal = query(&volume)?;
        let kept = source
            .journal_dir()
            .map(|d| d.join(format!("journal-{}", source.source_id().0)));
        let saved = kept.as_deref().and_then(super::read_kept);
        // Read on from where the last run stopped, if the journal still holds
        // that far back; otherwise from now, and the start walk covers the gap.
        let (next, caught_up) = match saved {
            Some((id, usn))
                if id == journal.UsnJournalID
                    && usn >= journal.FirstUsn
                    && usn <= journal.NextUsn =>
            {
                (usn, true)
            }
            _ => (journal.NextUsn, false),
        };
        let stamp = source
            .journal_dir()
            .map(|d| d.join(format!("walked-{}", source.source_id().0)));
        let caught_up = caught_up && stamp.as_deref().is_some_and(|s| !super::walk_due(s));
        if !caught_up && let Some(s) = &stamp {
            super::walked_now(s);
        }
        let name = scour_core::Source::describe(source).name;
        scour_core::note!(
            "scourd: {name} is watched through the NTFS change journal of {letter}{}",
            if caught_up {
                ", read on from the last run"
            } else {
                ""
            }
        );
        let stopped = Arc::new(AtomicBool::new(false));
        let reader = Reader {
            volume,
            journal: journal.UsnJournalID,
            next,
            kept,
            pending: (std::time::Instant::now(), journal.UsnJournalID, next),
            roots: roots.iter().map(|r| r.to_lowercase()).collect(),
            id: source.source_id(),
            real_modes: source.real_modes(),
            rules: Arc::clone(&rules),
            sink,
            stopped: Arc::clone(&stopped),
            dirs: HashMap::new(),
        };
        std::thread::Builder::new()
            .name(format!("usn {letter}"))
            .spawn(move || reader.run())
            .ok()?;
        Some(Box::new(Journal {
            stopped,
            rules,
            caught_up,
        }))
    }

    fn is_ntfs(letter: &str) -> bool {
        let root = wide(&format!("{letter}\\"));
        let mut fs = [0u16; 32];
        // SAFETY: a null-terminated root and a buffer of the size given.
        let ok = unsafe {
            GetVolumeInformationW(
                root.as_ptr(),
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                fs.as_mut_ptr(),
                fs.len() as u32,
            )
        };
        let n = fs.iter().position(|&c| c == 0).unwrap_or(0);
        ok != 0 && String::from_utf16_lossy(&fs[..n]) == "NTFS"
    }

    fn query(volume: &Volume) -> Option<USN_JOURNAL_DATA_V0> {
        let mut data = USN_JOURNAL_DATA_V0::default();
        let mut got = 0u32;
        // SAFETY: an output buffer of the size given.
        let ok = unsafe {
            DeviceIoControl(
                volume.h(),
                FSCTL_QUERY_USN_JOURNAL,
                std::ptr::null(),
                0,
                &mut data as *mut _ as *mut _,
                std::mem::size_of::<USN_JOURNAL_DATA_V0>() as u32,
                &mut got,
                std::ptr::null_mut(),
            )
        };
        (ok != 0).then_some(data)
    }

    struct Reader {
        volume: Volume,
        journal: u64,
        next: i64,
        roots: Vec<String>,
        id: scour_core::SourceId,
        real_modes: bool,
        rules: SharedRules,
        sink: Arc<dyn ChangeSink>,
        stopped: Arc<AtomicBool>,
        /// Directory paths by reference number, as the volume gave them.
        dirs: HashMap<u64, String>,
        /// Where the position is written down between runs.
        kept: Option<std::path::PathBuf>,
        /// The position read up to, and when: written once it is [`KEEP_LAG`]
        /// old, so what is on disk is never ahead of what the index has kept.
        pending: (std::time::Instant, u64, i64),
    }

    impl Reader {
        fn run(mut self) {
            let mut buf = vec![0u8; 64 * 1024];
            while !self.stopped.load(Ordering::Relaxed) {
                let ask = READ_USN_JOURNAL_DATA_V0 {
                    StartUsn: self.next,
                    ReasonMask: u32::MAX,
                    ReturnOnlyOnClose: 0,
                    Timeout: 0,
                    BytesToWaitFor: 0,
                    UsnJournalID: self.journal,
                };
                let mut got = 0u32;
                // SAFETY: input and output buffers of the sizes given.
                let ok = unsafe {
                    DeviceIoControl(
                        self.volume.h(),
                        FSCTL_READ_USN_JOURNAL,
                        &ask as *const _ as *const _,
                        std::mem::size_of::<READ_USN_JOURNAL_DATA_V0>() as u32,
                        buf.as_mut_ptr() as *mut _,
                        buf.len() as u32,
                        &mut got,
                        std::ptr::null_mut(),
                    )
                };
                if ok == 0 {
                    // SAFETY: reads this thread's last error.
                    let code = unsafe { GetLastError() };
                    self.lost(code);
                    continue;
                }
                let Some((next, records)) = parse(&buf[..got as usize]) else {
                    std::thread::sleep(REST);
                    continue;
                };
                let empty = next == self.next || records.is_empty();
                self.next = next;
                if !records.is_empty() {
                    self.apply(&records);
                }
                self.keep();
                if empty {
                    std::thread::sleep(REST);
                }
            }
        }

        /// Write down a position read at least [`KEEP_LAG`] ago. The index keeps
        /// what it was told every thirty seconds; a position newer than that
        /// would skip, after a crash, changes the index never wrote. Reading a
        /// change twice costs a look.
        fn keep(&mut self) {
            if self.pending.0.elapsed() < super::KEEP_LAG {
                return;
            }
            if let Some(path) = &self.kept {
                super::write_kept(path, self.pending.1, self.pending.2);
            }
            self.pending = (std::time::Instant::now(), self.journal, self.next);
        }

        /// The journal went from under the reader: it wrapped past the USN asked
        /// for, or was deleted and made again. What happened between is unknown,
        /// so the roots are walked, and reading resumes from the journal's end.
        fn lost(&mut self, code: u32) {
            scour_core::note!(
                "scourd: the change journal could not be read (error {code}); walking again"
            );
            std::thread::sleep(Duration::from_secs(5));
            if let Some(j) = query(&self.volume) {
                self.journal = j.UsnJournalID;
                self.next = j.NextUsn;
                self.dirs.clear();
                self.pending = (std::time::Instant::now(), self.journal, self.next);
                for r in &self.roots {
                    self.sink.emit(Change::Rescan { path: r.clone() });
                }
            }
        }

        fn apply(&mut self, records: &[super::Record]) {
            let volume = self.volume.h();
            let dirs = std::cell::RefCell::new(std::mem::take(&mut self.dirs));
            let settled = settle(
                records,
                |frn| {
                    if let Some(p) = dirs.borrow().get(&frn) {
                        return Some(p.clone());
                    }
                    let p = path_of(volume, frn)?;
                    let mut held = dirs.borrow_mut();
                    if held.len() >= KEPT {
                        held.clear();
                    }
                    held.insert(frn, p.clone());
                    Some(p)
                },
                || dirs.borrow_mut().clear(),
            );
            self.dirs = dirs.into_inner();
            let rules: Arc<Rules> = match self.rules.read() {
                Ok(r) => Arc::clone(&r),
                Err(p) => Arc::clone(&p.into_inner()),
            };
            for (path, act) in settled {
                if !under(&path, &self.roots) {
                    continue;
                }
                let kind = match act {
                    Act::Gone => notify::EventKind::Remove(notify::event::RemoveKind::Any),
                    Act::Fresh => notify::EventKind::Create(notify::event::CreateKind::Any),
                    Act::Changed => notify::EventKind::Modify(notify::event::ModifyKind::Any),
                };
                let event = notify::Event::new(kind).add_path(std::path::PathBuf::from(path));
                translate(self.id, self.real_modes, &rules, &event, &self.sink);
            }
        }
    }

    /// A directory's path, from its reference number: `C:/Users/x`.
    fn path_of(volume: HANDLE, frn: u64) -> Option<String> {
        let desc = FILE_ID_DESCRIPTOR {
            dwSize: std::mem::size_of::<FILE_ID_DESCRIPTOR>() as u32,
            Type: FileIdType,
            Anonymous: FILE_ID_DESCRIPTOR_0 { FileId: frn as i64 },
        };
        // SAFETY: a descriptor that lives for the call; the handle is closed below.
        let h = unsafe {
            OpenFileById(
                volume,
                &desc,
                FILE_READ_ATTRIBUTES,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                std::ptr::null(),
                FILE_FLAG_BACKUP_SEMANTICS,
            )
        };
        if h == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut buf = vec![0u16; 1024];
        let mut n;
        loop {
            // SAFETY: a buffer of the length given.
            n = unsafe { GetFinalPathNameByHandleW(h, buf.as_mut_ptr(), buf.len() as u32, 0) }
                as usize;
            if n < buf.len() || n == 0 {
                break;
            }
            buf.resize(n + 1, 0);
        }
        // SAFETY: the handle opened above.
        unsafe { CloseHandle(h) };
        if n == 0 {
            return None;
        }
        let text = String::from_utf16_lossy(&buf[..n]);
        let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
        Some(crate::path::normalise(text))
    }

    /// What the engine holds: the reading thread stops at its next read.
    struct Journal {
        stopped: Arc<AtomicBool>,
        rules: SharedRules,
        caught_up: bool,
    }

    impl std::fmt::Debug for Journal {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Journal")
        }
    }

    impl WatchHandle for Journal {
        fn unwatched(&self) -> Vec<String> {
            Vec::new()
        }

        /// The journal covers the whole volume already.
        fn cover(&self, _path: &str) {}

        fn caught_up(&self) -> bool {
            self.caught_up
        }

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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_journal_stands_in_for_a_walk_for_a_day_at_most() {
        let dir = std::env::temp_dir().join(format!("scour-usn-walk-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let stamp = dir.join("walked-1");
        assert!(walk_due(&stamp), "never walked is due");
        walked_now(&stamp);
        assert!(!walk_due(&stamp), "just walked is not");
        let old = std::time::SystemTime::now() - WALK_AT_LEAST - std::time::Duration::from_secs(60);
        std::fs::File::options()
            .write(true)
            .open(&stamp)
            .unwrap()
            .set_modified(old)
            .unwrap();
        assert!(walk_due(&stamp), "a day old is due");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A version-2 record as the volume writes it.
    fn record(parent: u64, reason: u32, attrs: u32, name: &str) -> Vec<u8> {
        let units: Vec<u8> = name.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let len = (60 + units.len()).div_ceil(8) * 8;
        let mut r = vec![0u8; len];
        r[0..4].copy_from_slice(&(len as u32).to_le_bytes());
        r[4..6].copy_from_slice(&2u16.to_le_bytes());
        r[8..16].copy_from_slice(&99u64.to_le_bytes());
        r[16..24].copy_from_slice(&parent.to_le_bytes());
        r[40..44].copy_from_slice(&reason.to_le_bytes());
        r[52..56].copy_from_slice(&attrs.to_le_bytes());
        r[56..58].copy_from_slice(&(units.len() as u16).to_le_bytes());
        r[58..60].copy_from_slice(&60u16.to_le_bytes());
        r[60..60 + units.len()].copy_from_slice(&units);
        r
    }

    #[test]
    fn records_are_read_as_the_volume_writes_them() {
        let mut buf = 1234i64.to_le_bytes().to_vec();
        buf.extend(record(5, FILE_CREATE, 0x20, "rapor.pdf"));
        buf.extend(record(
            7,
            RENAME_NEW_NAME,
            ATTRIBUTE_DIRECTORY,
            "Yeni klasör",
        ));
        let (next, recs) = parse(&buf).expect("a buffer");
        assert_eq!(next, 1234);
        assert_eq!(
            recs,
            vec![
                Record {
                    parent: 5,
                    reason: FILE_CREATE,
                    dir: false,
                    name: "rapor.pdf".into()
                },
                Record {
                    parent: 7,
                    reason: RENAME_NEW_NAME,
                    dir: true,
                    name: "Yeni klasör".into()
                },
            ]
        );
        // A truncated tail is not a record.
        let cut = &buf[..buf.len() - 4];
        assert_eq!(parse(cut).expect("a buffer").1.len(), 1);
    }

    #[test]
    fn a_batch_is_one_look_per_path() {
        let r = |reason, dir, name: &str| Record {
            parent: 5,
            reason,
            dir,
            name: name.into(),
        };
        let recs = vec![
            r(FILE_CREATE, false, "a.txt"),
            r(0x2, false, "a.txt"), // data extend
            r(0x2, false, "b.txt"),
            r(FILE_DELETE, false, "b.txt"),
            r(FILE_DELETE, false, "c.txt"),
            r(FILE_CREATE, false, "c.txt"),
            r(RENAME_OLD_NAME, true, "eski"),
            r(RENAME_NEW_NAME, true, "yeni"),
        ];
        let mut forgot = 0;
        let out = settle(&recs, |_| Some("C:/".into()), || forgot += 1);
        assert_eq!(
            out,
            vec![
                ("C:/a.txt".into(), Act::Fresh),
                ("C:/b.txt".into(), Act::Gone),
                ("C:/c.txt".into(), Act::Fresh),
                ("C:/eski".into(), Act::Gone),
                ("C:/yeni".into(), Act::Fresh),
            ]
        );
        assert_eq!(
            forgot, 2,
            "a directory that moved makes the kept paths stale"
        );
        // A parent the volume no longer has: nothing to say about it.
        assert!(settle(&recs, |_| None, || {}).is_empty());
    }

    #[test]
    fn a_position_is_kept_whole() {
        let dir = tempfile::tempdir().expect("tmpdir");
        let path = dir.path().join("journal-3");
        assert_eq!(read_kept(&path), None);
        write_kept(&path, 133_000_000_000, 98_765_432);
        assert_eq!(read_kept(&path), Some((133_000_000_000, 98_765_432)));
        std::fs::write(&path, "garbage").expect("write");
        assert_eq!(read_kept(&path), None);
    }

    #[test]
    fn only_paths_under_a_root_count() {
        let roots = vec!["c:/users/h".to_owned()];
        assert!(under("C:/Users/H/belge.txt", &roots));
        assert!(under("C:/Users/H", &roots));
        assert!(!under("C:/Users/Hasan/belge.txt", &roots));
        assert!(under("D:/x", &["d:/".to_owned()]));
    }
}
