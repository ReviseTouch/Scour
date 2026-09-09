//! One writer per index directory.
//!
//! Segments are mmapped, so a second writer truncating a file under a live
//! mapping is undefined behaviour — `SIGBUS` mid-search. The advisory lock is
//! kernel-released if the process dies. Readers are not excluded.

use std::fs::File;
use std::path::Path;

use scour_core::{Error, Result};

const LOCK_FILE: &str = "index.lock";

/// An exclusive claim on one index directory, released when dropped or when
/// the process ends, whichever comes first.
#[derive(Debug)]
pub struct DirLock {
    // The lock is keyed to this open file description; dropping it releases.
    _file: File,
}

impl DirLock {
    /// Claim `dir`, or say who has it.
    pub fn acquire(dir: &Path) -> Result<DirLock> {
        let path = dir.join(LOCK_FILE);
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|e| Error::io(&e, &path.to_string_lossy()))?;
        match try_lock(&file) {
            Ok(true) => Ok(DirLock { _file: file }),
            Ok(false) => Err(Error::IndexBusy {
                detail: format!("another Scour process is writing {}", dir.to_string_lossy()),
            }),
            Err(e) => Err(Error::io(&e, &path.to_string_lossy())),
        }
    }
}

#[cfg(unix)]
fn try_lock(file: &File) -> std::io::Result<bool> {
    use std::os::fd::AsRawFd;
    // SAFETY: `flock` takes a file descriptor and an integer. The descriptor
    // is valid for as long as `file` is, and the call has no other effects.
    let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if rc == 0 {
        return Ok(true);
    }
    let e = std::io::Error::last_os_error();
    // EWOULDBLOCK is the answer "someone else has it", not a failure.
    match e.raw_os_error() {
        Some(code) if code == libc::EWOULDBLOCK || code == libc::EAGAIN => Ok(false),
        _ => Err(e),
    }
}

#[cfg(windows)]
fn try_lock(file: &File) -> std::io::Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::Storage::FileSystem::{
        LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx,
    };
    use windows_sys::Win32::System::IO::OVERLAPPED;

    let mut overlapped: OVERLAPPED = unsafe { std::mem::zeroed() };
    // SAFETY: a valid handle, a zeroed OVERLAPPED, and a byte range of one —
    // the range only has to be agreed on between the processes involved.
    let ok = unsafe {
        LockFileEx(
            file.as_raw_handle() as HANDLE,
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut overlapped,
        )
    };
    if ok != 0 {
        return Ok(true);
    }
    let e = std::io::Error::last_os_error();
    // ERROR_LOCK_VIOLATION: held by someone else.
    match e.raw_os_error() {
        Some(33) => Ok(false),
        _ => Err(e),
    }
}

#[cfg(not(any(unix, windows)))]
fn try_lock(_file: &File) -> std::io::Result<bool> {
    // Nothing to lock with; refusing to run would be worse than running open.
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tempdir(tag: &str) -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!("scour-lock-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("temp dir");
        p
    }

    #[test]
    fn the_second_claim_on_a_directory_is_refused() {
        let dir = tempdir("busy");
        let first = DirLock::acquire(&dir).expect("first");
        let err = DirLock::acquire(&dir).expect_err("second must be refused");
        assert_eq!(err.code(), "index_busy");
        drop(first);
    }

    #[test]
    fn releasing_lets_the_next_one_in() {
        let dir = tempdir("release");
        drop(DirLock::acquire(&dir).expect("first"));
        DirLock::acquire(&dir).expect("second, after the first let go");
    }

    #[test]
    fn two_directories_do_not_contend() {
        let a = tempdir("a");
        let b = tempdir("b");
        let _one = DirLock::acquire(&a).expect("a");
        let _two = DirLock::acquire(&b).expect("b");
    }
}
