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
    #[arg(long, short = 'n', default_value_t = fits())]
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
        #[arg(long, short = 'n', default_value_t = fits())]
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
    /// Write everything that matches to a spreadsheet.
    ///
    /// The whole matching set, not a page of it — 2.24 M rows on this machine
    /// — written as it is walked. Nothing is held here or in the service, so
    /// the size of the answer is bounded by the disk it goes to and by
    /// nothing else.
    ///
    /// Rows arrive in the index's own order, which is not a sort by any
    /// column. Sort the file afterwards, or open it in something that sorts.
    Export {
        query: Vec<String>,
        /// Where to write it. Left out, it goes to standard output, which is
        /// what makes `scour export … | wc -l` work.
        #[arg(long, short)]
        out: Option<std::path::PathBuf>,
        /// The columns, comma-separated, in the order they should appear.
        ///
        /// One of `name path full ext size disk mtime ctime atime kind perm
        /// user group items`. The default five are what the window shows.
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
        /// Weigh only the files matching this query.
        ///
        /// The answer is then about those files and not about the disk — where
        /// your photos sit, not what `du` would say.
        #[arg(long, short = 'q', default_value = "")]
        query: String,
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
    /// Open another face — the window, the terminal or the browser — and
    /// remember that it is the one to open next time.
    ///
    /// **Through `scour-open`**, which owns the list of terminals and the
    /// rule about which face opens by default. A second copy of either here
    /// would be a second answer to the same question.
    Faces {
        /// `window`, `tui` or `browser`. Left out, it says which is current.
        face: Option<String>,
    },
    /// What Scour does, and which of its four faces does it.
    ///
    /// **The table is in the code, not in a plan.** Which face is behind on
    /// what is the thing that goes stale first when four of them share one
    /// service; it is `scour_ui::faces` and this prints it.
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

/// How many rows a listing should print when nobody said.
///
/// **What fits, rather than a number somebody picked.** It was forty, which
/// is more than most terminals are tall: the first lines scrolled away before
/// they could be read, and the summary at the bottom — the count, the
/// milliseconds — went with them. Forty is also arbitrary in the other
/// direction, on a tall screen it wastes two thirds of it.
///
/// So the terminal is asked. Three rows are left over for the summary line
/// and the prompt that follows it, and the answer is clamped: five, because
/// fewer is not a listing, and sixty, because past that a person scrolls
/// rather than reads and the service pays for rows nobody looks at.
///
/// Only when stdout is a terminal. Piped into `head`, `wc` or a script the
/// old fixed count stands — a program whose output changes with the window it
/// was not run in is a program that cannot be scripted against.
fn fits() -> u32 {
    const PIPED: u32 = 40;
    #[cfg(unix)]
    {
        // SAFETY: `isatty` reads a descriptor number and `ioctl` writes only
        // into the `winsize` handed to it.
        unsafe {
            if libc::isatty(libc::STDOUT_FILENO) != 1 {
                return PIPED;
            }
            let mut w: libc::winsize = std::mem::zeroed();
            if libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut w) != 0 || w.ws_row == 0 {
                return PIPED;
            }
            u32::from(w.ws_row).saturating_sub(3).clamp(5, 60)
        }
    }
    #[cfg(not(unix))]
    {
        PIPED
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Two commands answer without a service, because both are things you reach
    // for when the service is what is not working.
    match &args.command {
        Some(Command::McpConfig) => return render::mcp_config(),
        Some(Command::Where) => return render::locations(),
        // Three, now: what the faces can do is a fact about this build rather
        // than about any index.
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

    // Before `build`, because an export is not one request-and-reply and has
    // nowhere to put itself in the flow below: its answer is a run of frames
    // that go to a file or a pipe as they arrive, and `--json` printing a
    // pretty tree of two hundred megabytes is not a thing anybody wants.
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

/// Write an export where it was asked for, a frame at a time.
///
/// **The escaping is not here**, and that is the point of the whole change:
/// the service produced these bytes with `scour_export`, the same code the
/// browser bridge now relays, so a file written by this and a file downloaded
/// from the window are the same file. A second implementation of RFC 4180 in
/// the terminal is a second chance to get it wrong, and the way it goes wrong
/// is a spreadsheet that opens.
///
/// Buffered, because a frame is 128 KB of CSV and an unbuffered `write_all`
/// per frame is a syscall per frame — which is fine, and being explicit about
/// it costs a line. The flush is checked: an export that fills the disk must
/// not exit zero.
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

    // Set when a write fails, so that the reason survives the `false` that
    // stops the stream — the service is told nothing but "stop", and without
    // this a full disk would look like a successful export of however many
    // rows fitted.
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
            // Nothing else is sent as a piece of an export. Ignoring one is
            // safer than refusing: a service newer than this binary may have
            // something to add, and an export that still writes its rows is
            // better than one that stops because of a frame it did not know.
            _ => true,
        },
    );
    // **A closed pipe is not an error**, and Rust makes it look like one: it
    // ignores `SIGPIPE` at start-up, so `scour export | head` — the first thing
    // anybody types at a two-million-row export — comes back with `Broken pipe
    // (os error 32)` where every other tool on the machine exits quietly. The
    // reader got what it asked for and stopped, which is exactly the case the
    // whole stream is built to handle.
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
    // The row count, on stderr so that it does not land in the file or in
    // whatever the pipe feeds. It is what `scour count` prints for the same
    // query, and the pair is the only check a reader has that the file is
    // whole.
    if let scour_proto::Response::ExportDone { rows } = done {
        eprintln!("{rows}");
    }
    Ok(())
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
        // Handled before this is reached — an export is a run of frames, not a
        // request and a reply, and `main` sends it itself. Unreachable rather
        // than a silent fallback: if this ever runs, the early return above has
        // been removed and a wrong request is worse than a crash.
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

    /// **The listing has to end above the prompt.** Forty rows in a
    /// twenty-four-row terminal means the first sixteen and the summary line
    /// are gone before anybody reads them, and the summary is where the count
    /// and the milliseconds are.
    #[test]
    fn the_default_count_leaves_room_for_the_summary_and_the_prompt() {
        let n = fits();
        assert!((5..=60).contains(&n), "outside the clamp: {n}");
        // In a test the output is captured rather than a terminal, so this is
        // the piped answer — and the piped answer must be fixed. A number that
        // changed with a window the program was not run in is a number nothing
        // can be scripted against.
        assert_eq!(n, 40, "piped output should not follow a terminal");
    }
}
