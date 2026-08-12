//! The tools a model is offered.
//!
//! Each description says what the tool is *for*, not merely what it does. A
//! model choosing between `scour_search` and `scour_tree` is making the same
//! decision a person does — am I looking for something, or am I looking
//! around — and the descriptions are written to make that decision easy.

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
    /// One connection, serialised, and **not opened until something is asked**.
    ///
    /// Requests are milliseconds long and a model makes a few at a time, so a
    /// lock costs nothing measurable and avoids a connection per call.
    ///
    /// `None` means no connection is held — either nothing has been asked yet,
    /// or the last attempt found nobody listening. That distinction does not
    /// matter here, which is the point: every call takes the same path, and a
    /// service that appears later is picked up by the next one.
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
    /// Name the service to talk to. **Nothing is opened here.**
    ///
    /// This used to connect, and to fail if it could not — which read as
    /// carefulness and was the opposite. An MCP client starts its servers when
    /// *it* starts, and this service is started separately by hand; whichever
    /// order that happens in, a server that exits at spawn is a server the
    /// client marks dead and does not spawn again. So the window in which
    /// `scourd` was a few seconds behind cost the whole session its tools, and
    /// the only way back was for somebody to notice and reconnect by hand.
    ///
    /// Refusing to start is also the wrong shape for the failure. "The service
    /// is not running" is an answer a model can act on — it can say so, and it
    /// can try again later — and it can only reach the model as the reply to a
    /// tool call, which requires having started.
    pub fn new(addr: &str) -> Scour {
        Scour {
            inner: std::sync::Arc::new(Inner {
                client: Mutex::new(None),
                addr: addr.to_owned(),
            }),
            tool_router: Self::tool_router(),
        }
    }

    /// Send a request, connecting or reconnecting as needed.
    ///
    /// **Nothing that changes anything leaves this function.** Every tool goes
    /// through here, so this is the one place the promise can be kept rather
    /// than repeated: the server is read-only because a request that would
    /// write is refused, not because the three tools that could write were
    /// never written. The difference matters the day somebody adds a tenth
    /// tool — `Request::is_mutating` knows the answer for a variant nobody has
    /// thought about yet, and a guard that has to be remembered is a guard
    /// that will not be.
    fn call(&self, req: Request) -> String {
        if req.is_mutating() {
            return crate::render::failure(&scour_core::Error::unsupported(format!(
                "{}: this server is read-only",
                req.name()
            )));
        }
        // The query text, kept because the answer may carry a warning about a
        // term the engine could not read, and that warning arrives as offsets
        // into this string. Here rather than in each tool, so that the next
        // query-taking tool gets it without anybody remembering to add it.
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
        // The connection we hold, if we hold one. A transient failure drops it
        // and falls through to a fresh one; anything else is the answer, since
        // reconnecting will not change what the service thinks of the request.
        if let Some(held) = guard.as_mut() {
            match held.call(req.clone()) {
                Ok(r) => return say(&r),
                Err(e) if !e.is_transient() => return crate::render::failure(&e),
                Err(_) => *guard = None,
            }
        }
        match Client::connect(&self.inner.addr) {
            Ok(mut fresh) => {
                let out = match fresh.call(req) {
                    Ok(r) => say(&r),
                    Err(e) => crate::render::failure(&e),
                };
                *guard = Some(fresh);
                out
            }
            Err(e) => crate::render::failure(&e),
        }
    }

    #[tool(
        description = "Find files and folders by name, extension, size, date or folder. \
                       Instant, over an index of the whole filesystem — use this instead of \
                       walking directories or shelling out to find. Returns one page of \
                       results plus the total number of matches. Call scour_syntax for the \
                       query language."
    )]
    fn scour_search(&self, Parameters(a): Parameters<SearchArgs>) -> String {
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
    fn scour_count(&self, Parameters(a): Parameters<QueryArgs>) -> String {
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
    fn scour_tree(&self, Parameters(a): Parameters<TreeArgs>) -> String {
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
    fn scour_duplicates(&self, Parameters(a): Parameters<DupeArgs>) -> String {
        self.call(Request::Duplicates {
            under: a.under,
            min_size: a.min_mb.unwrap_or(1) * 1024 * 1024,
            read_budget: a.budget_mb.unwrap_or(1024) * 1024 * 1024,
            top: a.top.unwrap_or(20).min(200),
        })
    }

    #[tool(description = "Everything known about one path: size, dates, type, permissions.")]
    fn scour_stat(&self, Parameters(a): Parameters<PathArgs>) -> String {
        self.call(Request::Stat { path: a.path })
    }

    #[tool(
        description = "What a folder weighs and which of its children weigh the most, with \
                       how old the bytes are. Answers 'what is eating my disk' in \
                       milliseconds, over the index — do not walk the filesystem or shell out \
                       to du for this. Reports logical size and size on disk separately, and \
                       counts a hard-linked file once."
    )]
    fn scour_disk_usage(&self, Parameters(a): Parameters<UsageArgs>) -> String {
        self.call(Request::Usage {
            path: a.path,
            top: a.top.unwrap_or(20).min(200),
        })
    }

    #[tool(
        description = "Summarise what a set of files consists of, grouped by type, by extension, \
                       or by the child folders of a directory. Answers 'what is in here' without \
                       listing anything."
    )]
    fn scour_facets(&self, Parameters(a): Parameters<FacetArgs>) -> String {
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
    fn scour_explain(&self, Parameters(a): Parameters<QueryArgs>) -> String {
        self.call(Request::Explain {
            query: a.query,
            cursor: None,
        })
    }

    #[tool(description = "The query language reference. Read this before composing a query.")]
    fn scour_syntax(&self, Parameters(_): Parameters<NoArgs>) -> String {
        self.call(Request::Syntax {})
    }

    #[tool(
        description = "Which parts of the filesystem are indexed, and how fresh the index is. \
                       Check this when a search finds nothing you expected — the answer is \
                       often that the path is outside the indexed roots."
    )]
    fn scour_sources(&self, Parameters(_): Parameters<NoArgs>) -> String {
        let sources = self.call(Request::Sources {});
        let status = self.call(Request::Status {});
        format!("{sources}\n{status}")
    }
}

