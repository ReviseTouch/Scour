//! The command line: a client and only a client. It holds no index, walks no
//! filesystem and does not link the engine — it opens a socket and speaks
//! [`scour_proto`]. `--json` prints the protocol type serialised directly, so
//! there is no second format to keep in step.

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
    #[arg(long, short = 'n', default_value_t = fits())]
    limit: u32,
    /// What to order by. Must come *before* the query.
    #[arg(long, short, default_value = "relevance")]
    sort: Sort,
    #[command(subcommand)]
    command: Option<Command>,
    /// A query, when no subcommand is given. Everything from here on is the
    /// query, flags included: put options first, or use `scour search`.
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
        #[arg(long, short = 'n', default_value_t = fits())]
        limit: u32,
        #[arg(long, short, default_value_t = 0)]
        offset: u32,
        /// Stop counting matches here. Capped totals are printed with a `+`.
        #[arg(long, default_value_t = 100_000)]
        count_cap: u32,
    },
    /// Write everything that matches to a spreadsheet.
    ///
    /// The whole matching set, in the index's own order rather than sorted.
    Export {
        query: Vec<String>,
        /// Where to write it. Left out, it goes to standard output.
        #[arg(long, short)]
        out: Option<std::path::PathBuf>,
        /// The columns, comma-separated, in order: `name path full ext size disk
        /// mtime ctime atime kind perm user group items`.
        #[arg(long, short)]
        columns: Option<String>,
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
    /// What the walk skips, built in and configured.
    Rules,
    /// This desktop's own folders, and which volumes record read times.
    Places,
    /// What can be shown of a file, and the head of it when that is text.
    Preview { path: String },
    /// What a folder weighs, and which of its children weigh the most.
    Du {
        /// Empty for everything indexed.
        #[arg(default_value = "")]
        path: String,
        #[arg(long, short = 'n', default_value_t = 20)]
        top: u32,
        /// Weigh only the files matching this query. The answer is then about
        /// those files rather than about the disk.
        #[arg(long, short = 'q', default_value = "")]
        query: String,
    },
    /// The same file, several times over — largest saving first.
    ///
    /// Works down from the biggest; `--budget-mb 0` reads nothing and answers from sizes alone.
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
    /// Open another face — the window, the terminal or the browser.
    ///
    /// Remembered as the one to open next time, through `scour-open`.
    Faces {
        /// `window`, `tui` or `browser`. Left out, it says which is current.
        face: Option<String>,
    },
    /// What Scour does, and which of its four faces does it.
    Features,
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
    /// How well the name answers the query — the only order that depends on
    /// what was typed.
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

/// How many rows a listing prints when nobody said: five at a terminal, so the
/// summary line stays on screen, and a fixed forty when piped, because output
/// that varies with an attached terminal cannot be scripted against.
fn fits() -> u32 {
    use std::io::IsTerminal;
    const PIPED: u32 = 40;
    const SHOWN: u32 = 5;
    if std::io::stdout().is_terminal() {
        SHOWN
    } else {
        PIPED
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    // These four need no service — which is what you reach for when the service
    // is the thing that is not working.
    match &args.command {
        Some(Command::McpConfig) => return render::mcp_config(),
        Some(Command::Where) => return render::locations(),
        Some(Command::Features) => {
            print!("{}", scour_ui::faces::table());
            return Ok(());
        }
        Some(Command::Faces { face }) => return render::faces(face.as_deref()),
        _ => {}
    }

    let addr = args
        .socket
        .clone()
        .unwrap_or_else(|| scour_config::Config::load_or_default().0.socket());
    let mut client = Client::connect(&addr).with_context(|| {
        format!("no Scour service is listening on {addr}. Start one with `scourd`.")
    })?;

    // Before `build`: an export is a run of frames written as they arrive, not
    // one request and one reply.
    if let Some(Command::Export {
        query,
        out,
        columns,
    }) = &args.command
    {
        return run_export(
            &mut client,
            &query.join(" "),
            out.as_deref(),
            columns.as_deref(),
        );
    }

    let request = build(&args)?;
    // The query text must survive the call: warnings arrive as offsets into it,
    // and `explain` prints it back coloured.
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

/// Write an export where it was asked for, a frame at a time. The escaping is
/// the service's, done once in `scour_export`, so every face writes one format.
/// The flush is checked: an export that fills the disk must not exit zero.
fn run_export(
    client: &mut Client,
    query: &str,
    out: Option<&std::path::Path>,
    columns: Option<&str>,
) -> Result<()> {
    use std::io::Write;

    let columns: Vec<String> = columns
        .map(|c| {
            c.split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();

    let mut sink: Box<dyn Write> = match out {
        Some(p) => Box::new(std::io::BufWriter::new(
            std::fs::File::create(p).with_context(|| format!("cannot write {}", p.display()))?,
        )),
        None => Box::new(std::io::BufWriter::new(std::io::stdout().lock())),
    };

    // The reason has to survive the `false` that stops the stream: the service
    // is told only "stop", so a full disk would otherwise look like success.
    let mut failed: Option<std::io::Error> = None;
    let done = client.stream(
        Request::Export {
            query: query.to_owned(),
            columns,
        },
        |piece| match piece {
            scour_proto::Response::ExportChunk { csv } => match sink.write_all(csv.as_bytes()) {
                Ok(()) => true,
                Err(e) => {
                    failed = Some(e);
                    false
                }
            },
            // A frame this binary does not know is ignored, not refused: a
            // newer service must not stop an export that is writing its rows.
            _ => true,
        },
    );
    // A closed pipe is not an error: Rust ignores `SIGPIPE` at start-up, so
    // `scour export | head` would otherwise fail where every other tool is quiet.
    if let Some(e) = failed {
        if e.kind() == std::io::ErrorKind::BrokenPipe {
            return Ok(());
        }
        return Err(e).context("the export could not be written");
    }
    let done = done?;
    if let Err(e) = sink.flush() {
        if e.kind() != std::io::ErrorKind::BrokenPipe {
            return Err(e).context("the export could not be written");
        }
        return Ok(());
    }
    drop(sink);
    // The row count on stderr, so it lands in neither the file nor the pipe. It
    // matches `scour count` for the same query, which is how a reader checks.
    if let scour_proto::Response::ExportDone { rows } = done {
        eprintln!("{rows}");
    }
    Ok(())
}

fn build(args: &Args) -> Result<Request> {
    let join = |v: &[String]| v.join(" ");
    Ok(match &args.command {
        // No subcommand: everything after the program name is the query.
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
        // `main` sends this itself. Unreachable rather than a silent fallback:
        // reaching it means the early return is gone and the request is wrong.
        Some(Command::Export { .. }) => unreachable!("an export is streamed in main"),
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
        Some(Command::Du { path, top, query }) => Request::Usage {
            path: path.clone(),
            top: *top,
            query: query.clone(),
        },
        Some(Command::Explain { query }) => Request::Explain {
            query: join(query),
            // A command line has no caret, so there is nothing to complete.
            cursor: None,
        },
        Some(Command::Rules) => Request::Rules {},
        Some(Command::Places) => Request::Places {},
        Some(Command::Preview { path }) => Request::Preview { path: path.clone() },
        Some(Command::Syntax) => Request::Syntax {},
        Some(Command::Sources) => Request::Sources {},
        Some(Command::Status) => Request::Status {},
        Some(Command::Stats) => Request::Stats {},
        Some(Command::Rescan { path }) => Request::Rescan { path: path.clone() },
        Some(Command::Maintain { level }) => Request::Maintain {
            level: (*level).into(),
        },
        Some(Command::McpConfig | Command::Where | Command::Features | Command::Faces { .. }) => {
            unreachable!("handled before connecting")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_count_leaves_room_for_the_summary_and_the_prompt() {
        let n = fits();
        assert!((5..=40).contains(&n), "outside the range: {n}");
        // Output is captured here, so this is the piped answer, which is fixed.
        assert_eq!(n, 40, "piped output should not follow a terminal");
    }
}
