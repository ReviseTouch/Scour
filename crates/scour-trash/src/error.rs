//! Why a file could not be put in the trash.

use std::path::PathBuf;

/// Why a file could not be sent to the trash.
#[derive(Debug)]
pub enum Error {
    /// There is no trash to move it to on this platform.
    Unsupported,
    /// There is nowhere to put it: no writable trash for this filesystem.
    NoTrash(PathBuf),
    /// The path does not exist, or cannot be read.
    Missing(PathBuf),
    /// A path with no file name — `/`, or one ending in `..`.
    Unnamed(PathBuf),
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unsupported => write!(f, "this platform has no trash"),
            Error::NoTrash(p) => write!(f, "no trash directory for {}", p.display()),
            Error::Missing(p) => write!(f, "{} is not there", p.display()),
            Error::Unnamed(p) => write!(f, "{} has no name to trash", p.display()),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}
