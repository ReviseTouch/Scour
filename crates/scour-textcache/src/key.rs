//! What a cached text is found by, and what is kept of a document.

/// A document as it was when its text was taken. No digest of its contents:
/// a file whose size and time are the same is taken to be the same file.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Key {
    pub path: String,
    pub size: u64,
    /// Seconds since the epoch.
    pub mtime: i64,
}

impl Key {
    /// Whether `other` names the same file unchanged. FAT and exFAT keep
    /// times in steps of two seconds, so a second either way is the same time.
    pub fn same_file(&self, other: &Key) -> bool {
        self.path == other.path && self.size == other.size && (self.mtime - other.mtime).abs() <= 1
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
