//! The tools a model is offered. Each description says what the tool is for and
//! not merely what it does, so choosing between two of them is easy.

use std::sync::Mutex;

use rmcp::handler::server::wrapper::Parameters;
use rmcp::{ServerHandler, tool, tool_handler, tool_router};
use scour_core::{FacetBy, Page, SortKey};
use scour_ipc::Client;
use scour_proto::Request;
use serde::Deserialize;

#[derive(Clone)]
pub struct Scour {
    inner: std::sync::Arc<Inner>,
    // Read by the generated `#[tool_handler]` dispatch, not by anything here.
    #[allow(dead_code)]
    tool_router: rmcp::handler::server::router::tool::ToolRouter<Scour>,
}

struct Inner {
    /// One connection, serialised, and not opened until something is asked. `None`
    /// means none is held, whether or not anything was ever tried — every call
    /// takes the same path, so a service that starts later is picked up.
    client: Mutex<Option<Client>>,
    addr: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct SearchArgs {
    /// The query. Call `scour_syntax` for the language; a bare word matches
    /// any part of a file name.
    pub query: String,
    /// One of: modified, created, accessed, name, path, size, ext, kind.
    #[serde(default)]
    pub sort: Option<String>,
    /// Oldest, smallest or A-Z first. Newest first by default.
    #[serde(default)]
    pub ascending: Option<bool>,
    /// Rows to return. Keep it small; ask for a count instead of a long list.
    #[serde(default)]
    pub limit: Option<u32>,
    #[serde(default)]
    pub offset: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct QueryArgs {
    pub query: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct TreeArgs {
    /// Absolute path of the directory to list.
    pub path: String,
    /// How many levels down. One is a plain listing.
    #[serde(default)]
    pub depth: Option<u32>,
    /// Entries per level. The reply says how many were left out.
    #[serde(default)]
    pub limit: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct PathArgs {
    pub path: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct UsageArgs {
    /// The folder to weigh. Empty means everything indexed.
    #[serde(default)]
    pub path: String,
    /// How many child folders to name. Default 20.
    pub top: Option<u32>,
    /// Weigh only the files matching this query. Empty weighs all of them.
    /// With a query the answer is about those files — "where do the videos
    /// sit" — and is no longer what `du` would report for the folder.
    #[serde(default)]
    pub query: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct FacetArgs {
    /// Restrict the grouping to files matching this. Empty means everything.
    #[serde(default)]
    pub query: String,
    /// `kind`, `ext`, or an absolute directory path to group by child folder.
    pub by: String,
    #[serde(default)]
    pub top: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct DupeArgs {
    /// Limit it to this folder. Empty means everything indexed.
    #[serde(default)]
    pub under: String,
    /// Ignore files smaller than this many megabytes. Default 1.
    #[serde(default)]
    pub min_mb: Option<u64>,
    /// How many megabytes may be read to confirm. 0 answers from sizes alone,
    /// instantly, and reports those groups as unconfirmed. Default 1024.
    #[serde(default)]
    pub budget_mb: Option<u64>,
    /// How many groups. Default 20.
    #[serde(default)]
    pub top: Option<u32>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
pub struct NoArgs {}

#[tool_router]
impl Scour {
    /// Name the service to talk to. Nothing is opened here: a server that exits at
    /// spawn is one the MCP client marks dead, and "the service is not running" is
    /// an answer a model can act on — but only as the reply to a tool call.
    pub fn new(addr: &str) -> Scour {
        Scour {
            inner: std::sync::Arc::new(Inner {
                client: Mutex::new(None),
                addr: addr.to_owned(),
            }),
            tool_router: Self::tool_router(),
        }
    }

    /// Send a request, connecting or reconnecting as needed. Nothing mutating
    /// leaves here: every tool passes through, so the server is read-only because
    /// `Request::is_mutating` refuses, not because no such tool was written.
    fn call(&self, req: Request) -> Result<String, String> {
        if req.is_mutating() {
            return Err(crate::render::failure(&scour_core::Error::unsupported(
                format!("{}: this server is read-only", req.name()),
            )));
        }
        // Kept because a warning arrives as offsets into this string. Here
        // rather than in each tool, so the next one gets it for free.
        let query = match &req {
            Request::Search { query, .. }
            | Request::Count { query, .. }
            | Request::Facets { query, .. } => Some(query.clone()),
            _ => None,
        };
        let say = |r: &scour_proto::Response| {
            crate::render::human(r) + &crate::render::warning(r, query.as_deref())
        };
        let mut guard = match self.inner.client.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        // A transient failure drops the held connection and falls through to a
        // fresh one; anything else is the answer.
        if let Some(held) = guard.as_mut() {
            match held.call(req.clone()) {
                Ok(r) => return Ok(say(&r)),
                Err(e) if !e.is_transient() => return Err(crate::render::failure(&e)),
                Err(_) => *guard = None,
            }
        }
        match Client::connect(&self.inner.addr) {
            Ok(mut fresh) => {
                let out = match fresh.call(req) {
                    Ok(r) => Ok(say(&r)),
                    Err(e) => Err(crate::render::failure(&e)),
                };
                *guard = Some(fresh);
                out
            }
            Err(e) => Err(crate::render::failure(&e)),
        }
    }

    #[tool(
        description = "Find files and folders by name, extension, size, date or folder. \
                       Instant, over an index of the whole filesystem — use this instead of \
                       walking directories or shelling out to find. Returns one page of \
                       results plus the total number of matches. Call scour_syntax for the \
                       query language."
    )]
    fn scour_search(&self, Parameters(a): Parameters<SearchArgs>) -> Result<String, String> {
        self.call(Request::Search {
            query: a.query,
            sort: sort_of(a.sort.as_deref()),
            descending: !a.ascending.unwrap_or(false),
            page: Page {
                offset: a.offset.unwrap_or(0),
                limit: a.limit.unwrap_or(30).min(200),
                count_cap: 100_000,
            },
        })
    }

    #[tool(
        description = "Count matching files without listing them. Use this when the question \
                       is how many, or to find out whether a search is worth running at all."
    )]
    fn scour_count(&self, Parameters(a): Parameters<QueryArgs>) -> Result<String, String> {
        self.call(Request::Count {
            query: a.query,
            cap: 10_000_000,
        })
    }

    #[tool(
        description = "List a directory from the index. Bounded per level and instant even on \
                       a directory holding a million files — it reports how many entries it \
                       left out rather than returning them. Use this to explore a filesystem \
                       instead of reading directories one by one."
    )]
    fn scour_tree(&self, Parameters(a): Parameters<TreeArgs>) -> Result<String, String> {
        self.call(Request::Tree {
            path: a.path,
            depth: a.depth.unwrap_or(1).min(6),
            limit: a.limit.unwrap_or(50).min(500),
        })
    }

    #[tool(
        description = "Find files that are the same file, biggest saving first. Answers \
                       'what can I delete to get space back'. Works down from the largest \
                       files and confirms by reading and comparing them — the reply says \
                       per group whether it was confirmed or is only a size match, and a \
                       size match is NOT a duplicate. Do not delete anything on the strength \
                       of an unconfirmed group."
    )]
    fn scour_duplicates(&self, Parameters(a): Parameters<DupeArgs>) -> Result<String, String> {
        self.call(Request::Duplicates {
            under: a.under,
            min_size: a.min_mb.unwrap_or(1) * 1024 * 1024,
            read_budget: a.budget_mb.unwrap_or(1024) * 1024 * 1024,
            top: a.top.unwrap_or(20).min(200),
        })
    }

