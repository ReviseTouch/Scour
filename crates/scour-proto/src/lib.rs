//! What a client and the service say to each other: types and nothing else — no
//! socket, no threads, no serialisation format chosen.
//!
//! Queries cross as **text**, not as a parsed tree. The service parses, so the
//! language cannot come to mean three things in three callers.

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
    /// Make the pictures this desktop has not made yet. How many decoders may run at
    /// once is one number for the whole machine: one [`scour_thumbs::Maker`], in
    /// `scourd`. It **runs a program on the file**, so it is fenced like `stat`.
    Thumbnails {
        /// At most [`scour_thumbs::Maker::BATCH`]; the rest are ignored. A frontend is
        /// not a fence, so the cap is applied here too.
        files: Vec<String>,
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
    Settings(scour_settings::Settings),
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
            | Request::SetSettings { .. }
            // It starts programs and writes files. Nothing about the index changes,
            // but this predicate is a security boundary, not a classification.
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
        // Named one by one: `is_mutating` is exhaustive, so a new variant cannot be
        // forgotten, but an existing one could be quietly moved to the other side.
        let mutating: Vec<Request> = vec![
            Request::Rescan { path: None },
            Request::Maintain {
                level: Maintenance::Compact,
            },
            Request::Shutdown {},
            // A write to the desktop's thumbnail cache, and processes started for it.
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
        // Everything a caller can reasonably leave out has a default: the common
        // caller is a person typing JSON or a model filling in a schema.
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

    /// Every response shape, serialised and read back: an internally tagged enum
    /// cannot carry a bare string or a sequence, and serde says so only when sending.
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

    /// A whole answer says nothing about being one, and a piece says it. That
    /// asymmetry is what makes the field free: only the frames nobody used to ask for
    /// carry the extra key.
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

        // Read back as false when absent, as every older frame on the wire is.
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
        // Zero is "I have seen nothing", so a service that has applied anything
        // answers at once rather than making a new client wait out the timeout.
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
