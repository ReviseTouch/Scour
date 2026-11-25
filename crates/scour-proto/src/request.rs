//! What a client may ask, and what each question defaults to.

use super::*;
use scour_core::{FacetBy, Maintenance, Page, SortKey};
use serde::{Deserialize, Serialize};

#[allow(clippy::large_enum_variant)] // see `Outcome`: one per request
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// Search, and return a page of rows.
    Search {
        query: String,
        #[serde(default)]
        sort: SortKey,
        #[serde(default = "yes")]
        descending: bool,
        #[serde(default)]
        page: Page,
    },
    /// The whole matching set, as a spreadsheet, in pieces — the only request answered
    /// by more than one frame. Stitching pages cannot substitute: a page costs a walk
    /// to its offset, 117.6 ms at a million. No `sort` — rows arrive as the index holds
    /// them, newest-first within a segment.
    Export {
        #[serde(default)]
        query: String,
        /// Column ids, in the order they are wanted. Empty means the five the window
        /// shows out of the box; see `scour_export::Sheet`.
        #[serde(default)]
        columns: Vec<String>,
    },
    /// How many match, up to the cap.
    Count {
        query: String,
        #[serde(default = "default_cap")]
        cap: u32,
    },
    /// Group the matching set — by kind, by extension, by folder, by age. A list,
    /// because the index answers them all from one walk of the matching rows.
    Facets {
        query: String,
        by: Vec<FacetBy>,
    },
    /// List a directory from the index. A request of its own rather than a search
    /// because it is bounded per level: a million children answer as fast as ten.
    Tree {
        path: String,
        #[serde(default = "one")]
        depth: u32,
        #[serde(default = "default_tree_limit")]
        limit: u32,
    },
    /// Everything known about one path.
    Stat {
        path: String,
    },
    /// What a directory weighs, and which of its children weigh the most: two passes
    /// over data already in memory, where a disk-usage tool walks the filesystem.
    Usage {
        /// Empty means every root the index holds.
        #[serde(default)]
        path: String,
        #[serde(default = "default_usage_top")]
        top: u32,
        /// Weigh only the files this matches. Empty — the default — weighs all of
        /// them, which is the `du` question.
        #[serde(default)]
        query: String,
    },
    /// The same file, several times over, ordered by what deleting the copies would
    /// give back, largest first. `read_budget` of zero answers from the sizes alone.
    Duplicates {
        /// Empty means everything indexed.
        #[serde(default)]
        under: String,
        /// Ignore anything smaller. A unique size eliminates only 6.2% of files, but
        /// candidates over a megabyte are 18,723 of them holding 141.8 GB.
        #[serde(default = "default_dupe_floor")]
        min_size: u64,
        /// How many bytes may be read confirming. Zero reads nothing.
        #[serde(default = "default_dupe_budget")]
        read_budget: u64,
        #[serde(default = "default_dupe_top")]
        top: u32,
    },
    /// Read the files a query names and say which hold `pattern`. Bounded by
    /// `read_budget` and `timeout_ms`; an answer that stopped short says where
    /// to go on from, in `next`.
    Grep {
        /// Which files, in the query language. Empty is every file indexed.
        #[serde(default)]
        query: String,
        pattern: String,
        #[serde(default)]
        mode: GrepMode,
        /// `a` is not `A`. Off, the four Turkish i's are one letter too.
        #[serde(default)]
        exact_case: bool,
        /// Read what is not text as well, looking for UTF-16 in it too.
        #[serde(default)]
        binary: bool,
        /// Order in which candidate files are drawn from the index.
        #[serde(default)]
        sort: SortKey,
        #[serde(default = "yes")]
        descending: bool,
        /// Files of the query already read by an earlier call.
        #[serde(default)]
        offset: u32,
        /// Stop at this many files with a find.
        #[serde(default = "default_grep_top")]
        top: u32,
        /// Finds shown from one file. Zero only says there is one.
        #[serde(default = "default_grep_hits")]
        hits: u32,
        /// How many bytes may be read. Zero is no limit but the time.
        #[serde(default = "default_dupe_budget")]
        read_budget: u64,
        /// Larger files are not opened. Zero is no limit.
        #[serde(default = "default_grep_max_size")]
        max_size: u64,
        #[serde(default = "default_grep_timeout")]
        timeout_ms: u64,
        /// MiB a second at most, so a long reading leaves the disk to others.
        /// Zero, the default, is as fast as the disks go.
        #[serde(default)]
        rate_mib: u32,
    },
    /// What this person's frontends remember: columns, widths, order, the queries they
    /// have run. Held by the service, the only thing all the frontends talk to.
    Settings {},
    /// Change some of them. What a change does not name, it does not touch, so a
    /// terminal and a window can both be open without erasing each other, and a field
    /// can be added to [`scour_settings::Settings`] before any frontend knows it.
    SetSettings {
        #[serde(default)]
        change: scour_settings::Change,
    },
    /// Read a query back — as a sentence, as coloured pieces, and as what could be
    /// typed next. Nothing is run. A mistyped field is searched for as literal text,
    /// so this is how a caller checks what its query was understood to mean.
    Explain {
        query: String,
        /// Where the caret is, as a byte offset, when completions are wanted.
        /// Absent means none are — `scour explain` has no caret.
        #[serde(default)]
        cursor: Option<u32>,
    },
    /// What can be shown of one file — and, when that is text, the head of it. The
    /// decision, not the bytes: deciding needs the first eight kilobytes and a table of
    /// extensions. Fenced like `stat`: only a path the index holds.
    Preview {
        path: String,
    },
    /// What the walk is told to skip, and what a person may change about it. The
    /// built-in and configured exclusions cannot be edited, so they stay apart from
    /// what a window added. No counts: what a rule excludes is not in the index.
    Rules {},
    /// Where this person keeps things, and what the volumes under them record. Asked
    /// of the service because it runs where the files are.
    Places {},
    /// The disks this machine has, and for each whether it is indexed, left out,
    /// or waiting for somebody to say.
    Volumes {},
    /// Index this disk, or leave it out for good: a yes or a no to the question,
    /// and the switch in a disks panel.
    Choose {
        volume: String,
        index: bool,
    },
    /// Not now: stop asking about this disk until the service starts again.
    /// `volume`, not `id`: a call's own `id` is its number, and they share one object.
    Postpone {
        volume: String,
    },
    /// Make the pictures this desktop has not made yet. How many decoders may run at
    /// once is one number for the whole machine: one [`scour_thumbs::Maker`], in
    /// `scourd`. It **runs a program on the file**, so it is fenced like `stat`.
    Thumbnails {
        /// At most [`scour_thumbs::Maker::BATCH`]; the rest are ignored. A frontend is
        /// not a fence, so the cap is applied here too.
        files: Vec<String>,
        /// Target pixel edge (`large` 256 when omitted, `x-large` 512 / `xx-large` 1024
        /// for HiDPI preview panels).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<u32>,
    },
    /// The configured sources and what each can do.
    Sources {},
    Status {},
    Stats {},
    /// Do not answer until the index would answer differently. `since` is the
    /// [`Status::revision`](scour_core::Status::revision) the caller last saw; the reply
    /// is a `Status`, whether something changed or `timeout_ms` ran out.
    Await {
        #[serde(default)]
        since: u64,
        #[serde(default = "default_wait_ms")]
        timeout_ms: u32,
    },
    /// The reading of the last `content:` query: go on from where it stopped,
    /// or with `from_start` forget it and read the files again.
    Reread {
        #[serde(default)]
        from_start: bool,
    },
    /// What documents' text is kept, where, and how much.
    Texts {},
    /// Forget every kept text; the next search reads the documents again.
    ForgetTexts {},
    /// Walk a source again. `path` narrows it to one subtree.
    Rescan {
        #[serde(default)]
        path: Option<String>,
    },
    /// Look at these paths again, now — one `stat` each, no tree walked. For a change
    /// the caller itself just made and must not wait on the watcher to see. It changes
    /// nothing on disk: the service only re-reads.
    Recheck {
        paths: Vec<String>,
    },
    Maintain {
        #[serde(default)]
        level: Maintenance,
    },
    /// Empty the index and walk every source again, from nothing. Accepted at
    /// once; the walk runs behind it, and `Status` says when it is done.
    Reset {},
    /// The query language reference, as text.
    Syntax {},
    /// Stop the service.
    Shutdown {},
}

