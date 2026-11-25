//! Documents whose words need a reader this crate does not have — PDF, the
//! legacy Office formats, e-mail — are handed to whoever the caller says.
//! It may keep their text: a document found kept is searched without being
//! opened, once its size and time say it is unchanged and its reader may
//! read it.

use std::path::Path;

/// A document as the reader saw it.
#[derive(Debug, Clone, Copy)]
pub struct Doc<'a> {
    pub path: &'a Path,
    pub size: u64,
    /// Nanoseconds since the epoch.
    pub mtime: i64,
    /// Lower case, without the dot; what tells `.doc` from `.xls`.
    pub extension: &'a str,
}

/// What a document gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocText {
    Words(String),
    /// It opened, and holds no text: a scan.
    NoText,
    /// Its reader could not open it.
    Broken,
}

/// The caller's documents: what is kept of them, and a reader for the rest.
pub trait Texts: Send + Sync {
    /// What is kept for this document, unchanged; None to read it.
    fn kept(&self, doc: &Doc<'_>) -> Option<DocText>;
    /// Its words, from its bytes. May keep them for next time.
    fn read(&self, doc: &Doc<'_>, bytes: &[u8]) -> DocText;
}

/// Extensions of the documents handed over. The bytes decide in the end:
/// an OLE2 file named `.doc` is still one, a `.pdf` that is text is text.
pub(crate) fn handed_over(extension: &str) -> bool {
    matches!(
        extension,
        "pdf" | "doc" | "dot" | "xls" | "xlt" | "ppt" | "pps" | "pot" | "eml"
    )
}

pub(crate) fn extension_of(path: &Path) -> String {
    path.extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// Size and time of the file at `path` without opening it, and whether the
/// reading thread may read it. None for a link, a directory, or no access.
pub(crate) fn unopened(path: &Path) -> Option<(u64, i64)> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || !may_read(path) {
        return None;
    }
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos() as i64);
    Some((meta.len(), mtime))
}

/// Asked of the system as the one reading, so a kept text is never shown to
/// somebody who could not have read the file.
#[cfg(unix)]
fn may_read(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    let Ok(c) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: a NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), libc::R_OK) == 0 }
}

/// Opened for its data and closed unread: the thread's own identity is what
/// is asked, the person's when the service reads as them.
#[cfg(windows)]
fn may_read(path: &Path) -> bool {
    std::fs::File::open(path).is_ok()
}

#[cfg(not(any(unix, windows)))]
fn may_read(_path: &Path) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_document_kinds_are_handed_over() {
        assert!(handed_over("pdf") && handed_over("xls") && handed_over("eml"));
        assert!(!handed_over("docx") && !handed_over("txt"));
        assert_eq!(extension_of(Path::new("/a/B.PDF")), "pdf");
    }

    #[cfg(unix)]
    #[test]
    fn a_file_its_reader_may_not_read_is_not_answered() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.pdf");
        std::fs::write(&path, b"%PDF").unwrap();
        assert_eq!(unopened(&path).map(|(s, _)| s), Some(4));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads anything; the check is the system's, not ours.
        let root = unsafe { libc::geteuid() } == 0;
        assert_eq!(unopened(&path).is_some(), root);
        let link = dir.path().join("l.pdf");
        std::os::unix::fs::symlink(&path, &link).unwrap();
        assert_eq!(unopened(&link), None);
    }
}
