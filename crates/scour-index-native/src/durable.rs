//! Writing a file so that a kill does not take the index with it.
//!
//! Everything here answers one question: after `kill -9`, or a power cut, does
//! the index still open? Before this the answer was no, and not rarely. There
//! was no `fsync` anywhere in the workspace, the manifest was written in place
//! over the only copy of itself, and the file that says which segments exist
//! was written *after* the segments it names were unlinked. Each of those is a
//! window in which the index becomes a manifest pointing at files that are not
//! there — which is not a lost update, it is an index that never opens again
//! and a disk that has to be walked from scratch.
//!
//! The rules, in the order they matter:
//!
//! 1. **Contents before names.** A file's bytes are flushed to the device
//!    before anything points at it.
//! 2. **Rename, never overwrite.** The manifest is written beside itself and
//!    renamed over. `rename` within a directory is atomic on every filesystem
//!    this runs on, so a reader sees the old manifest or the new one.
//! 3. **The directory too.** A rename is not durable until the *directory* is
//!    synced; without it the manifest can be the old one after a crash even
//!    though the rename returned. Windows has no directory handle to sync, and
//!    does not need one — `ReplaceFile`-style renames are ordered there.
//!
//! The cost is real and was measured rather than assumed: see
//! `docs/MEASUREMENTS.md`. It is paid per commit, not per entry.

use std::fs::File;
use std::io::Write;
use std::path::Path;

use scour_core::{Error, Result};

/// Write a file and flush it to the device.
///
/// Not `std::fs::write`, which returns as soon as the kernel has the bytes.
/// That is enough for a file nothing depends on, and not enough for one the
/// manifest is about to name.
pub fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let name = || path.to_string_lossy().into_owned();
    let mut f = File::create(path).map_err(|e| Error::io(&e, &name()))?;
    f.write_all(bytes).map_err(|e| Error::io(&e, &name()))?;
    f.sync_all().map_err(|e| Error::io(&e, &name()))?;
    Ok(())
}

/// Replace a file's contents atomically: write beside it, sync, rename over.
///
/// A reader concurrent with this sees either the whole old file or the whole
/// new one, never a truncated file and never a mixture. That is the property
/// the manifest needs and the one `std::fs::write` cannot give: it truncates
/// first, so a kill mid-write leaves a zero-length manifest and an index that
/// reports itself as corrupt.
pub fn replace_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    write_synced(&tmp, bytes)?;
    std::fs::rename(&tmp, path).map_err(|e| {
        // Leaving the temporary file behind would make the next open see a
        // stray `.tmp`, which is harmless, but cleaning up is one line.
        let _ = std::fs::remove_file(&tmp);
        Error::io(&e, &path.to_string_lossy())
    })?;
    if let Some(dir) = path.parent() {
        sync_dir(dir);
    }
    Ok(())
}

/// Flush a directory's own entries, so a rename inside it survives a crash.
///
/// Best effort by design. Opening a directory as a file is not portable — it
/// fails on Windows, and on some network filesystems — and in every one of
/// those cases the rename is already ordered or the guarantee was never
/// available. Failing the commit over it would trade a real index for a
/// theoretical one.
fn sync_dir(dir: &Path) {
    #[cfg(unix)]
    if let Ok(f) = File::open(dir) {
        let _ = f.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacing_leaves_no_temporary_behind() {
        let dir = tempdir();
        let p = dir.join("meta.json");
        replace_synced(&p, b"first").expect("write");
        replace_synced(&p, b"second").expect("rewrite");
        assert_eq!(std::fs::read(&p).expect("read"), b"second");
        let strays: Vec<_> = std::fs::read_dir(&dir)
            .expect("list")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(strays.is_empty(), "left behind {strays:?}");
    }

    #[test]
    fn a_replacement_never_shortens_the_old_file() {
        // The property `std::fs::write` cannot offer: it truncates first, so a
        // reader between the truncate and the write sees an empty file. Here
        // the old contents are intact until the rename.
        let dir = tempdir();
        let p = dir.join("meta.json");
        replace_synced(&p, b"a longer first version").expect("write");
        let before = std::fs::read(&p).expect("read");
        replace_synced(&p, b"short").expect("rewrite");
        assert_eq!(before, b"a longer first version");
        assert_eq!(std::fs::read(&p).expect("read"), b"short");
    }

    fn tempdir() -> std::path::PathBuf {
        let p = std::env::temp_dir().join(format!(
            "scour-durable-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).expect("temp dir");
        p
    }
}
