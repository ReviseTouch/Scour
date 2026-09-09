//! What a client and the service say to each other.
//!
//! Types and nothing else — no socket, no threads, no serialisation format
//! chosen. That separation is what lets the same vocabulary carry over a local
//! socket, inside one process, and, when a phone eventually wants to talk to a
//! desktop, over something else entirely. It is also what lets the MCP server
//! be a thin mapping rather than a second implementation of everything.
//!
//! Queries cross as **text**, not as a parsed tree. The service parses. A model
//! or a script writing `ext:rs size:>1mb` should not have to know the shape of
//! an `Ast`, and every caller parsing for itself would be three chances for
//! the language to mean three things.

use scour_core::{
    Completion, Entry, Error, FacetBy, FacetResponse, IndexStats, MaintReport, Maintenance, Page,
    SearchResponse, SortKey, SourceInfo, Span, Status, TreeNode, UsageResponse,
};
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

/// One frame of one answer.
///
/// Almost every request is answered by exactly one of these. The exception is
/// [`Request::Export`], which is answered by a run of them — see `more`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub id: u64,
    /// Another frame with this id follows.
    ///
    /// **The whole of the streaming change to the wire.** The framing was
    /// already one JSON object per line with a request id on it; what it could
    /// not say was "this is a piece". Without a marker, a client that asked for
    /// something answered in pieces would read the first piece as the answer
    /// and leave the rest in its buffer, and the *next* request on that
    /// connection would be answered by the leftovers — a desynchronised stream
    /// that reports the wrong file rather than an error.
    ///
    /// Defaulted and omitted when false, so a frame written by a service that
    /// predates this is read unchanged, and a client that predates it ignores
    /// the field on the frames it will never ask for.
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

