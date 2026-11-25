//! What a cached text is found by, and what is kept of a document.

/// A document as it was when its text was taken. No digest of its contents:
/// a file whose size and time are the same is taken to be the same file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    pub path: String,
    pub size: u64,
    /// Nanoseconds since the epoch, as finely as the filesystem keeps them.
    pub mtime: i64,
}

const SECOND: i64 = 1_000_000_000;

impl Key {
    /// Whether `other` names the same file unchanged: the same time to the
    /// nanosecond. Only where both times are whole seconds — FAT keeps them
    /// in steps of two, some network filesystems in seconds — is a second
    /// either way the same time; on a finer filesystem an edit in the same
    /// second that keeps the size is still an edit.
    pub fn same_file(&self, other: &Key) -> bool {
        let coarse = self.mtime % SECOND == 0 && other.mtime % SECOND == 0;
        let near = (self.mtime - other.mtime).abs() <= if coarse { SECOND } else { 0 };
        self.path == other.path && self.size == other.size && near
    }
}

/// What a document gave when it was read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kept {
    Words(String),
    /// It opened, and holds no text.
    NoText,
    /// It would not open under this version of the readers; tried again
    /// only when the version changes.
    Failed {
        version: u32,
    },
}
