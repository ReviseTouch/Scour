//! A disk that says it spins, asked again. A USB bridge reports whatever is
//! behind it as rotating: a stick and a class 4 card both did here. The
//! first 4 KiB of a file just dropped from the cache answers in 0.4–1 ms
//! from flash and in about 11 ms from a 5,400 rpm platter, so a few such
//! reads settle it, once a disk for the life of the process.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// Faster than this, the first read of a cold file came from flash.
#[cfg(target_os = "linux")]
const FLASH_MS: f64 = 3.0;
/// Files asked: the median of these decides.
#[cfg(target_os = "linux")]
const SAMPLE: usize = 5;

/// Readers a flash disk behind USB is worth: two beat one by 12–24 % on a
/// stick and a card, and four lost on the card.
pub(crate) const FLASH_READERS: usize = 2;

fn known() -> &'static Mutex<HashMap<PathBuf, bool>> {
    static KNOWN: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
    KNOWN.get_or_init(Default::default)
}

/// Whether the disk under `root` really spins, from `files` on it.
pub(crate) fn spins<'a>(root: &Path, files: impl Iterator<Item = &'a Path>) -> bool {
    if let Some(&answer) = known()
        .lock()
        .ok()
        .and_then(|k| k.get(root).copied())
        .as_ref()
    {
        return answer;
    }
    let answer = sys::spins(files);
    if let Ok(mut k) = known().lock() {
        k.insert(root.to_path_buf(), answer);
    }
    answer
}

#[cfg(target_os = "linux")]
mod sys {
    use std::os::fd::AsRawFd as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    use std::path::Path;
    use std::time::Instant;

    /// The median first read of a few cold files; spinning unless it is
    /// clearly flash, and spinning when nothing could be read.
    pub(super) fn spins<'a>(files: impl Iterator<Item = &'a Path>) -> bool {
        let mut times: Vec<f64> = files
            .filter_map(first_read_ms)
            .take(super::SAMPLE)
            .collect();
        if times.len() < 3 {
            return true;
        }
        times.sort_by(f64::total_cmp);
        times[times.len() / 2] >= super::FLASH_MS
    }

    fn first_read_ms(path: &Path) -> Option<f64> {
        let file = std::fs::File::options()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_NOATIME)
            .open(path)
            .or_else(|_| {
                std::fs::File::options()
                    .read(true)
                    .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
                    .open(path)
            })
            .ok()?;
        let meta = file.metadata().ok()?;
        if !meta.is_file() || meta.len() < 4096 {
            return None;
        }
        let fd = file.as_raw_fd();
        let mut buf = [0u8; 4096];
        // SAFETY: a valid descriptor and a buffer of the length given.
        unsafe {
            libc::posix_fadvise(fd, 0, 0, libc::POSIX_FADV_DONTNEED);
            let began = Instant::now();
            let n = libc::pread(fd, buf.as_mut_ptr().cast(), buf.len(), 0);
            (n == 4096).then(|| began.elapsed().as_secs_f64() * 1e3)
        }
    }
}

/// Elsewhere the system's word is taken.
#[cfg(not(target_os = "linux"))]
mod sys {
    pub(super) fn spins<'a>(_files: impl Iterator<Item = &'a std::path::Path>) -> bool {
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The temporary directory is on this machine's NVMe disk: asked, it
    /// answers like flash, whatever the system said of it.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_disk_that_answers_like_flash_does_not_spin() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<PathBuf> = (0..8)
            .map(|i| {
                let p = dir.path().join(format!("f{i}"));
                std::fs::write(&p, vec![b'a'; 8192]).unwrap();
                p
            })
            .collect();
        assert!(!spins(dir.path(), paths.iter().map(PathBuf::as_path)));
        // Files too small to time say nothing: the system's word stands.
        let small = tempfile::tempdir().unwrap();
        let tiny = small.path().join("t");
        std::fs::write(&tiny, b"x").unwrap();
        assert!(spins(small.path(), std::iter::repeat_n(tiny.as_path(), 8)));
    }

    #[test]
    fn an_answer_is_kept_for_its_root_and_nothing_readable_spins() {
        let root = Path::new("/nowhere-settle-test");
        assert!(spins(root, std::iter::empty()));
        // Asked again with files that would say otherwise: the kept answer stands.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a");
        std::fs::write(&path, vec![0u8; 8192]).unwrap();
        assert!(spins(root, std::iter::once(path.as_path())));
    }
}
