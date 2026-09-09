//! Writing a file so that a kill does not take the index with it.
//!
//! Bytes are synced before anything names the file; the manifest is renamed
//! over, never overwritten; the directory is synced after the rename, because
//! a rename is not durable until it is. The cost is per commit, not per entry.

use std::fs::File;
use std::io::Write;
use std::path::Path;

use scour_core::{Error, Result};

/// Write a file and flush it to the device. Not `std::fs::write`, which returns
/// once the kernel has the bytes.
pub fn write_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let name = || path.to_string_lossy().into_owned();
    let mut f = File::create(path).map_err(|e| Error::io(&e, &name()))?;
    f.write_all(bytes).map_err(|e| Error::io(&e, &name()))?;
    f.sync_all().map_err(|e| Error::io(&e, &name()))?;
    Ok(())
}

/// Replace a file's contents atomically: write beside it, sync, rename over, so
/// a concurrent reader sees the whole old file or the whole new one.
pub fn replace_synced(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    write_synced(&tmp, bytes)?;
    std::fs::rename(&tmp, path).map_err(|e| {
        // A failed rename would otherwise leave the `.tmp` for the next open.
        let _ = std::fs::remove_file(&tmp);
        Error::io(&e, &path.to_string_lossy())
    })?;
    if let Some(dir) = path.parent() {
        sync_dir(dir);
    }
    Ok(())
}

/// Flush a directory's own entries, so a rename inside it survives a crash. Best
/// effort: it fails on Windows and some network filesystems, where the rename is
/// already ordered.
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
