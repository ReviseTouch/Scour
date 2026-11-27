//! Reading the files a query names for what they hold. The index says which
//! files, newest first; `scour-grep` reads them; this decides how far to go
//! before answering, and says where it stopped.

use std::sync::Arc;
use std::time::{Duration, Instant};

use scour_core::{Ast, Error, Index, Kind, Meta, Page, Result, SearchRequest, SortKey, Span};
use scour_grep::{
    Budget, Class, Enter, Expr, File, Ground, Hit, Needle, Options, Outcome, Skip, Texts,
    is_disk_image,
};

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
    /// A ZIP-based document's XML searched as it is: see `scour_grep::Options::markup`.
    pub markup: bool,
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
    /// Bytes a second at most; zero is as fast as the disks go.
    pub rate: u64,
    /// Nobody waits on it: every thread gives way from the first file, and
    /// the reading does not count as somebody using the index.
    pub background: bool,
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
    pub header_only: u64,
    pub no_reader: u64,
    pub locked: u64,
    pub offline: u64,
    pub no_text: u64,
    pub broken: u64,
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
    texts: Option<Arc<dyn Texts>>,
) -> Result<Grepped> {
    let began = Instant::now();
    let letters = scour_grep::Letters {
        exact_case: ask.exact_case,
        exact_accents: query.exact_accents,
    };
    let expr =
        Expr::forgiving(&ask.needle, letters, scour_core::mistakes_allowed).map_err(|e| {
            Error::QuerySyntax {
                at: 0,
                expected: e.to_string(),
            }
        })?;
    let mut options = Options {
        binary: ask.binary,
        markup: ask.markup,
        max_size: ask.max_size,
        hits_per_file: ask.hits as usize,
        ..Options::default()
    };
    let ground = Ground::probe(roots.iter().map(|root| scour_core::path::to_path(root)));
    let budget = Budget::new(ask.read_budget, Some(began + ask.timeout)).paced(ask.rate);
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
            // Past the first batch the reading is a long one, and gives way.
            options.quiet = ask.background || start > ask.offset;
            if let Some(next) = read(
                &expr,
                &options,
                &ground,
                enter.clone(),
                texts.clone(),
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
    texts: Option<Arc<dyn Texts>>,
    budget: &Budget,
    rows: &[scour_core::Hit],
    top: u32,
    out: &mut Grepped,
) -> Option<u32> {
    let (places, files) = files_of(rows);
    for (k, outcome) in expr
        .scan(options, ground, enter, texts, &files, budget)
        .into_iter()
        .enumerate()
    {
        match outcome {
            Outcome::NotReached => return Some(places[k]),
            Outcome::Clean => out.scanned += 1,
            Outcome::Skipped(Skip::Binary) => out.binary += 1,
            Outcome::Skipped(Skip::TooBig) => out.too_big += 1,
            Outcome::Skipped(Skip::NotAFile) => {}
            Outcome::Skipped(Skip::HeaderOnly) => out.header_only += 1,
            Outcome::Skipped(Skip::NoReader) => out.no_reader += 1,
            Outcome::Skipped(Skip::Locked) => out.locked += 1,
            Outcome::Skipped(Skip::Offline) => out.offline += 1,
            Outcome::Skipped(Skip::NoText) => out.no_text += 1,
            Outcome::Skipped(Skip::Broken) => out.broken += 1,
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
    let mut seen = std::collections::HashSet::new();
    rows.iter()
        .enumerate()
        // A link is read through its target, which is a row of its own; and
        // no file is read twice in one reading.
        .filter(|(_, hit)| !hit.is_dir && !is_link(hit.meta.mode) && seen.insert(&hit.id))
        .map(|(row, hit)| {
            // A row never stat'ed says zero, which is not "empty".
            let known = hit.meta.mtime != 0 || hit.meta.size != 0;
            let size = known.then_some(hit.meta.size.max(0) as u64);
            let path = scour_core::path::to_path(&hit.path);
            let class = class_of(hit.kind, &path);
            (row as u32, File { path, size, class })
        })
        .unzip()
}

/// What the index already knows of a file's kind, as the reader takes it.
fn class_of(kind: Kind, path: &std::path::Path) -> Option<Class> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
    match kind {
        Kind::Image | Kind::Audio | Kind::Video | Kind::Media => Some(Class::HeaderOnly),
        Kind::Archive if is_disk_image(ext) => Some(Class::HeaderOnly),
        _ => None,
    }
}

/// `S_IFLNK` in a recorded mode; zero, where none was recorded, is not one.
fn is_link(mode: i64) -> bool {
    mode & 0o170000 == 0o120000
}

#[cfg(test)]
mod tests {
    use super::*;
    use scour_core::{EntryId, SourceId};

    fn row(ino: u64, path: &str, kind: Kind, mode: i64) -> scour_core::Hit {
        scour_core::Hit {
            id: EntryId::inode(SourceId(0), 1, ino),
            path: path.into(),
            is_dir: false,
            kind,
            meta: Meta {
                mode,
                size: 10,
                ..Meta::UNKNOWN
            },
            under: None,
            found: Vec::new(),
        }
    }

    #[test]
    fn links_repeats_and_media_are_settled_before_reading() {
        let rows = [
            row(1, "/a/note.txt", Kind::File, 0o100644),
            row(1, "/a/again.txt", Kind::File, 0o100644),
            row(2, "/a/link.txt", Kind::File, 0o120777),
            row(3, "/a/film.mkv", Kind::Video, 0o100644),
            row(4, "/a/disk.iso", Kind::Archive, 0o100644),
            row(5, "/a/pack.zip", Kind::Archive, 0o100644),
        ];
        let (places, files) = files_of(&rows);
        assert_eq!(places, [0, 3, 4, 5]);
        let classes: Vec<_> = files.iter().map(|f| f.class).collect();
        assert_eq!(
            classes,
            [None, Some(Class::HeaderOnly), Some(Class::HeaderOnly), None]
        );
    }
}
