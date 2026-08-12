//! The command line.
//!
//! A client, and only a client. It holds no index, walks no filesystem and
//! does not link the engine — it opens a socket and speaks [`scour_proto`],
//! exactly as the MCP server and, later, the user interface do. Any of the
//! three could be deleted without the others noticing.
//!
//! Every command takes `--json`, and what it prints is the protocol type
//! serialised directly. There is no second format to keep in step: a script
//! reading `scour search --json` is reading the same bytes the service sent.

mod render;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use scour_core::{FacetBy, Maintenance, Page, SortKey};
use scour_ipc::Client;
use scour_proto::Request;

#[derive(Debug, Parser)]
#[command(name = "scour", about = "Instant file search", version)]
struct Args {
    /// Talk to a service listening here.
    #[arg(long, global = true, value_name = "PATH")]
    socket: Option<String>,
    /// Print the raw protocol reply.
    #[arg(long, global = true)]
    json: bool,
    /// How many results. Must come *before* the query.
    #[arg(long, short = 'n', default_value_t = 40)]
    limit: u32,
    /// What to order by. Must come *before* the query.
    #[arg(long, short, default_value = "relevance")]
    sort: Sort,
    #[command(subcommand)]
    command: Option<Command>,
    /// A query, when no subcommand is given.
    ///
    /// **Everything from here on is the query, flags included.** That is on
    /// purpose — a filename can contain `--` and a search tool that refuses to
    /// look for it is broken — but it has one sharp edge worth knowing: a flag
    /// written *after* the query is searched for rather than obeyed, and the
    /// result is a silent zero. `scour rapor -n 100` looks for the three words
    /// `rapor -n 100`. Put options first, or use `scour search`.
    #[arg(trailing_var_arg = true)]
    query: Vec<String>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Search for files.
    Search {
        query: Vec<String>,
        #[arg(long, short, default_value = "modified")]
        sort: Sort,
        /// Oldest, smallest or A-Z first.
        #[arg(long, short)]
        ascending: bool,
        #[arg(long, short = 'n', default_value_t = 40)]
        limit: u32,
        #[arg(long, short, default_value_t = 0)]
        offset: u32,
        /// Stop counting matches here.
        ///
        /// The total is the only work left that is proportional to the number
        /// of matches, so a low cap is what a search box wants and a high one
        /// is what a report wants. Capped totals are printed with a `+`.
        #[arg(long, default_value_t = 100_000)]
        count_cap: u32,
    },
    /// How many files match.
    Count { query: Vec<String> },
    /// Group the matching files.
    Facets {
        query: Vec<String>,
        /// kind, ext, or a directory path.
        #[arg(long, short, default_value = "kind")]
        by: String,
        #[arg(long, short = 'n', default_value_t = 10)]
        top: u32,
    },
    /// List a directory, from the index.
    Tree {
        path: String,
        #[arg(long, short, default_value_t = 1)]
        depth: u32,
        #[arg(long, short = 'n', default_value_t = 50)]
        limit: u32,
    },
    /// Everything known about one path.
    Stat { path: String },
    /// What a folder weighs, and which of its children weigh the most.
    Du {
        /// Empty for everything indexed.
        #[arg(default_value = "")]
        path: String,
        #[arg(long, short = 'n', default_value_t = 20)]
        top: u32,
    },
    /// The same file, several times over — largest saving first.
    ///
    /// Works down from the biggest files, because a unique size rules out only
    /// 6.2% of files but everything over a megabyte is 18,723 of them holding
    /// 141.8 GB. `--budget-mb 0` reads nothing and answers from the sizes
    /// alone, which is free and already says where the disk might be going.
    Dupes {
        /// Empty for everything indexed.
        #[arg(default_value = "")]
        under: String,
        /// Ignore anything smaller, in megabytes.
        #[arg(long, default_value_t = 1)]
        min_mb: u64,
        /// How much may be read confirming, in megabytes. Zero reads nothing.
        #[arg(long, default_value_t = 1024)]
        budget_mb: u64,
        #[arg(long, short = 'n', default_value_t = 20)]
        top: u32,
    },
    /// Read a query back without running it.
    Explain { query: Vec<String> },
    /// The query language reference.
    Syntax,
    /// The configured sources.
    Sources,
    /// What the service is doing.
    Status,
    /// What the index contains.
    Stats,
    /// Walk the filesystem again.
    Rescan {
        /// Narrow it to one subtree.
        path: Option<String>,
    },
    /// Housekeeping: flush, compact or rebuild.
    Maintain {
        #[arg(default_value = "flush")]
        level: Level,
    },
    /// Print the MCP configuration to paste into a client.
    McpConfig,
    /// Where the settings and the index live.
    Where,
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Sort {
    /// How well the name answers the query. Descending by default, like the
    /// others, and the only order that depends on what was typed.
    Relevance,
    Name,
    Path,
    Size,
    Modified,
    Created,
    Accessed,
    Ext,
    Kind,
}

impl From<Sort> for SortKey {
    fn from(s: Sort) -> SortKey {
        match s {
            Sort::Relevance => SortKey::Relevance,
            Sort::Name => SortKey::Name,
            Sort::Path => SortKey::Path,
            Sort::Size => SortKey::Size,
            Sort::Modified => SortKey::Modified,
            Sort::Created => SortKey::Created,
            Sort::Accessed => SortKey::Accessed,
            Sort::Ext => SortKey::Ext,
            Sort::Kind => SortKey::Kind,
        }
    }
}

#[derive(Debug, Clone, Copy, clap::ValueEnum)]
enum Level {
    Flush,
    Compact,
    Rebuild,
}

impl From<Level> for Maintenance {
    fn from(l: Level) -> Maintenance {
        match l {
            Level::Flush => Maintenance::Flush,
            Level::Compact => Maintenance::Compact,
            Level::Rebuild => Maintenance::Rebuild,
        }
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Two commands answer without a service, because both are things you reach
    // for when the service is what is not working.
    match &args.command {
        Some(Command::McpConfig) => return render::mcp_config(),
        Some(Command::Where) => return render::locations(),
        _ => {}
    }

    let addr = args
        .socket
        .clone()
        .unwrap_or_else(|| scour_config::Config::load_or_default().0.socket());
    let mut client = Client::connect(&addr).with_context(|| {
        format!("no Scour service is listening on {addr}. Start one with `scourd`.")
    })?;

    let request = build(&args)?;
    // The query text has to survive the call, for two things. `explain` prints
    // it back with its own colouring — and **any** answer may carry a warning
    // about a term the parser could not read, which arrives as offsets into
    // this string. A warning about `>abc` with no `size:` in front of it does
    // not say who refused it.
    let echo = match &request {
        scour_proto::Request::Explain { query, .. }
        | scour_proto::Request::Search { query, .. }
        | scour_proto::Request::Count { query, .. }
        | scour_proto::Request::Facets { query, .. } => Some(query.clone()),
        _ => None,
    };
    let reply = client.call(request)?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&reply)?);
        return Ok(());
    }
    render::human(&reply, echo.as_deref())
}