/// **Not boxed.** One of these exists per request, and the large variant is
/// the answer itself — the allocation boxing would add is one more than the
/// reply already made, to save copying it once.
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
    /// The whole matching set, as a spreadsheet, in pieces.
    ///
    /// **The only request answered by more than one frame.** Everything else
    /// here is a page or a summary and fits in one; this is the whole answer,
    /// which on this index is 2.24 M rows and a couple of hundred megabytes,
    /// and there is no size at which holding all of it was ever the plan.
    ///
    /// ## Why it is a request rather than something the caller assembles
    ///
    /// It was the latter, in the browser bridge, and paging is what made it
    /// impossible. A page costs what it takes to walk to its offset — 2.1 ms
    /// at the start of this index, 25.3 at a hundred thousand, 65.5 at half a
    /// million, 117.6 at a million — so a caller stitching pages together pays
    /// a cost that is linear per page and therefore quadratic in total. The
    /// whole of this index wrote 1.4 M lines in ten minutes and had not
    /// finished. The endpoint stopped at half a million and said so in the
    /// file, and the owner asked for no limit.
    ///
    /// A keyset cursor is the usual escape from an offset and cannot be
    /// written here: the query language takes a date and not a time.
    /// `dm:<=2026-03-07` parses; `dm:<=1770000000` and `dm:<2026-03-07T18:25:13`
    /// do not — checked, not assumed — so a cursor could only step a day, and
    /// one package install stamps a hundred thousand files in a day.
    ///
    /// So the walk happens once, in the service, and the rows leave as they
    /// are produced.
    ///
    /// ## The order, which is the index's and not the caller's
    ///
    /// There is no `sort` here, and its absence is the honest version of a
    /// field that would have to be ignored. Rows arrive in the order the index
    /// holds them: newest-first within a segment, which is the layout the whole
    /// search path is built on. See [`Response::ExportDone`] and the note in
    /// `scour-index-native`'s `scan` for what an ordered export would take and
    /// why it is not this change.
    Export {
        #[serde(default)]
        query: String,
        /// Column ids, in the order they are wanted. Empty means the five the
        /// window shows out of the box. Named by the caller because they are
        /// what the reader chose — see `scour_export::Sheet`.
        #[serde(default)]
        columns: Vec<String>,
    },
    /// How many match, up to the cap.
    Count {
        query: String,
        #[serde(default = "default_cap")]
        cap: u32,
    },
    /// Group the matching set — by kind, by extension, by folder, by age.
    ///
    /// A **list**, because they are all questions about the same rows and the
    /// index answers them from one walk of it. A sidebar asking for three
    /// separately walked the matching set three times.
    Facets {
        query: String,
        by: Vec<FacetBy>,
    },
    /// List a directory from the index.
    ///
    /// The operation an assistant exploring a filesystem actually performs, and
    /// the reason it is a request of its own rather than a search: it is
    /// bounded per level, so a directory holding a million files answers in the
    /// same time as one holding ten.
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
    /// What a directory weighs, and which of its children weigh the most.
    ///
    /// The question every disk-usage tool answers by walking the filesystem,
    /// which takes minutes. Here it is two passes over data already in memory.
    Usage {
        /// Empty means every root the index holds.
        #[serde(default)]
        path: String,
        #[serde(default = "default_usage_top")]
        top: u32,
        /// Weigh only the files this matches. Empty weighs all of them, which
        /// is the `du` question and what this answered before the field
        /// existed — defaulted, so a caller written against that version still
        /// asks it.
        #[serde(default)]
        query: String,
    },
    /// The same file, several times over.
    ///
    /// Ordered by what deleting the copies would give back, largest first,
    /// because that is the question — a count of duplicates is not something
    /// anybody wanted. `read_budget` of zero answers from the sizes alone,
    /// which costs nothing and is already the answer to "where might my disk
    /// be going".
    Duplicates {
        /// Empty means everything indexed.
        #[serde(default)]
        under: String,
        /// Ignore anything smaller. A unique size eliminates only 6.2% of
        /// files but candidates over a megabyte are 18,723 of them holding
        /// 141.8 GB, measured on the live index.
        #[serde(default = "default_dupe_floor")]
        min_size: u64,
        /// How many bytes may be read confirming. Zero reads nothing.
        #[serde(default = "default_dupe_budget")]
        read_budget: u64,
        #[serde(default = "default_dupe_top")]
        top: u32,
    },
    /// What this person's frontends remember: columns, widths, order, the
    /// queries they have run.
    ///
    /// **Held by the service because it is the only thing all the frontends
    /// talk to.** The window kept these in `localStorage`, which a browser
    /// writes on a clean shutdown and loses when it is killed — measured both
    /// ways — and which a terminal interface cannot read at all.
    Settings {},
    /// Change some of them.
    ///
    /// **Not the whole object, and it was.** The reasoning written here said
    /// *a frontend that sent one field would have to know what the others
    /// currently are anyway* — which is only true if it has to send them. It
    /// does not: what a change does not name, it does not touch. That is what
    /// lets a terminal and a window be open at once without each erasing what
    /// the other understands, and what lets a field be added to
    /// [`scour_settings::Settings`] without every frontend learning about it
    /// first.
    SetSettings {
        #[serde(default)]
        change: scour_settings::Change,
    },
    /// Read a query back — as a sentence, as coloured pieces, and as what
    /// could be typed next. Nothing is run.
    ///
    /// The parser is forgiving by design: a mistyped field is searched for as
    /// literal text rather than rejected. This is how a caller checks what its
    /// query was actually understood to mean.
    ///
    /// It is also what a search box calls on every keystroke, which is why the
    /// colouring lives here and not in the frontend. A frontend that tokenised
    /// the query itself would be a second parser, and the day the two
    /// disagreed the box would be confidently colouring a lie.
    Explain {
        query: String,
        /// Where the caret is, as a byte offset, when completions are wanted.
        /// Absent means none are — `scour explain` has no caret.
        #[serde(default)]
        cursor: Option<u32>,
    },
    /// What can be shown of one file — and, when that is text, the head of it.
    ///
    /// **The decision, not the bytes.** Deciding needs the file's first eight
    /// kilobytes and a table of extensions, and getting it wrong is invisible:
    /// a frontend guessing from the name calls `notes.bak` unreadable and
    /// `model.safetensors` text. Moving the bytes as well would be worse than
    /// useless — a browser asks for a video a piece at a time and cannot seek
    /// without ranged HTTP, so whoever speaks to the browser has to serve
    /// them. A terminal interface needs nothing but this reply.
    ///
    /// Fenced like `stat`: only a path the index holds.
    Preview {
        path: String,
    },
    /// Where this person keeps things, and what the volumes under them record.
    ///
    /// **Asked of the service because it runs where the files are.** Both
    /// halves were worked out in the browser bridge — `user-dirs.dirs` parsed
    /// there, `/proc/self/mounts` read there — which is one frontend's copy of
    /// a rule that four are meant to share. The same guess had already been
    /// wrong a layer higher: the page shipped with `/home/hasan` written into
    /// it. A frontend draws what it is told now.
    /// What the walk is told to skip, and what a person may change about it.
    ///
    /// **Two lists, kept apart on purpose.** The exclusions that do the work
    /// are a built-in set — `target`, `node_modules`, `.cargo/registry` and the
    /// rest — plus whatever the configuration adds. Only the second can be
    /// edited, so handing back one merged list would offer a window entries it
    /// cannot remove. A rail that lies about what a button does is worse than
    /// no button.
    ///
    /// The counts are not here and cannot be: what a rule excludes is *not in
    /// the index*, so the only way to know how many files it holds is to walk
    /// the disk. That is a separate, deliberate act — measured at nine minutes
    /// for `target` on this machine — and it is not something an answer to
    /// "what are the rules" should quietly do.
    Rules {},
    Places {},
    /// Make the pictures this desktop has not made yet.
    ///
    /// **Asked of the service because the bound is about the machine.** A
    /// thumbnail is produced by a separate process doing image or video
    /// decoding, and how many of those may run at once is one number for the
    /// whole desktop. A bridge that bounded itself to four, a window that
    /// bounded itself to four and a terminal that bounded itself to four would
    /// each be reasonable and the machine would be running twelve. There is one
    /// [`scour_thumbs::Maker`], in `scourd`, for the same reason there is one
    /// index.
    ///
    /// It follows the split `Preview` already made: the *decision and the
    /// work* cross the wire, the *bytes* do not. What comes back is which
    /// paths have a picture now — the caller then reads it out of the shared
    /// cache the way it already read the ones that were already there.
    ///
    /// **This one is allowed to take its time**, like `await` and unlike
    /// everything else: it is seconds of somebody else's decoding. A caller
    /// that cannot afford to wait must not put it where waiting matters —
    /// `scour-web` gives it a connection of its own so a search never queues
    /// behind one.
    ///
    /// Fenced like `stat` and for a much better reason than `preview`: this
    /// **runs a program on the file**. Only a path the index holds.
    Thumbnails {
        /// At most [`scour_thumbs::Maker::BATCH`]; the rest are ignored. A
        /// frontend is not a fence, so the cap is applied here as well as
        /// there.
        files: Vec<String>,
    },
    /// The configured sources and what each can do.
    Sources {},
    Status {},
    Stats {},
    /// Do not answer until the index would answer differently.
    ///
    /// The one request that is allowed to take its time. `since` is the
    /// [`Status::revision`] the caller last saw; the reply is a [`Status`],
    /// either because something changed or because `timeout_ms` ran out — and
    /// the revision in it says which.
    ///
    /// This is how a list stays live without polling. The alternative, a client
    /// asking every second whether anything happened, is 86,400 searches a day
    /// to discover that a desktop was idle; this is one blocked thread and no
    /// requests at all until something moves. It is also what tells the service
    /// that somebody is looking, which is what makes a change worth committing
    /// sooner than it would be for nobody.
    ///
    /// [`Status`]: scour_core::Status
    /// [`Status::revision`]: scour_core::Status::revision
    Await {
        #[serde(default)]
        since: u64,
        #[serde(default = "default_wait_ms")]
        timeout_ms: u32,
    },
    /// Walk a source again. `path` narrows it to one subtree.
    Rescan {
        #[serde(default)]
        path: Option<String>,
    },
    /// Look at these paths again, now — one `stat` each, no tree walked.
    ///
    /// **The answer to "the row is still there".** Something outside the index
    /// changed these files a moment ago, and the caller is the one that changed
    /// them: a face that has just sent a file to the trash, renamed one, or
    /// moved one. A watcher finds that out on its own schedule, which is right
    /// for a change nobody is waiting on and wrong for this one.
    ///
    /// It changes nothing on disk. Whoever moved the file did the moving, with
    /// their own permissions; the service only re-reads. That distinction is
    /// the reason this is a separate request rather than a `Delete`: a
    /// background service that indexes a filesystem should not also be able to
    /// empty one, and it still cannot.
    Recheck {
        paths: Vec<String>,
    },
    Maintain {
        #[serde(default)]
        level: Maintenance,
    },
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