fn yes() -> bool {
    true
}

fn one() -> u32 {
    1
}

fn default_cap() -> u32 {
    10_000
}

fn default_tree_limit() -> u32 {
    200
}

fn default_usage_top() -> u32 {
    20
}

/// A megabyte: the measured knee. Below it the candidate list is a million
/// files covering 28 GB; above it, nineteen thousand covering 141.8 GB.
fn default_dupe_floor() -> u64 {
    1024 * 1024
}

/// A gigabyte of reading, which confirmed the whole interesting range on the
/// corpus those numbers came from.
fn default_dupe_budget() -> u64 {
    1024 * 1024 * 1024
}

fn default_dupe_top() -> u32 {
    50
}

fn default_grep_top() -> u32 {
    50
}

fn default_grep_hits() -> u32 {
    3
}

/// 64 MiB: a megabyte holds 98% of the files this was written against, and
/// the 1,717 over a hundred hold nearly half the bytes.
fn default_grep_max_size() -> u64 {
    64 << 20
}

fn default_grep_timeout() -> u64 {
    10_000
}

impl Request {
    /// Does this request change anything? `scour-mcp` refuses everything answering
    /// `true` before it reaches the socket, so this is a security boundary. Exhaustive
    /// on purpose: a new variant must not default to harmless.
    pub fn is_mutating(&self) -> bool {
        match self {
            Request::Rescan { .. }
            | Request::Recheck { .. }
            | Request::Maintain { .. }
            | Request::Reset {}
            | Request::SetSettings { .. }
            // It starts programs and writes files. Nothing about the index changes,
            // but this predicate is a security boundary, not a classification.
            | Request::Thumbnails { .. }
            | Request::Postpone { .. }
            | Request::ForgetTexts {}
            | Request::Choose { .. }
            | Request::Shutdown {} => true,
            Request::Search { .. }
            | Request::Export { .. }
            | Request::Count { .. }
            | Request::Facets { .. }
            | Request::Tree { .. }
            | Request::Stat { .. }
            | Request::Rules {}
            | Request::Places {}
            | Request::Volumes {}
            | Request::Preview { .. }
            | Request::Usage { .. }
            | Request::Duplicates { .. }
            | Request::Grep { .. }
            | Request::Settings {}
            | Request::Explain { .. }
            | Request::Sources {}
            | Request::Status {}
            | Request::Stats {}
            | Request::Await { .. }
            | Request::Reread { .. }
            | Request::Texts {}
            | Request::Syntax {} => false,
        }
    }

