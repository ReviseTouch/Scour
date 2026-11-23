//! Reading the files a query names for what they hold. The index says which
//! files, newest first; `scour-grep` reads them; this decides how far to go
//! before answering, and says where it stopped.

use std::time::{Duration, Instant};

use scour_core::{Ast, Error, Index, Kind, Meta, Page, Result, SearchRequest, SortKey, Span};
use scour_grep::{Budget, Enter, Expr, File, Ground, Hit, Needle, Options, Outcome, Skip};

/// The first page is small, so a find among the newest files answers at once.
const FIRST: u32 = 64;
/// A page of candidates costs the index about 60 ms whatever its size —
/// 442 pages of 2,048 were 35 of the 47 s that 905,000 files took.
const WIDEST: u32 = 65_536;
/// Files handed to the readers at once: what is read past the last find wanted.
const BATCH: usize = 2048;

/// What to look for and how far to go.
#[derive(Debug, Clone)]
pub struct GrepAsk {
    pub needle: Needle,
    pub exact_case: bool,
    pub binary: bool,
    pub sort: SortKey,
    pub descending: bool,
    /// Files of the query already read by an earlier call.
    pub offset: u32,
    /// Stop at this many files with a find.
    pub top: u32,
    pub hits: u32,
    pub read_budget: u64,
    pub max_size: u64,
    pub timeout: Duration,
}

#[derive(Debug, Default)]
pub struct Grepped {
    pub files: Vec<GreppedFile>,
    pub candidates: u64,
    pub capped: bool,
    pub scanned: u64,
    pub read: u64,
    pub binary: u64,
    pub too_big: u64,
    pub unreadable: u64,
    /// The offset to go on from. None: everything the query names was read.
    pub next: Option<u32>,
    pub misread: Vec<Span>,
    pub took_us: u64,
}

#[derive(Debug)]
pub struct GreppedFile {
    pub path: String,
    pub meta: Meta,
    pub kind: Kind,
    pub hits: Vec<Hit>,
    pub more: bool,
}

pub(crate) fn run(
    index: &dyn Index,
    roots: &[String],
    query: Ast,
    count_cap: u32,
    ask: &GrepAsk,
    enter: Option<Enter>,
) -> Result<Grepped> {
    let began = Instant::now();
    let expr = Expr::new(&ask.needle, ask.exact_case).map_err(|e| Error::QuerySyntax {
        at: 0,
        expected: e.to_string(),
    })?;
    let options = Options {
        binary: ask.binary,
        max_size: ask.max_size,
        hits_per_file: ask.hits as usize,
        ..Options::default()
    };
    let ground = Ground::probe(roots.iter().map(|root| scour_core::path::to_path(root)));
    let budget = Budget::new(ask.read_budget, Some(began + ask.timeout));
    let mut out = Grepped::default();
    // No wider than the finds wanted need: every file of a batch is read.
    let (mut at, mut width) = (ask.offset, ask.top.clamp(8, FIRST));
    'pages: loop {
        let found = index.search(&SearchRequest {
            query: query.clone(),
            sort: ask.sort,
            descending: ask.descending,
            page: Page {
                offset: at,
                limit: width,
                count_cap,
            },
        })?;
        if at == ask.offset {
            (out.candidates, out.capped) = (found.total, found.capped);
            out.misread = found.misread;
        }
        let rows = found.hits.len() as u32;
        for (nth, batch) in found.hits.chunks(BATCH).enumerate() {
            let start = at.saturating_add((nth * BATCH) as u32);
            if budget.exhausted() {
                out.next = Some(start);
                break 'pages;
            }
            if let Some(next) = read(
                &expr,
                &options,
                &ground,
                enter.clone(),
                &budget,
                batch,
                ask.top,
                &mut out,
            ) {
                out.next = Some(start.saturating_add(next));
                break 'pages;
            }
        }
        at = at.saturating_add(rows);
        if rows < width {
            break;
        }
        if budget.exhausted() {
            out.next = Some(at);
            break;
        }
        width = (width * 8).min(WIDEST);
    }
    out.read = budget.spent();
    out.took_us = began.elapsed().as_micros() as u64;
    Ok(out)
}

/// Read one batch of rows into `out`. Returns the row to go on from when the
/// reading stopped inside the batch: enough was found, or the budget ran out.
#[allow(clippy::too_many_arguments)]
fn read(
    expr: &Expr,
    options: &Options,
    ground: &Ground,
    enter: Option<Enter>,
    budget: &Budget,
    rows: &[scour_core::Hit],
    top: u32,
    out: &mut Grepped,
) -> Option<u32> {
    let (places, files) = files_of(rows);
    for (k, outcome) in expr.scan(options, ground, enter, &files, budget).into_iter().enumerate() {
        match outcome {
            Outcome::NotReached => return Some(places[k]),
            Outcome::Clean => out.scanned += 1,
            Outcome::Skipped(Skip::Binary) => out.binary += 1,
            Outcome::Skipped(Skip::TooBig) => out.too_big += 1,
            Outcome::Skipped(Skip::NotAFile) => {}
            Outcome::Unreadable(_) => out.unreadable += 1,
            Outcome::Found { hits, more } => {
                out.scanned += 1;
                let row = &rows[places[k] as usize];
                out.files.push(GreppedFile {
                    path: row.path.clone(),
                    meta: row.meta,
                    kind: row.kind,
                    hits,
                    more,
                });
                if out.files.len() >= top.max(1) as usize {
                    return Some(places[k] + 1);
                }
            }
        }
    }
    None
}

/// The files among these rows, and the row each came from: directories are
/// in an answer and not in a reading.
pub(crate) fn files_of(rows: &[scour_core::Hit]) -> (Vec<u32>, Vec<File>) {
    rows.iter()
        .enumerate()
        .filter(|(_, hit)| !hit.is_dir)
        .map(|(row, hit)| {
            // A row never stat'ed says zero, which is not "empty".
            let known = hit.meta.mtime != 0 || hit.meta.size != 0;
            let size = known.then_some(hit.meta.size.max(0) as u64);
            let path = scour_core::path::to_path(&hit.path);
            (row as u32, File { path, size })
        })
        .unzip()
}