/// Files that are, or may be, the same file.
///
/// A wire type of its own rather than `scour_dupes::Group` reaching this far:
/// `scour-dupes` has no dependencies and no serde, deliberately, and a
/// protocol crate is exactly the wrong place to force one on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DupGroup {
    /// What each of them weighs.
    pub size: u64,
    pub paths: Vec<String>,
    /// What deleting all but one would give back.
    pub waste: u64,
    /// `size`, `edges` or `content` — how far the checking got. **Only
    /// `content` means read end to end and compared**, and the difference
    /// decides whether a caller may say "identical" or only "the same size".
    pub certainty: String,
}
/// How long an unqualified [`Request::Await`] waits.
///
/// Long enough that a quiet machine costs one round trip a minute, short
/// enough that a client which has lost its connection finds out without
/// anybody restarting anything.
fn default_wait_ms() -> u32 {
    25_000
}

/// Internally tagged, which constrains the shapes allowed here: a variant may
/// hold a struct (its fields are flattened alongside the tag) or its own named
/// fields, but **not** a bare string or a sequence — serde cannot merge a tag
/// into those, and the failure appears at run time as a serialisation error
/// rather than at compile time. `Sources` and `Text` are named-field variants
/// for exactly that reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum Response {
    Search(SearchResponse),
    Count {
        total: u64,
        /// The count stopped at the cap, so this is a floor.
        capped: bool,
        /// Terms the parser could not read as written. See
        /// [`SearchResponse::misread`] — a count is the answer most likely to
        /// be believed without a second look, so it is the one that can least
        /// afford to drop the warning on its way out.
        #[serde(default)]
        misread: Vec<scour_core::Span>,
        /// What the count cost, in microseconds.
        ///
        /// **Every other answer carries this and these two did not**, which is
        /// how a hundred milliseconds hid in `tree`: nothing that reads
        /// `took_us` could see it, so nothing reported it, so nobody looked.
        /// Defaulted, so an older client reading a newer reply is unaffected.
        #[serde(default)]
        took_us: u64,
    },
    Duplicates {
        groups: Vec<DupGroup>,
        /// Files considered at all.
        candidates: u64,
        /// Everything the groups could give back, **including any left out of
        /// `groups`** — a total that shrank when the list was truncated would
        /// be a total nobody could act on.
        waste: u64,
        /// How much of `waste` was read and compared rather than guessed.
        /// The two are different questions: 39.36 GiB by size against 18.29
        /// GiB once read, measured here. Printing only the first tells
        /// somebody they can delete files that were never copies.
        #[serde(default)]
        proven: u64,
        /// Bytes read confirming.
        read: u64,
        /// Groups the read budget did not reach. Said rather than left to be
        /// inferred: a partial answer that looks complete is what gets files
        /// deleted on the strength of a guess.
        unconfirmed: u64,
    },
    /// A piece of an export: CSV text, whole lines, ready to write.
    ///
    /// **Whole lines**, so that a relay never has to buffer a partial one and
    /// a reader that stops mid-export stops on a row boundary. The first piece
    /// carries the byte-order mark and the heading row.
    ///
    /// Sized by the service — see `EXPORT_CHUNK` in `scour-engine` — rather
    /// than being one row a frame: a frame is a line of JSON with a `{"id":…}`
    /// on it, and paying that per row would make the framing most of the
    /// bytes on the wire.
    ExportChunk {
        csv: String,
    },
    /// The export finished, and how many rows it wrote.
    ///
    /// **A count the caller can check.** An export that stops early because
    /// the service failed halfway is otherwise indistinguishable from one that
    /// ran out of rows, and a truncated spreadsheet read as complete is a
    /// wrong conclusion about a disk. A failure arrives as `Outcome::Error`
    /// instead of this, so a reader that never sees either knows the answer is
    /// incomplete.
    ExportDone {
        rows: u64,
    },
    Settings(scour_settings::Settings),
    Facets(FacetResponse),
    Tree {
        root: TreeNode,
        /// What listing it cost, in microseconds. See [`Response::Count`] —
        /// this is the one where it mattered most: a depth-one listing of
        /// fifty children measured 112 ms, because the count under each child
        /// is a query of its own, and no dashboard could see any of it.
        #[serde(default)]
        took_us: u64,
    },
    Stat(Entry),
    Usage(UsageResponse),
    /// See [`Request::Rules`]. Three groups, because they are three different
    /// kinds of thing and only one of them a window may change:
    ///
    /// * `builtin_*` — code. Not editable anywhere.
    /// * `config_*` — `config.toml`, written by hand and left alone. Shown so
    ///   a person can see why something is missing, not offered for deletion:
    ///   rewriting that file through a serialiser would destroy the comments
    ///   and measurements that are most of its value.
    /// * `added_*` — what a window wrote, kept beside the index like the
    ///   column widths. This is the group a write replaces.
    ///
    /// And `off`, which cuts across all three: the ids of rules that are listed
    /// but not applied. **A switched-off rule is still reported in its own
    /// group**, because that is where it lives and switching it back on has to
    /// be possible — a panel built from what the engine is enforcing would
    /// watch the rule disappear rather than see it switch. This is what makes
    /// the two groups nobody can delete — code, and a hand-written file —
    /// something a person can nonetheless turn off. See
    /// [`scour_settings::rule_id`] for how an id is spelled; a frontend builds
    /// the same string to compare.
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
    Preview(scour_preview::Look),
    Thumbnails(scour_thumbs::Made),
    Explain {
        /// The query as it was understood.
        description: String,
        /// True when the query asks for document contents.
        needs_content: bool,
        /// The query cut into runs, in order, covering every byte of it.
        ///
        /// A frontend maps a [`Role`] to a colour and does nothing else. Two
        /// of the roles say *this is not what you think it is*, which is the
        /// only way a forgiving parser can be honest about what it did.
        ///
        /// [`Role`]: scour_core::Role
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
    /// Does this request change anything?
    ///
    /// **This is the MCP server's read-only promise**, and it is a promise
    /// rather than a description: `scour-mcp` refuses anything that answers
    /// `true` before the request reaches the socket, so the server is
    /// read-only because writes are stopped, not because the tools that could
    /// write were never written.
    ///
    /// Written as an exhaustive `match` on purpose. `matches!` would let a new
    /// variant default to harmless and be waved through, which is exactly the
    /// mistake this guards: whoever adds the next request has to say which
    /// side it is on, because nothing compiles until they do.
    pub fn is_mutating(&self) -> bool {
        match self {
            Request::Rescan { .. }
            | Request::Recheck { .. }
            | Request::Maintain { .. }
            | Request::SetSettings { .. }
            // **It starts programs and writes files.** Nothing about the index
            // changes, so this is the looser reading of "mutating" — but this
            // predicate is the MCP server's security boundary rather than a
            // classification, and "a model may cause this machine to run a
            // handful of image decoders on files it chose" is not something to
            // arrive at by leaving a variant on the quiet side of a match.
            | Request::Thumbnails { .. }
            | Request::Shutdown {} => true,
            Request::Search { .. }
            | Request::Export { .. }
            | Request::Count { .. }
            | Request::Facets { .. }
            | Request::Tree { .. }
            | Request::Stat { .. }
            | Request::Rules {}
            | Request::Places {}
            | Request::Preview { .. }
            | Request::Usage { .. }
            | Request::Duplicates { .. }
            | Request::Settings {}
            | Request::Explain { .. }
            | Request::Sources {}
            | Request::Status {}
            | Request::Stats {}
            | Request::Await { .. }
            | Request::Syntax {} => false,
        }
    }

    /// Is this answered by a run of frames rather than by one?
    ///
    /// **A client has to know before it asks.** `Client::call` reads exactly
    /// one line; asking it for something answered in pieces would leave the
    /// rest of them in the buffer for the next request to mistake for its own
    /// answer. So this is checked at the door — see `scour_ipc::Client::call`,
    /// which refuses rather than desynchronises.
    ///
    /// Exhaustive for the same reason [`Request::is_mutating`] is: whoever
    /// adds the next streaming request has to say so here, because nothing
    /// compiles until they do.
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
            | Request::Preview { .. }
            | Request::Usage { .. }
            | Request::Duplicates { .. }
            | Request::Settings {}
            | Request::SetSettings { .. }
            | Request::Explain { .. }
            | Request::Sources {}
            | Request::Status {}
            | Request::Stats {}
            | Request::Await { .. }
            | Request::Thumbnails { .. }
            | Request::Rescan { .. }
            | Request::Maintain { .. }
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
            Request::Preview { .. } => "preview",
            Request::Thumbnails { .. } => "thumbnails",
            Request::Usage { .. } => "usage",
            Request::Duplicates { .. } => "duplicates",
            Request::Settings {} => "settings",
            Request::SetSettings { .. } => "set-settings",
            Request::Explain { .. } => "explain",
            Request::Sources {} => "sources",
            Request::Status {} => "status",
            Request::Stats {} => "stats",
            Request::Await { .. } => "await",
            Request::Rescan { .. } => "rescan",
            Request::Recheck { .. } => "recheck",
            Request::Maintain { .. } => "maintain",
            Request::Syntax {} => "syntax",
            Request::Shutdown {} => "shutdown",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_changes_something_is_named_one_by_one() {
        // The MCP server refuses everything this calls mutating, so the list
        // is a security boundary rather than a classification. Named one by
        // one: `is_mutating` is exhaustive, so a new variant cannot be
        // forgotten — but an existing one could be quietly moved to the other
        // side, and that is what this catches.
        let mutating: Vec<Request> = vec![
            Request::Rescan { path: None },
            Request::Maintain {
                level: Maintenance::Compact,
            },
            Request::Shutdown {},
            // Not a write to the index — a write to the desktop's thumbnail
            // cache, and a handful of processes started to fill it.
            Request::Thumbnails { files: Vec::new() },
        ];
        for r in &mutating {
            assert!(r.is_mutating(), "{} has to be refused", r.name());
        }

        let readonly: Vec<Request> = vec![
            Request::Search {
                query: String::new(),
                sort: SortKey::default(),
                descending: true,
                page: Page::default(),
            },
            Request::Count {
                query: String::new(),
                cap: 1,
            },
            Request::Sources {},
            Request::Status {},
            Request::Stats {},
            Request::Syntax {},
            Request::Await {
                since: 0,
                timeout_ms: 1,
            },
        ];
        for r in &readonly {
            assert!(!r.is_mutating(), "{} is not a write", r.name());
        }
    }

    #[test]
    fn a_minimal_search_request_needs_only_a_query() {
        // Everything a caller can reasonably leave out has a default, because
        // the most common caller is a person typing JSON by hand or a model
        // filling in a schema.
        let call: Call =
            serde_json::from_str(r#"{"id":1,"op":"search","query":"rapor"}"#).expect("parse");
        assert_eq!(call.id, 1);
        let Request::Search {
            query,
            sort,
            descending,
            page,
        } = call.request
        else {
            panic!("expected a search");
        };
        assert_eq!(query, "rapor");
        assert_eq!(sort, SortKey::Modified);
        assert!(descending, "newest first is what a search box shows");
        assert_eq!(page.limit, 200);
    }

    #[test]
    fn every_request_round_trips() {
        let all = [
            Request::Search {
                query: "x".into(),
                sort: SortKey::Size,
                descending: false,
                page: Page::new(20, 10),
            },
            Request::Count {
                query: "x".into(),
                cap: 50,
            },
            Request::Facets {
                query: String::new(),
                by: vec![FacetBy::Ext { top: 5 }],
            },
            Request::Tree {
                path: "/a".into(),
                depth: 2,
                limit: 10,
            },
            Request::Stat {
                path: "/a/b".into(),
            },
            Request::Explain {
                query: "size:abc".into(),
                cursor: Some(4),
            },
            Request::Sources {},
            Request::Status {},
            Request::Stats {},
            Request::Await {
                since: 9,
                timeout_ms: 1_000,
            },
            Request::Export {
                query: "kind:font".into(),
                columns: vec!["name".into(), "size".into()],
            },
            Request::Rescan {
                path: Some("/a".into()),
            },
            Request::Maintain {
                level: Maintenance::Rebuild,
            },
            Request::Syntax {},
            Request::Shutdown {},
            Request::Thumbnails {
                files: vec!["/a/b.png".into()],
            },
        ];
        let mut names = Vec::new();
        for r in all {
            let json = serde_json::to_string(&r).expect("serialise");
            assert_eq!(
                serde_json::from_str::<Request>(&json).expect("deserialise"),
                r
            );
            names.push(r.name());
        }
        names.sort_unstable();
        let n = names.len();
        names.dedup();
        assert_eq!(names.len(), n, "every request needs its own name");
    }

    /// Every response shape, serialised and read back.
    ///
    /// The guard this exists to be: an internally tagged enum cannot carry a
    /// bare string or a sequence, and serde reports that when the message is
    /// *sent*, not when it is written. Without this, the failure surfaces as a
    /// client whose connection silently closes.
    #[test]
    fn every_response_round_trips() {
        let all = [
            Response::Search(SearchResponse::default()),
            Response::Count {
                total: 5,
                capped: true,
                misread: vec![scour_core::Span::new(0, 3, scour_core::Role::BadValue)],
                took_us: 0,
            },
            Response::Facets(FacetResponse::default()),
            Response::Tree {
                root: TreeNode {
                    name: "u".into(),
                    path: "/home/u".into(),
                    is_dir: true,
                    kind: scour_core::Kind::Dir,
                    size: 0,
                    mtime: 0,
                    children: 3,
                    nodes: Vec::new(),
                    truncated: false,
                },
                took_us: 0,
            },
            Response::Explain {
                description: "everything".into(),
                needs_content: false,
                spans: vec![scour_core::Span::new(0, 3, scour_core::Role::Text)],
                completions: Vec::new(),
            },
            Response::Sources {
                sources: Vec::new(),
            },
            Response::Status(Status::default()),
            Response::Stats(IndexStats::default()),
            Response::Maintained(MaintReport::default()),
            Response::Accepted,
            Response::ExportChunk {
                csv: "a.txt,12\r\n".into(),
            },
            Response::ExportDone { rows: 2 },
            Response::Text {
                text: "hello".into(),
            },
            Response::Thumbnails(scour_thumbs::Made {
                ready: vec!["/a/b.png".into()],
                ran: 1,
            }),
        ];
        for r in all {
            let json = serde_json::to_string(&r)
                .unwrap_or_else(|e| panic!("{r:?} cannot be sent at all: {e}"));
            assert_eq!(
                serde_json::from_str::<Response>(&json).expect("deserialise"),
                r
            );
        }
    }

    #[test]
    fn a_reply_carries_either_an_answer_or_a_typed_failure() {
        let ok = Reply::whole(7, Outcome::Ok(Response::Accepted));
        let json = serde_json::to_string(&ok).expect("serialise");
        assert_eq!(
            serde_json::from_str::<Reply>(&json).expect("deserialise"),
            ok
        );

        let bad = Reply::whole(8, Outcome::Error(Error::QueryTooShort { need: 3 }));
        let json = serde_json::to_string(&bad).expect("serialise");
        let back: Reply = serde_json::from_str(&json).expect("deserialise");
        let Outcome::Error(e) = back.outcome else {
            panic!("expected an error")
        };
        // The code is what a caller matches on; the English is a fallback.
        assert_eq!(e.code(), "query_too_short");
    }

    /// A whole answer says nothing about being one, and a piece says it.
    ///
    /// The asymmetry is deliberate and is what makes the field free: a service
    /// written before streaming existed emits exactly the frames this reads as
    /// whole, and a client written before it reads a whole frame unchanged.
    /// Only the frames nobody used to ask for carry the extra key.
    #[test]
    fn only_a_piece_of_an_answer_says_that_more_follows() {
        let whole = serde_json::to_string(&Reply::whole(1, Outcome::Ok(Response::Accepted)))
            .expect("serialise");
        assert!(
            !whole.contains("more"),
            "a whole answer is the frame it always was: {whole}"
        );

        let piece = Reply::piece(
            1,
            Response::ExportChunk {
                csv: "a.txt\r\n".into(),
            },
        );
        let json = serde_json::to_string(&piece).expect("serialise");
        assert!(json.contains("\"more\":true"), "{json}");
        let back: Reply = serde_json::from_str(&json).expect("deserialise");
        assert!(back.more);
        assert_eq!(back.id, 1);

        // Read back as false when absent, which is what every existing frame
        // on the wire looks like.
        let old: Reply =
            serde_json::from_str(r#"{"id":4,"ok":{"result":"accepted"}}"#).expect("parse");
        assert!(!old.more);
    }

    /// Exactly one request is answered in pieces, and a client checks before
    /// it asks — see `Request::streams`.
    #[test]
    fn only_the_export_answers_in_pieces() {
        assert!(
            Request::Export {
                query: String::new(),
                columns: Vec::new(),
            }
            .streams()
        );
        for r in [
            Request::Search {
                query: String::new(),
                sort: SortKey::default(),
                descending: true,
                page: Page::default(),
            },
            Request::Count {
                query: String::new(),
                cap: 1,
            },
            Request::Status {},
            Request::Shutdown {},
        ] {
            assert!(!r.streams(), "{} answers in one frame", r.name());
        }
    }

    /// The two fields a live client leaves out most of the time.
    #[test]
    fn waiting_needs_nothing_spelled_out() {
        let call: Call = serde_json::from_str(r#"{"id":3,"op":"await"}"#).expect("parse");
        let Request::Await { since, timeout_ms } = call.request else {
            panic!("expected a wait");
        };
        // Zero is "I have seen nothing", so a service that has already applied
        // anything answers at once rather than making a new client wait out a
        // timeout to learn what it could have been told immediately.
        assert_eq!(since, 0);
        assert_eq!(timeout_ms, 25_000);
    }

    #[test]
    fn mutating_requests_are_identified() {
        assert!(
            !Request::Search {
                query: String::new(),
                sort: SortKey::Name,
                descending: true,
                page: Page::default()
            }
            .is_mutating()
        );
        assert!(
            !Request::Tree {
                path: "/".into(),
                depth: 1,
                limit: 1
            }
            .is_mutating()
        );
        assert!(Request::Rescan { path: None }.is_mutating());
        assert!(Request::Shutdown {}.is_mutating());
    }
}
