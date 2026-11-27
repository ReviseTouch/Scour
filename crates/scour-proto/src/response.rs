//! What the service answers.

use super::*;
use scour_core::{
    Completion, Entry, FacetResponse, IndexStats, MaintReport, SearchResponse, SourceInfo, Span,
    Status, TreeNode, UsageResponse,
};
use serde::{Deserialize, Serialize};

/// Files that are, or may be, the same file. A wire type of its own so that
/// `scour-dupes`, which takes no dependencies, is not made to grow serde.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DupGroup {
    /// What each of them weighs.
    pub size: u64,
    pub paths: Vec<String>,
    /// What deleting all but one would give back.
    pub waste: u64,
    /// `size`, `edges` or `content` — how far the checking got. Only `content` means
    /// read end to end, which is "identical" rather than "the same size".
    pub certainty: String,
}

/// Internally tagged, which constrains the shapes here: a variant may hold a struct or
/// its own named fields, but **not** a bare string or a sequence — serde cannot merge a
/// tag into those, and it fails when sending. `Sources` and `Text` are named-field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Search(SearchResponse),
    Count {
        total: u64,
        /// The count stopped at the cap, so this is a floor.
        capped: bool,
        /// Terms the parser could not read as written. See [`SearchResponse::misread`];
        /// a count is the answer most likely to be believed without a second look.
        #[serde(default)]
        misread: Vec<scour_core::Span>,
        /// What the count cost, in microseconds. Defaulted, so an older client reading
        /// a newer reply is unaffected.
        #[serde(default)]
        took_us: u64,
    },
    Duplicates {
        groups: Vec<DupGroup>,
        /// Files considered at all.
        candidates: u64,
        /// Everything the groups could give back, **including any left out of
        /// `groups`**: a total that shrank on truncation could not be acted on.
        waste: u64,
        /// How much of `waste` was read and compared rather than guessed: 39.36 GiB by
        /// size against 18.29 GiB once read, measured here.
        #[serde(default)]
        proven: u64,
        /// Bytes read confirming.
        read: u64,
        /// Groups the read budget did not reach. Said rather than inferred: a partial
        /// answer that looks complete gets files deleted on a guess.
        unconfirmed: u64,
    },
    Grep(GrepResponse),
    /// What `resources` asked: files, memory, processor, programs.
    Resources(crate::Resources),
    /// What `texts` asked: whether documents' text is kept, where, and what is.
    Texts {
        on: bool,
        dir: String,
        max_mb: u32,
        /// Documents whose words are kept, scans with none, and those that
        /// would not open.
        documents: u64,
        no_text: u64,
        failed: u64,
        /// Compressed, on disk; and text left behind by changed documents.
        disk: u64,
        dead: u64,
        /// False when no extractor is installed: documents are then counted,
        /// not read.
        #[serde(default)]
        reader: bool,
    },
    /// A piece of an export: CSV text, whole lines, so a relay never buffers a partial
    /// one. The first piece carries the byte-order mark and the heading row. Sized by
    /// the service — see `EXPORT_CHUNK` in `scour-engine` — not one row a frame.
    ExportChunk {
        csv: String,
    },
    /// The export finished, and how many rows it wrote — a count the caller can check.
    /// A failure arrives as `Outcome::Error` instead of this frame.
    ExportDone {
        rows: u64,
    },
    /// Boxed: with the tabs in it, the largest answer by far, and every other
    /// answer would be its size too. The same on the wire.
    Settings(Box<scour_settings::Settings>),
    Facets(FacetResponse),
    Tree {
        root: TreeNode,
        /// What listing it cost, in microseconds. A depth-one listing of fifty children
        /// measured 112 ms: the count under each child is a query of its own.
        #[serde(default)]
        took_us: u64,
    },
    Stat(Entry),
    Usage(UsageResponse),
    /// See [`Request::Rules`]. `builtin_*` is code and `config_*` is `config.toml`,
    /// neither editable; `added_*` is what a window wrote and what a write replaces.
    /// `off` cuts across all three, keyed by [`scour_settings::rule_id`], and a
    /// switched-off rule is still listed in its own group.
    Rules {
        builtin_paths: Vec<String>,
        builtin_dirs: Vec<String>,
        builtin_files: Vec<String>,
        config_paths: Vec<String>,
        config_dirs: Vec<String>,
        config_files: Vec<String>,
        config_allow: Vec<String>,
        added_paths: Vec<String>,
        added_dirs: Vec<String>,
        added_files: Vec<String>,
        added_allow: Vec<String>,
        off: Vec<String>,
    },
    Places(scour_places::Places),
    Volumes {
        disks: Vec<Disk>,
        /// The folders added from a face, each with how it is kept and how its
        /// last walk went. Absent from an older service.
        #[serde(default)]
        folders: Vec<FolderState>,
    },
    Preview(scour_preview::Look),
    Flow(scour_flow::Flow),
    Thumbnails(scour_thumbs::Made),
    Explain {
        /// The query as it was understood.
        description: String,
        /// True when the query asks for document contents.
        needs_content: bool,
        /// The query cut into runs, in order, covering every byte of it. A frontend
        /// maps a [`Role`](scour_core::Role) to a colour and does nothing else.
        #[serde(default)]
        spans: Vec<Span>,
        /// What could be typed at `cursor`. Empty unless one was given.
        #[serde(default)]
        completions: Vec<Completion>,
    },
    Sources {
        sources: Vec<SourceInfo>,
    },
    Status(Status),
    Stats(IndexStats),
    Maintained(MaintReport),
    /// Accepted; the work happens in the background.
    Accepted,
    Text {
        text: String,
    },
}