    #[tool(description = "Everything known about one path: size, dates, type, permissions.")]
    fn scour_stat(&self, Parameters(a): Parameters<PathArgs>) -> Result<String, String> {
        self.call(Request::Stat { path: a.path })
    }

    #[tool(
        description = "What a folder weighs and which of its children weigh the most, with \
                       how old the bytes are. Answers 'what is eating my disk' in \
                       milliseconds, over the index — do not walk the filesystem or shell out \
                       to du for this. Reports logical size and size on disk separately, and \
                       counts a hard-linked file once. Pass a query to weigh only part of it — \
                       'kind:video', 'ext:log', 'dm:>1y' — which answers where a kind of file \
                       sits rather than what the folder holds."
    )]
    fn scour_disk_usage(&self, Parameters(a): Parameters<UsageArgs>) -> Result<String, String> {
        self.call(Request::Usage {
            path: a.path,
            top: a.top.unwrap_or(20).min(200),
            query: a.query,
        })
    }

    #[tool(
        description = "Where this person keeps things — their Documents, Downloads and Pictures \
                       folders under whatever names their desktop uses — and which volumes do \
                       not record read times. Ask before guessing a path from a home directory, \
                       and before reading anything into an 'accessed' date: on a volume mounted \
                       `noatime` that date is when the file was created, not when it was last \
                       looked at."
    )]
    fn scour_places(&self) -> Result<String, String> {
        self.call(Request::Places {})
    }

    #[tool(
        description = "Summarise what a set of files consists of, grouped by type, by extension, \
                       or by the child folders of a directory. Answers 'what is in here' without \
                       listing anything."
    )]
    fn scour_facets(&self, Parameters(a): Parameters<FacetArgs>) -> Result<String, String> {
        let top = a.top.unwrap_or(15).min(100);
        self.call(Request::Facets {
            query: a.query,
            by: vec![match a.by.as_str() {
                "kind" => FacetBy::Kind,
                "ext" => FacetBy::Ext { top },
                dir => FacetBy::Dir {
                    path: dir.to_owned(),
                    top,
                },
            }],
        })
    }

    #[tool(
        description = "Read a query back as a sentence, without running it. The parser is \
                       forgiving — an unrecognised field is searched for as literal text — so \
                       use this when a search returns something surprising."
    )]
    fn scour_explain(&self, Parameters(a): Parameters<QueryArgs>) -> Result<String, String> {
        self.call(Request::Explain {
            query: a.query,
            cursor: None,
        })
    }

    #[tool(description = "The query language reference. Read this before composing a query.")]
    fn scour_syntax(&self, Parameters(_): Parameters<NoArgs>) -> Result<String, String> {
        self.call(Request::Syntax {})
    }

    #[tool(
        description = "Which parts of the filesystem are indexed, and how fresh the index is. \
                       Check this when a search finds nothing you expected — the answer is \
                       often that the path is outside the indexed roots."
    )]
    fn scour_sources(&self, Parameters(_): Parameters<NoArgs>) -> Result<String, String> {
        let sources = self.call(Request::Sources {})?;
        let status = self.call(Request::Status {})?;
        Ok(format!("{sources}\n{status}"))
    }
}

