//! What a client and the service say to each other: types and nothing else — no
//! socket, no threads, no serialisation format chosen.
//!
//! Queries cross as **text**, not as a parsed tree. The service parses, so the
//! language cannot come to mean three things in three callers.

use scour_core::{
    Completion, Entry, Error, FacetBy, FacetResponse, IndexStats, MaintReport, Maintenance, Page,
    SearchResponse, SortKey, SourceInfo, Span, Status, TreeNode, UsageResponse,
};
/// What a disk is, named here so a face that reads a [`Disk`] needs no other crate.
pub use scour_volumes::{Kind as VolumeKind, Volume};
use serde::{Deserialize, Serialize};

/// The protocol version. Bumped when an existing message changes shape;
/// adding a variant does not need it.
pub const VERSION: u32 = 1;

/// One request, with the id its reply will carry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Call {
    pub id: u64,
    #[serde(flatten)]
    pub request: Request,
}

/// One frame of one answer. Almost every request is answered by exactly one; see
/// `more` for [`Request::Export`], which is answered by a run of them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub id: u64,
    /// Another frame with this id follows. Without the marker a client reading one
    /// line per answer takes the first piece as the whole and leaves the rest for the
    /// next request. Defaulted and omitted when false, so old frames read unchanged.
    #[serde(default, skip_serializing_if = "is_false")]
    pub more: bool,
    #[serde(flatten)]
    pub outcome: Outcome,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl Reply {
    /// A whole answer, in one frame.
    pub fn whole(id: u64, outcome: Outcome) -> Reply {
        Reply {
            id,
            more: false,
            outcome,
        }
    }

    /// A piece, with more to come.
    pub fn piece(id: u64, response: Response) -> Reply {
        Reply {
            id,
            more: true,
            outcome: Outcome::Ok(response),
        }
    }
}

/// **Not boxed.** One of these exists per request and the large variant is the answer
/// itself, so boxing would add an allocation to save one copy.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok(Response),
    Error(Error),
}

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
/// A disk and what is being done about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Disk {
    pub volume: scour_volumes::Volume,
    pub state: DiskState,
    /// How it is kept up to date; empty from an older service.
    #[serde(default)]
    pub seen: Seen,
}

/// How a disk or a folder is kept up to date, as a settings page says it:
/// watched live or walked now and then, when it was last walked, and when a
/// change in it last reached the index.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    /// Its last whole walk in this run of the service.
    pub walked: Option<scour_core::Walked>,
    /// A watcher covers all of it: changes arrive as they happen.
    pub live: bool,
    /// When a change in it last reached the index, unix seconds.
    pub changed: Option<i64>,
    /// Being walked right now, or waiting to be.
    pub walking: bool,
}

/// One folder added from a face, as the settings page shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderState {
    pub path: String,
    /// Switched on: in the index.
    pub on: bool,
    /// Watched for changes, rather than only walked.
    pub watched: bool,
    /// Already inside a disk that is indexed, so not a source of its own.
    pub held: bool,
    /// Its last whole walk in this run of the service, if there was one.
    pub walked: Option<scour_core::Walked>,
    /// The rest of [`Seen`]; `walked` stays where older faces read it.
    #[serde(default)]
    pub live: bool,
    #[serde(default)]
    pub changed: Option<i64>,
    #[serde(default)]
    pub walking: bool,
}

/// Whether a disk is indexed, and who decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskState {
    /// Built in, and indexed without asking.
    Auto,
    /// Somebody said yes.
    Chosen,
    /// `config.toml` names it; it is changed there.
    Configured,
    /// Somebody took it out, or said no.
    Off,
    /// Plugged in or on a network, and nobody has said.
    Asking,
    /// Asked, and put off until the service starts again.
    Later,
    /// Built in, but `config.toml` lists the sources, so only if chosen.
    Manual,
}

impl DiskState {
    /// Its files are in the index.
    pub fn indexed(self) -> bool {
        matches!(
            self,
            DiskState::Auto | DiskState::Chosen | DiskState::Configured
        )
    }

    /// The name the wire uses.
    pub fn as_str(self) -> &'static str {
        match self {
            DiskState::Auto => "auto",
            DiskState::Chosen => "chosen",
            DiskState::Configured => "configured",
            DiskState::Off => "off",
            DiskState::Asking => "asking",
            DiskState::Later => "later",
            DiskState::Manual => "manual",
        }
    }
}

/// How long an unqualified [`Request::Await`] waits: a quiet machine costs one round
/// trip a minute, and a client that has lost its connection finds out.
fn default_wait_ms() -> u32 {
    25_000
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
            Request::Rescan { .. } => "rescan",
            Request::Recheck { .. } => "recheck",
            Request::Maintain { .. } => "maintain",
            Request::Reset {} => "reset",
            Request::Syntax {} => "syntax",
            Request::Shutdown {} => "shutdown",
        }
    }
}

#[cfg(test)]
mod tests;
