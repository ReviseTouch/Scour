//! Why a file could not be sent.

use std::path::PathBuf;

#[derive(Debug)]
pub enum Error {
    /// Not one of the places [`targets`] offers now.
    Unknown(String),
    /// A directory, to a place that only takes files.
    FilesOnly,
    /// A path that is not there, or has no name.
    Missing(PathBuf),
    /// Not on this platform.
    Unsupported,
    Io(std::io::Error),
}

impl Error {
    /// The catalogue key for what went wrong, where there is a fixed one.
    pub fn msgid(&self) -> &'static str {
        match self {
            Error::Unknown(_) => "that place is not there any more",
            Error::FilesOnly => "only files can be sent there",
            Error::Missing(_) => "it is not there",
            Error::Unsupported => "not on this platform",
            Error::Io(_) => "it could not be sent",
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unknown(id) => write!(f, "no place called {id} to send to"),
            Error::FilesOnly => write!(f, "only files can be sent there"),
            Error::Missing(p) => write!(f, "{} is not there", p.display()),
            Error::Unsupported => write!(f, "not on this platform"),
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