#[tool_handler]
impl ServerHandler for Scour {
    /// Overriding `#[tool_handler]`'s generated `get_info` replaces it silently,
    /// so the tool capability and the server name are declared here by hand —
    /// without them this introduces itself as rmcp with no tools at all.
    fn get_info(&self) -> rmcp::model::ServerInfo {
        let mut info = rmcp::model::ServerInfo::new(
            rmcp::model::ServerCapabilities::builder()
                .enable_tools()
                .build(),
        );
        info.server_info =
            rmcp::model::Implementation::new(env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"));
        info.instructions = Some(
            "Scour indexes the filesystem and answers questions about it instantly. \
                 Prefer these tools over walking directories or running find: a search over \
                 millions of files takes about a millisecond, and every answer is bounded, \
                 so nothing here can flood the context.\n\n\
                 Start with scour_sources to see what is indexed, scour_syntax for the query \
                 language, scour_tree to look around, and scour_search to find something. \
                 When a count would answer the question, use scour_count rather than listing \
                 files.\n\n\
                 Everything is read-only."
                .into(),
        );
        info
    }
}

fn sort_of(s: Option<&str>) -> SortKey {
    match s.unwrap_or("modified") {
        "name" => SortKey::Name,
        "path" => SortKey::Path,
        "size" => SortKey::Size,
        "created" => SortKey::Created,
        "accessed" => SortKey::Accessed,
        "ext" => SortKey::Ext,
        "kind" => SortKey::Kind,
        _ => SortKey::Modified,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOWHERE: &str = "/nonexistent/scour-should-not-be-here.sock";

    /// A server with nothing to talk to is still a server: exiting at spawn loses
    /// every tool for the session, and says so only in a log nobody opens.
    #[test]
    fn a_server_starts_with_no_service_to_talk_to() {
        let s = Scour::new(NOWHERE);
        let out = s.call(Request::Sources {}).unwrap_err();
        assert!(
            out.contains("Nothing answered") && out.contains(NOWHERE),
            "should name the path nothing answered at, said: {out}"
        );
    }

    /// One attempt per call and no memory of having failed, so a service that
    /// starts late is picked up.
    #[test]
    fn a_failed_call_does_not_poison_the_next_one() {
        let s = Scour::new(NOWHERE);
        let first = s.call(Request::Sources {});
        let second = s.call(Request::Sources {});
        assert_eq!(first, second);
    }

    /// The order matters: refusing after connecting would turn "this server is
    /// read-only" into "the service is not running" whenever it is down.
    #[test]
    fn a_request_that_would_write_is_refused_before_anything_is_opened() {
        let s = Scour::new(NOWHERE);
        let out = s.call(Request::Shutdown {}).unwrap_err();
        assert!(out.contains("read-only"), "{out}");
        assert!(
            s.inner.client.lock().unwrap().is_none(),
            "refusing should not have opened a connection"
        );
    }
}