#[tool_handler]
impl ServerHandler for Scour {
    /// **Written out rather than left to the macro, and that cost something.**
    ///
    /// `#[tool_handler]` generates a `get_info` that declares the tool
    /// capability and names the server; defining one by hand replaces it
    /// silently. What went out on the wire was `ServerInfo::default()`, whose
    /// `server_info` comes from *rmcp's* build environment — so this server
    /// introduced itself as **rmcp 3.1.0** with an empty `capabilities`, never
    /// declaring that it has tools at all. `tools/list` still answered, which
    /// is why a permissive client never complained and nobody noticed.
    ///
    /// So the two halves the macro would have provided are here explicitly,
    /// beside the instructions that are the reason for overriding it.
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

    /// A server with nothing to talk to is still a server.
    ///
    /// The failure this guards is not a crash, which is why it was invisible:
    /// the process exited 1 at spawn, the MCP client wrote "server failed" in
    /// a log nobody opens, and every tool was gone for the rest of the
    /// session. Whoever noticed blamed the query.
    #[test]
    fn a_server_starts_with_no_service_to_talk_to() {
        let s = Scour::new(NOWHERE);
        let out = s.call(Request::Sources {});
        assert!(
            out.contains("not running"),
            "should say the service is down, said: {out}"
        );
    }

    /// Asking again is how a service that starts late gets picked up.
    ///
    /// One attempt per call, and no memory of having failed — a server that
    /// gave up after the first refusal would be the same bug one call later.
    #[test]
    fn a_failed_call_does_not_poison_the_next_one() {
        let s = Scour::new(NOWHERE);
        let first = s.call(Request::Sources {});
        let second = s.call(Request::Sources {});
        assert_eq!(first, second);
    }

    /// The read-only promise costs no connection to keep.
    ///
    /// Worth its own test because the order matters: refusing *after*
    /// connecting would mean a service that is down turns "this server is
    /// read-only" into "the service is not running", which is a different and
    /// wrong answer to the question the caller asked.
    #[test]
    fn a_request_that_would_write_is_refused_before_anything_is_opened() {
        let s = Scour::new(NOWHERE);
        let out = s.call(Request::Shutdown {});
        assert!(out.contains("read-only"), "{out}");
        assert!(
            s.inner.client.lock().unwrap().is_none(),
            "refusing should not have opened a connection"
        );
    }
}
