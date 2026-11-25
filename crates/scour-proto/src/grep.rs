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
    /// Pictures, sound, video and disk images: not read for words, only for bytes.
    #[serde(default)]
    pub header_only: u64,
    /// Documents with no reader yet — PDF, `.doc`, `.xls`, `.ppt` — left unread.
    #[serde(default)]
    pub no_reader: u64,
    /// Held open by another program that shares it with nobody.
    #[serde(default)]
    pub locked: u64,
    /// Cloud placeholders, OneDrive's or iCloud's, and files on a FUSE mount
    /// that fetches them: reading would download them.
    #[serde(default)]
    pub offline: u64,
    /// Documents that opened and hold no text: scans without a text layer.
    #[serde(default)]
    pub no_text: u64,
    /// Documents their reader could not open: damaged, encrypted, misnamed.
    #[serde(default)]
    pub broken: u64,
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
    /// Every find in `text` of every word asked for, in order, `at` among
    /// them. Absent from an older service: `at` alone.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marks: Vec<(u32, u32)>,
}
