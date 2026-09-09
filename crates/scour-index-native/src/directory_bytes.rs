//! The summed file lengths of an index directory, without walking it for every status call.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use parking_lot::Mutex;

/// A cached logical byte count for one index directory.
///
/// Counts partial and orphan files too, which is what `IndexStats::bytes_on_disk`
/// means. Every index-controlled mutation marks the value dirty; a quiet reader
/// does one directory metadata check and no per-file ones.
#[derive(Debug)]
pub(crate) struct DirectoryBytes {
    dir: PathBuf,
    state: Mutex<State>,
}

#[derive(Debug)]
struct State {
    bytes: u64,
    modified: Option<SystemTime>,
    active_writers: usize,
    dirty: bool,
    #[cfg(test)]
    measurements: usize,
}

impl DirectoryBytes {
    pub(crate) fn new(dir: &Path) -> DirectoryBytes {
        let (bytes, modified) = measure(dir);
        DirectoryBytes {
            dir: dir.to_owned(),
            state: Mutex::new(State {
                bytes,
                modified,
                active_writers: 0,
                dirty: false,
                #[cfg(test)]
                measurements: 1,
            }),
        }
    }

    /// The file lengths present now, including files no manifest names.
    pub(crate) fn get(&self) -> u64 {
        let mut state = self.state.lock();
        if state.active_writers != 0 {
            // Do not cache a half-written segment; the next quiet read is exact.
            drop(state);
            return dir_size(&self.dir);
        }

        let modified = directory_modified(&self.dir);
        if !state.dirty && modified.is_some() && modified == state.modified {
            return state.bytes;
        }

        let (bytes, modified) = measure(&self.dir);
        state.bytes = bytes;
        state.modified = modified;
        state.dirty = false;
        #[cfg(test)]
        {
            state.measurements += 1;
        }
        bytes
    }

    /// Run one filesystem mutation and invalidate the quiet cached value. The
    /// guard releases the writer count on unwinding as well as on return.
    pub(crate) fn changing<T>(&self, change: impl FnOnce() -> T) -> T {
        {
            let mut state = self.state.lock();
            state.active_writers += 1;
            state.dirty = true;
        }
        let _writer = Writer { bytes: self };
        change()
    }
}

struct Writer<'a> {
    bytes: &'a DirectoryBytes,
}

impl Drop for Writer<'_> {
    fn drop(&mut self) {
        let mut state = self.bytes.state.lock();
        state.active_writers -= 1;
        // `dirty` stays set: refreshing here would walk the directory on every
        // commit even when nobody asks for status.
    }
}

/// Measure the directory and retain the stamp from after the walk, so that an
/// external create or unlink invalidates the value on the next call.
fn measure(dir: &Path) -> (u64, Option<SystemTime>) {
    (dir_size(dir), directory_modified(dir))
}

fn directory_modified(dir: &Path) -> Option<SystemTime> {
    std::fs::metadata(dir).and_then(|m| m.modified()).ok()
}

pub(crate) fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A measurement of walk against cache, not a timing assertion.
    #[test]
    #[ignore = "manual performance measurement"]
    fn cached_directory_byte_cost() {
        let dir = tempfile::tempdir().expect("temporary directory");
        for n in 0..220 {
            std::fs::write(dir.path().join(format!("part-{n}")), b"x").expect("part");
        }
        let bytes = DirectoryBytes::new(dir.path());
        assert_eq!(bytes.get(), 220);

        let began = std::time::Instant::now();
        for _ in 0..2_000 {
            std::hint::black_box(dir_size(dir.path()));
        }
        let walked = began.elapsed();
        let began = std::time::Instant::now();
        for _ in 0..2_000 {
            std::hint::black_box(bytes.get());
        }
        let cached = began.elapsed();
        eprintln!("2,000 reads: walked={walked:?} cached={cached:?}");
    }

    #[test]
    fn quiet_reads_reuse_one_directory_measurement() {
        let dir = tempfile::tempdir().expect("temporary directory");
        std::fs::write(dir.path().join("first"), b"one").expect("first file");
        let bytes = DirectoryBytes::new(dir.path());

        assert_eq!(bytes.get(), 3);
        assert_eq!(bytes.get(), 3);
        assert_eq!(bytes.state.lock().measurements, 1);
    }

    #[test]
    fn a_completed_change_is_measured_once_and_then_cached() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let bytes = DirectoryBytes::new(dir.path());

        bytes.changing(|| std::fs::write(dir.path().join("segment"), b"seven77").expect("segment"));
        assert_eq!(bytes.get(), 7);
        assert_eq!(bytes.get(), 7);
        assert_eq!(bytes.state.lock().measurements, 2);

        bytes.changing(|| std::fs::remove_file(dir.path().join("segment")).expect("erase"));
        assert_eq!(bytes.get(), 0);
        assert_eq!(bytes.state.lock().measurements, 3);
    }

    #[test]
    fn an_in_flight_and_a_failed_write_both_count_by_file_length() {
        let dir = tempfile::tempdir().expect("temporary directory");
        let bytes = DirectoryBytes::new(dir.path());

        let failed: std::io::Result<()> = bytes.changing(|| {
            std::fs::write(dir.path().join("orphan"), b"partial")?;
            assert_eq!(bytes.get(), 7, "an in-flight file is visible");
            Err(std::io::Error::other("publication failed"))
        });
        assert!(failed.is_err());
        assert_eq!(bytes.get(), 7, "the orphan remains part of disk usage");
        assert_eq!(bytes.state.lock().measurements, 2);
    }
}
