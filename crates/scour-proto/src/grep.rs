//! Reading files for a pattern: how it is read, and what was found.

use scour_core::Span;
use serde::{Deserialize, Serialize};

/// How a [`Request::Grep`] pattern is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GrepMode {
    /// The characters as typed.
    #[default]
    Text,
    /// A regular expression over each line.
    Regex,
    /// Pairs of hex digits: those bytes, wherever they are.
    Hex,
}

/// What reading the files found, and how far it got.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GrepResponse {
    /// Files with a find, newest first.
    pub files: Vec<GrepFile>,
    /// Entries the query names, a floor when `capped`.
    pub candidates: u64,
    #[serde(default)]
    pub capped: bool,
    /// Files opened and read by this call.
    pub scanned: u64,
    /// Bytes read by this call.
    pub read: u64,
    /// Left unread for not being text. Said, so that silence is not "no".
    #[serde(default)]
    pub binary: u64,
    /// Left unread for being over `max_size`.
    #[serde(default)]
    pub too_big: u64,
    /// Could not be opened or read: gone, or not this person's to read.
    #[serde(default)]
    pub unreadable: u64,
    /// The `offset` to ask again with. None: every file the query names was read.
    #[serde(default)]
    pub next: Option<u32>,
    /// Terms of `query` the parser could not read as written.
    #[serde(default)]
    pub misread: Vec<Span>,
    #[serde(default)]
    pub took_us: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrepFile {
    pub path: String,
    #[serde(default = "default_meta")]
    pub meta: scour_core::Meta,
    #[serde(default = "default_kind")]
    pub kind: scour_core::Kind,
    pub hits: Vec<GrepHit>,
    /// There are finds past the ones shown.
    #[serde(default)]
    pub more: bool,
}

fn default_meta() -> scour_core::Meta {
    scour_core::Meta::UNKNOWN
}

fn default_kind() -> scour_core::Kind {
    scour_core::Kind::File
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GrepHit {
    /// Counted from one. None in a file that has no lines.
    #[serde(default)]
    pub line: Option<u64>,
    /// Bytes from the start of the file.
    pub offset: u64,
    /// The line, or the bytes around the find, safe to print.
    pub text: String,
    /// Where in `text` the find is: byte offset, then length.
    pub at: (u32, u32),
}
