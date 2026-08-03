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
    /// One connection, serialised.
    ///
    /// Requests are milliseconds long and a model makes a few at a time, so a
    /// lock costs nothing measurable and avoids a connection per call.
    client: Mutex<Client>,
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
pub struct NoArgs {}

#[tool_router]
impl Scour {
    pub fn connect(addr: &str) -> scour_core::Result<Scour> {
        Ok(Scour {
            inner: std::sync::Arc::new(Inner {
                client: Mutex::new(Client::connect(addr)?),
                addr: addr.to_owned(),
            }),
            tool_router: Self::tool_router(),
        })
    }

    /// Send a request, reconnecting once if the service was restarted.
    fn call(&self, req: Request) -> String {
        let mut guard = match self.inner.client.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        match guard.call(req.clone()) {
            Ok(r) => crate::render::human(&r),
            Err(e) if e.is_transient() => match Client::connect(&self.inner.addr) {
                Ok(mut fresh) => {
                    let out = match fresh.call(req) {
                        Ok(r) => crate::render::human(&r),
                        Err(e) => crate::render::failure(&e),
                    };
                    *guard = fresh;
                    out
                }
                Err(e) => crate::render::failure(&e),
            },
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

    #[tool(description = "Everything known about one path: size, dates, type, permissions.")]
    fn scour_stat(&self, Parameters(a): Parameters<PathArgs>) -> String {
        self.call(Request::Stat { path: a.path })
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
            by: match a.by.as_str() {
                "kind" => FacetBy::Kind,
                "ext" => FacetBy::Ext { top },
                dir => FacetBy::Dir {
                    path: dir.to_owned(),
                    top,
                },
            },
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
    fn get_info(&self) -> rmcp::model::ServerInfo {
        let mut info = rmcp::model::ServerInfo::default();
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
