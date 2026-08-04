//! Typed failures.
//!
//! The core never composes a sentence for a human. It returns a variant, and
//! the frontend — CLI, MCP server, user interface — turns that variant into
//! words in the user's language. That is the only way three frontends and
//! several locales can describe the same failure without three copies of the
//! wording drifting apart.
//!
//! `Display` is implemented anyway, in English, because the English text is
//! also the message id: a frontend with no catalogue entry falls back to it and
//! is still correct, just untranslated.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::SourceId;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "error", rename_all = "snake_case")]
pub enum Error {
    /// The query could not be parsed. `at` is a byte offset into the input.
    QuerySyntax {
        at: usize,
        expected: String,
    },
    /// A substring index cannot answer a term this short.
    ///
    /// The index is built on trigrams, so two characters have nothing to match
    /// against. Saying so is better than returning everything or nothing.
    QueryTooShort {
        need: usize,
    },
    /// The query asks about document contents, but this index has none.
    ContentNotIndexed,
    /// No index exists yet. The first scan has not finished.
    NotIndexed,
    /// Another writer holds the index.
    ///
    /// `detail` says which directory and is optional so that an older client
    /// deserialising a newer reply still reads the variant.
    IndexBusy {
        #[serde(default)]
        detail: String,
    },
    /// The index is unreadable and has to be rebuilt.
    IndexCorrupt {
        detail: String,
    },
    /// The index was written by an older version and has to be built again.
    ///
    /// Separate from [`Error::IndexCorrupt`] because nothing is damaged and
    /// nothing was lost: an index is derived from the filesystem in its
    /// entirety, so this is a wait, not a loss. The two read the same to a
    /// program and could not be more different to a person watching a rebuild
    /// that takes minutes.
    IndexOutdated {
        found: u32,
        expected: u32,
    },
    /// A source is configured but not reachable — an unmounted drive, an
    /// expired cloud token.
    SourceUnavailable {
        source: SourceId,
    },
    /// The source does not support what was asked of it. `Caps` says which
    /// things those are before they are attempted; this covers the rest.
    Unsupported {
        what: String,
    },
    NotFound {
        path: String,
    },
    PermissionDenied {
        path: String,
    },
    Io {
        detail: String,
    },
    /// The configuration file is malformed.
    Config {
        detail: String,
    },
    /// The daemon is not running, or the socket could not be reached.
    Unreachable {
        detail: String,
    },
}

impl Error {
    pub fn unsupported(what: impl Into<String>) -> Self {
        Error::Unsupported { what: what.into() }
    }

    pub fn io(e: &std::io::Error, path: &str) -> Self {
        match e.kind() {
            std::io::ErrorKind::NotFound => Error::NotFound {
                path: path.to_owned(),
            },
            std::io::ErrorKind::PermissionDenied => Error::PermissionDenied {
                path: path.to_owned(),
            },
            _ => Error::Io {
                detail: e.to_string(),
            },
        }
    }

    /// A short, stable identifier for this failure.
    ///
    /// Machine-facing: it is what the MCP server reports and what a catalogue
    /// keys on. It never changes for a given variant, unlike the English text.
    pub fn code(&self) -> &'static str {
        match self {
            Error::QuerySyntax { .. } => "query_syntax",
            Error::QueryTooShort { .. } => "query_too_short",
            Error::ContentNotIndexed => "content_not_indexed",
            Error::NotIndexed => "not_indexed",
            Error::IndexBusy { .. } => "index_busy",
            Error::IndexCorrupt { .. } => "index_corrupt",
            Error::IndexOutdated { .. } => "index_outdated",
            Error::SourceUnavailable { .. } => "source_unavailable",
            Error::Unsupported { .. } => "unsupported",
            Error::NotFound { .. } => "not_found",
            Error::PermissionDenied { .. } => "permission_denied",
            Error::Io { .. } => "io",
            Error::Config { .. } => "config",
            Error::Unreachable { .. } => "unreachable",
        }
    }

    /// Would retrying, unchanged, plausibly work later?
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Error::IndexBusy { .. }
                | Error::NotIndexed
                | Error::SourceUnavailable { .. }
                | Error::Unreachable { .. }
        )
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::QuerySyntax { at, expected } => {
                write!(
                    f,
                    "Query syntax error at character {at}: expected {expected}"
                )
            }
            Error::QueryTooShort { need } => {
                write!(f, "Search terms need at least {need} characters")
            }
            Error::ContentNotIndexed => {
                f.write_str("This index does not contain document contents")
            }
            Error::NotIndexed => f.write_str("The index is not ready yet"),
            Error::IndexBusy { detail } if detail.is_empty() => f.write_str("The index is busy"),
            Error::IndexBusy { detail } => write!(f, "The index is busy: {detail}"),
            Error::IndexCorrupt { detail } => write!(f, "The index is damaged: {detail}"),
            Error::IndexOutdated { found, expected } => write!(
                f,
                "The index was written in format {found} and this is format {expected}; it has to be built again"
            ),
            Error::SourceUnavailable { source } => {
                write!(f, "Source {} is not available", source.0)
            }
            Error::Unsupported { what } => write!(f, "Not supported here: {what}"),
            Error::NotFound { path } => write!(f, "Not found: {path}"),
            Error::PermissionDenied { path } => write!(f, "Permission denied: {path}"),
            Error::Io { detail } => write!(f, "Input/output error: {detail}"),
            Error::Config { detail } => write!(f, "Configuration problem: {detail}"),
            Error::Unreachable { detail } => write!(f, "Cannot reach the Scour service: {detail}"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_distinct() {
        let all = [
            Error::QuerySyntax {
                at: 0,
                expected: "a term".into(),
            },
            Error::QueryTooShort { need: 3 },
            Error::ContentNotIndexed,
            Error::NotIndexed,
            Error::IndexBusy {
                detail: "in use".into(),
            },
            Error::IndexCorrupt {
                detail: String::new(),
            },
            Error::IndexOutdated {
                found: 3,
                expected: 4,
            },
            Error::SourceUnavailable {
                source: SourceId(0),
            },
            Error::unsupported("x"),
            Error::NotFound {
                path: String::new(),
            },
            Error::PermissionDenied {
                path: String::new(),
            },
            Error::Io {
                detail: String::new(),
            },
            Error::Config {
                detail: String::new(),
            },
            Error::Unreachable {
                detail: String::new(),
            },
        ];
        let mut codes: Vec<_> = all.iter().map(|e| e.code()).collect();
        codes.sort_unstable();
        let n = codes.len();
        codes.dedup();
        assert_eq!(codes.len(), n, "every variant needs its own code");
    }

    #[test]
    fn io_errors_keep_their_shape() {
        let e = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        assert_eq!(
            Error::io(&e, "/root/secret"),
            Error::PermissionDenied {
                path: "/root/secret".into()
            }
        );
    }

    #[test]
    fn transient_failures_are_worth_retrying() {
        assert!(Error::NotIndexed.is_transient());
        assert!(!Error::QueryTooShort { need: 3 }.is_transient());
    }
}
