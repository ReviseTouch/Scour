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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    pub id: u64,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Ok(Response),
    Error(Error),
}

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
    },
    Facets(FacetResponse),
    Tree {
        root: TreeNode,
    },
    Stat(Entry),
    Usage(UsageResponse),
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
            Request::Rescan { .. } | Request::Maintain { .. } | Request::Shutdown {} => true,
            Request::Search { .. }
            | Request::Count { .. }
            | Request::Facets { .. }
            | Request::Tree { .. }
            | Request::Stat { .. }
            | Request::Usage { .. }
            | Request::Explain { .. }
            | Request::Sources {}
            | Request::Status {}
            | Request::Stats {}
            | Request::Await { .. }
            | Request::Syntax {} => false,
        }
    }

    /// A short, stable name for logs and metrics.
    pub fn name(&self) -> &'static str {
        match self {
            Request::Search { .. } => "search",
            Request::Count { .. } => "count",
            Request::Facets { .. } => "facets",
            Request::Tree { .. } => "tree",
            Request::Stat { .. } => "stat",
            Request::Usage { .. } => "usage",
            Request::Explain { .. } => "explain",
            Request::Sources {} => "sources",
            Request::Status {} => "status",
            Request::Stats {} => "stats",
            Request::Await { .. } => "await",
            Request::Rescan { .. } => "rescan",
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
    fn only_three_requests_change_anything() {
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
            Request::Rescan {
                path: Some("/a".into()),
            },
            Request::Maintain {
                level: Maintenance::Rebuild,
            },
            Request::Syntax {},
            Request::Shutdown {},
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
            Response::Text {
                text: "hello".into(),
            },
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
        let ok = Reply {
            id: 7,
            outcome: Outcome::Ok(Response::Accepted),
        };
        let json = serde_json::to_string(&ok).expect("serialise");
        assert_eq!(
            serde_json::from_str::<Reply>(&json).expect("deserialise"),
            ok
        );

        let bad = Reply {
            id: 8,
            outcome: Outcome::Error(Error::QueryTooShort { need: 3 }),
        };
        let json = serde_json::to_string(&bad).expect("serialise");
        let back: Reply = serde_json::from_str(&json).expect("deserialise");
        let Outcome::Error(e) = back.outcome else {
            panic!("expected an error")
        };
        // The code is what a caller matches on; the English is a fallback.
        assert_eq!(e.code(), "query_too_short");
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
