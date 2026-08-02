//! One separator, everywhere.
//!
//! Paths cross this boundary as `/`-separated strings on every platform. The
//! index tokenises ancestors by splitting on `/`, the query language matches
//! `path:` fragments, the wire protocol carries them to a model that has never
//! heard of drive letters. Having two separators above this line would mean
//! every one of those places needing to know which platform produced the
//! string.
//!
//! So converting is this crate's job, in both directions, and nothing above it
//! ever sees a backslash.

use std::path::{Path, PathBuf};

/// A platform path as the rest of Scour sees it.
pub fn normalise(p: &str) -> String {
    if cfg!(windows) {
        p.replace('\\', "/")
    } else {
        p.to_owned()
    }
}

pub fn from_path(p: &Path) -> String {
    normalise(&p.to_string_lossy())
}

/// Back to something the operating system will accept.
pub fn to_path(p: &str) -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(p.replace('/', "\\"))
    } else {
        PathBuf::from(p)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips() {
        for p in ["/home/u/x.txt", "/a", "relative/thing"] {
            assert_eq!(from_path(&to_path(p)), p);
        }
    }

    #[test]
    #[cfg(windows)]
    fn backslashes_do_not_escape_this_crate() {
        assert_eq!(normalise(r"C:\Users\hasan"), "C:/Users/hasan");
        assert_eq!(
            to_path("C:/Users/hasan").to_string_lossy(),
            r"C:\Users\hasan"
        );
    }

    #[test]
    #[cfg(not(windows))]
    fn a_backslash_is_an_ordinary_character_on_unix() {
        // It is a legal filename character here, and rewriting it would rename
        // the file as far as every lookup is concerned.
        assert_eq!(normalise("/home/u/odd\\name"), "/home/u/odd\\name");
    }
}