    /// Is this answered by a run of frames rather than by one? A client has to know
    /// before it asks: `scour_ipc::Client::call` reads one line and refuses these
    /// rather than desynchronise. Exhaustive, like [`Request::is_mutating`].
    pub fn streams(&self) -> bool {
        match self {
            Request::Export { .. } => true,
            Request::Recheck { .. }
            | Request::Rules {}
            | Request::Search { .. }
            | Request::Count { .. }
            | Request::Facets { .. }
            | Request::Tree { .. }
            | Request::Stat { .. }
            | Request::Places {}
            | Request::Volumes {}
            | Request::Postpone { .. }
            | Request::Choose { .. }
            | Request::Preview { .. }
            | Request::Usage { .. }
            | Request::Duplicates { .. }
            | Request::Grep { .. }
            | Request::Settings {}
            | Request::SetSettings { .. }
            | Request::Explain { .. }
            | Request::Sources {}
            | Request::Status {}
            | Request::Stats {}
            | Request::Await { .. }
            | Request::Thumbnails { .. }
            | Request::Texts {}
            | Request::ForgetTexts {}
            | Request::Reread { .. }
            | Request::Rescan { .. }
            | Request::Maintain { .. }
            | Request::Reset {}
            | Request::Syntax {}
            | Request::Shutdown {} => false,
        }
    }

    /// A short, stable name for logs and metrics.
    pub fn name(&self) -> &'static str {
        match self {
            Request::Search { .. } => "search",
            Request::Rules {} => "rules",
            Request::Export { .. } => "export",
            Request::Count { .. } => "count",
            Request::Facets { .. } => "facets",
            Request::Tree { .. } => "tree",
            Request::Stat { .. } => "stat",
            Request::Places {} => "places",
            Request::Volumes {} => "volumes",
            Request::Postpone { .. } => "postpone",
            Request::Choose { .. } => "choose",
            Request::Preview { .. } => "preview",
            Request::Thumbnails { .. } => "thumbnails",
            Request::Usage { .. } => "usage",
            Request::Duplicates { .. } => "duplicates",
            Request::Grep { .. } => "grep",
            Request::Settings {} => "settings",
            Request::SetSettings { .. } => "set-settings",
            Request::Explain { .. } => "explain",
            Request::Sources {} => "sources",
            Request::Status {} => "status",
            Request::Stats {} => "stats",
            Request::Await { .. } => "await",
            Request::Reread { .. } => "reread",
            Request::Texts {} => "texts",
            Request::ForgetTexts {} => "forget-texts",
            Request::Rescan { .. } => "rescan",
            Request::Recheck { .. } => "recheck",
            Request::Maintain { .. } => "maintain",
            Request::Reset {} => "reset",
            Request::Syntax {} => "syntax",
            Request::Shutdown {} => "shutdown",
        }
    }
}

/// How long an unqualified [`Request::Await`] waits: a quiet machine costs one round
/// trip a minute, and a client that has lost its connection finds out.
fn default_wait_ms() -> u32 {
    25_000
}
