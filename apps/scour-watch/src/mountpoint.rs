//! An atomically created, private directory for the short-lived root mount.

use std::ffi::CString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

pub fn create(parent: &Path) -> std::io::Result<PathBuf> {
    let template = parent.join("scour-watch-XXXXXX");
    let mut bytes = CString::new(template.as_os_str().as_bytes())?.into_bytes_with_nul();
    if unsafe { libc::mkdtemp(bytes.as_mut_ptr().cast()) }.is_null() {
        return Err(std::io::Error::last_os_error());
    }
    bytes.pop();
    Ok(std::ffi::OsString::from_vec(bytes).into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn mountpoints_are_private_and_distinct() {
        let a = create(&std::env::temp_dir()).unwrap();
        let b = create(&std::env::temp_dir()).unwrap();
        assert_ne!(a, b);
        for path in [a, b] {
            let meta = std::fs::symlink_metadata(&path).unwrap();
            assert!(meta.is_dir());
            assert_eq!(meta.mode() & 0o777, 0o700);
            assert_eq!(meta.uid(), unsafe { libc::geteuid() });
            std::fs::remove_dir(path).unwrap();
        }
    }
}