fn build(args: &Args) -> Result<Request> {
    let join = |v: &[String]| v.join(" ");
    Ok(match &args.command {
        // No subcommand: everything after the program name is the query, which
        // is the shape people actually type. Ordered by relevance rather than
        // by date, because someone who typed a word wants the file that answers
        // it and not the file that happens to be newest.
        None => Request::Search {
            query: join(&args.query),
            sort: args.sort.into(),
            descending: true,
            page: Page::new(0, args.limit),
        },
        Some(Command::Search {
            query,
            sort,
            ascending,
            limit,
            offset,
            count_cap,
        }) => Request::Search {
            query: join(query),
            sort: (*sort).into(),
            descending: !*ascending,
            page: Page {
                offset: *offset,
                limit: *limit,
                count_cap: *count_cap,
            },
        },
        Some(Command::Count { query }) => Request::Count {
            query: join(query),
            cap: 1_000_000,
        },
        Some(Command::Facets { query, by, top }) => Request::Facets {
            query: join(query),
            by: vec![match by.as_str() {
                "kind" => FacetBy::Kind,
                "ext" => FacetBy::Ext { top: *top },
                dir => FacetBy::Dir {
                    path: dir.to_owned(),
                    top: *top,
                },
            }],
        },
        Some(Command::Tree { path, depth, limit }) => Request::Tree {
            path: path.clone(),
            depth: *depth,
            limit: *limit,
        },
        Some(Command::Stat { path }) => Request::Stat { path: path.clone() },
        Some(Command::Dupes {
            under,
            min_mb,
            budget_mb,
            top,
        }) => Request::Duplicates {
            under: under.clone(),
            min_size: min_mb * 1024 * 1024,
            read_budget: budget_mb * 1024 * 1024,
            top: *top,
        },
        Some(Command::Du { path, top }) => Request::Usage {
            path: path.clone(),
            top: *top,
        },
        Some(Command::Explain { query }) => Request::Explain {
            query: join(query),
            // A command line has no caret, so there is nothing to complete.
            cursor: None,
        },
        Some(Command::Syntax) => Request::Syntax {},
        Some(Command::Sources) => Request::Sources {},
        Some(Command::Status) => Request::Status {},
        Some(Command::Stats) => Request::Stats {},
        Some(Command::Rescan { path }) => Request::Rescan { path: path.clone() },
        Some(Command::Maintain { level }) => Request::Maintain {
            level: (*level).into(),
        },
        Some(Command::McpConfig | Command::Where) => unreachable!("handled before connecting"),
    })
}
