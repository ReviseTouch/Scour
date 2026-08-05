//! Answering a query by walking rows.
//!
//! There is no posting list here. A search starts at row zero and walks
//! forward, and the reason that is not absurd is the row order: rows are
//! stored newest-first, so the default view — the newest forty — stops after
//! forty matches. On the common query the walk never gets past the first
//! page's worth of rows.
//!
//! When it *cannot* stop early — sorting by size, say — it walks everything.
//! That is a linear pass over memory-mapped columns at gigabytes a second,
//! and the arithmetic says single-digit milliseconds at a million entries.
//!
//! ## Cheap first
//!
//! A query is a list of conditions ANDed together, so they can be tested in
//! any order, and the order matters enormously. Reading `size` from a column
//! is a few nanoseconds; folding a name and searching it is a hundred; joining
//! the directory path to the name and searching *that* is more. So conditions
//! are sorted by what they cost and the walk short-circuits on the first
//! failure. A query like `ext:rs size:>1mb rapor` rejects almost every row on
//! the size column and never touches its name.
//!
//! ## What it gives up
//!
//! Nothing that matters here, and one thing that does not: without postings,
//! a query that matches very few rows out of ten million still walks all ten
//! million when it cannot terminate early. That is the case a trigram layer
//! would fix, and it can be added without changing any of these files.

use memchr::memmem::Finder;
use scour_core::{Ast, Cmp, Entry, EntryId, Hit, Key, Kind, Match, Meta, SortKey, SourceId};

use crate::columns::{BLOCK, ColumnBlocks, Field};
use crate::dirs::{DirScope, DirTable};
use crate::names::{Folded, NameArena};
use crate::trigram::TrigramIndex;

/// The files a search reads, opened together.
#[derive(Debug, Clone, Copy)]
pub struct Segment<'a> {
    /// Names as the filesystem spells them. Read for the rows that are shown.
    pub names: NameArena<'a>,
    /// The same names, folded when the segment was written. **This is what a
    /// search walks**, and the reason it exists is measured: folding at query
    /// time was three quarters of the inner loop.
    pub folded: NameArena<'a>,
    pub cols: ColumnBlocks<'a>,
    pub dirs: DirTable<'a>,
    pub tri: TrigramIndex<'a>,
    /// One bit a row, set when the row is still live.
    pub alive: &'a [u8],
}

impl<'a> Segment<'a> {
    pub fn rows(&self) -> usize {
        self.cols.rows().min(self.names.rows())
    }

    /// Is any row of this block still live?
    ///
    /// Sixteen bytes of the bitmap, read once instead of a hundred and
    /// twenty-eight times. A segment that a sweep emptied is otherwise walked
    /// in full on every query until something rebuilds it — which is exactly
    /// what a real index was found doing, 1,204,270 rows a query to return
    /// nothing.
    pub fn block_alive(&self, block: usize) -> bool {
        let from = block * BLOCK / 8;
        let to = (from + BLOCK / 8).min(self.alive.len());
        match self.alive.get(from..to) {
            Some(b) => b.iter().any(|&x| x != 0),
            // An absent bitmap means nothing has been deleted yet.
            None => self.alive.is_empty(),
        }
    }

    pub fn is_alive(&self, row: usize) -> bool {
        match self.alive.get(row / 8) {
            Some(byte) => byte & (1 << (row % 8)) != 0,
            // An absent bitmap means nothing has been deleted yet.
            None => self.alive.is_empty(),
        }
    }

    fn num(&self, field: Field, row: usize) -> i64 {
        self.cols.get(field, row).unwrap_or(0)
    }

    /// One column of one row, for callers outside the walk — faceting, stats.
    pub fn num_of(&self, field: Field, row: usize) -> i64 {
        self.num(field, row)
    }

    /// Which directory a row lives in, as a number into [`Segment::dirs`].
    pub fn dir_id(&self, row: usize) -> u32 {
        self.num(Field::DirId, row) as u32
    }

    /// The full path of a row: its directory joined to its name.
    pub fn path(&self, row: usize, name: &str) -> String {
        let dir = self
            .dirs
            .get(self.num(Field::DirId, row) as u32)
            .unwrap_or_default();
        match dir.as_str() {
            "" => name.to_owned(),
            "/" => format!("/{name}"),
            d => format!("{d}/{name}"),
        }
    }

    pub fn entry_id(&self, row: usize) -> EntryId {
        let source = SourceId(self.num(Field::Source, row) as u32);
        let a = self.num(Field::KeyA, row);
        let b = self.num(Field::KeyB, row);
        match self.num(Field::KeyKind, row) {
            1 => EntryId {
                source,
                key: Key::Inode {
                    dev: a as u64,
                    ino: b as u64,
                },
            },
            _ => EntryId {
                source,
                key: Key::PathHash(a as u64),
            },
        }
    }

    fn hit(&self, row: usize, name: &str) -> Hit {
        let path = self.path(row, name);
        Hit {
            id: self.entry_id(row),
            is_dir: self.num(Field::IsDir, row) != 0,
            kind: Kind::from_u8(self.num(Field::Kind, row) as u8).unwrap_or(Kind::File),
            meta: Meta {
                size: self.num(Field::Size, row),
                mtime: self.num(Field::Mtime, row),
                ctime: self.num(Field::Ctime, row),
                atime: self.num(Field::Atime, row),
                mode: self.num(Field::Mode, row),
                uid: self.num(Field::Uid, row),
                gid: self.num(Field::Gid, row),
                disk: self.num(Field::Disk, row),
                items: self.num(Field::Items, row),
            },
            path,
        }
    }

    /// Reconstruct a whole entry, for rebuilding or merging.
    pub fn entry(&self, row: usize) -> Option<Entry> {
        let name = self.names.get(row)?;
        let h = self.hit(row, name);
        Some(Entry {
            id: h.id,
            path: h.path,
            is_dir: h.is_dir,
            meta: h.meta,
        })
    }
}

/// One condition, compiled against this segment.
#[derive(Debug)]
enum Test {
    /// A column compared to a number. The cheapest thing there is.
    Num {
        field: Field,
        cmp: Cmp,
        value: i64,
        span: i64,
    },
    /// The directory number falls in this scope: `under:` and `parent:`, both
    /// resolved once by the directory table.
    DirIn(DirScope),
    /// The kind column is any one of these.
    ///
    /// A bitset over the discriminants rather than a list, because one word
    /// can name four kinds and the test has to stay a shift and a mask however
    /// many it named.
    KindIn(u16),
    /// The name contains this, case-folded.
    ///
    /// A prebuilt searcher rather than the string, because `str::contains`
    /// constructs a Two-Way searcher on every call and this is called once a
    /// row. On a term that matches almost nothing — which is what a user types
    /// when they are looking for one file — the walk cannot stop early and
    /// that per-row construction is most of the query.
    NameHas(Needle),
    /// The name matches this wildcard pattern, anchored end to end.
    NameGlob(String),
    /// The extension, taken from the name, is one of these.
    Ext(Vec<String>),
    /// The whole path contains this, case-folded. The most expensive test.
    PathHas(Needle),
    /// Matches nothing. What an impossible condition compiles to — an `under:`
    /// naming a directory that is not in the table, say.
    Never,
}

impl Test {
    /// Roughly what this costs to evaluate, in the units that matter: whether
    /// it reads a column, folds a name, or builds a path.
    fn cost(&self) -> u32 {
        match self {
            Test::Never => 0,
            Test::Num { .. } | Test::DirIn(_) | Test::KindIn(_) => 1,
            Test::Ext(_) => 3,
            Test::NameHas(_) | Test::NameGlob(_) => 10,
            Test::PathHas(_) => 30,
        }
    }
}

/// A needle the query owns, prepared once.
///
/// Two decisions, both measured on a real home directory of 1,197,474 entries
/// where a term matching fifteen files cannot stop early and so pays for every
/// row.
///
/// The searcher is built once rather than per row: `str::contains` constructs a
/// Two-Way searcher on every call.
///
/// And the row is folded into a buffer before being searched, rather than
/// compared in place. Comparing in place looks cheaper — no copy — but it
/// gives up SIMD on both halves: the fold becomes a byte loop instead of
/// `make_ascii_lowercase`, and the search becomes a hand-written scan instead
/// of `memmem`. That version was written, measured at exactly no improvement,
/// and removed.
#[derive(Debug)]
pub struct Needle {
    finder: Finder<'static>,
}

impl Needle {
    fn new(needle: &str) -> Needle {
        Needle {
            finder: Finder::new(needle.as_bytes()).into_owned(),
        }
    }

    /// Is the needle in this **already folded** text?
    fn found_in(&self, folded: &[u8]) -> bool {
        self.finder.find(folded).is_some()
    }

    /// The same, on text that still has to be folded — for the path, which is
    /// built at query time and so cannot have been folded in advance.
    fn found_in_raw(&self, hay: &[u8], fold: &mut Folded) -> bool {
        self.finder.find(fold.fold_bytes(hay)).is_some()
    }

    /// The needle as the parser folded it — what the trigram index is keyed on.
    fn folded(&self) -> &[u8] {
        self.finder.needle()
    }
}

/// How well a name answers the terms that were typed.
///
/// Everything the ordinary sorts do is a property of the file — its date, its
/// size, its spelling. This is the only one that is a property of the *query*,
/// and the reason it exists is what a default of "newest first" does to a
/// common word: searching this machine for `main` returns a log file, four git
/// refs and two generated `main_window.rs` before the `main.rs` anyone was
/// looking for.
///
/// The weights are ordered, not tuned. Each rung is worth more than everything
/// below it can add up to, so the comparison never turns into arithmetic
/// nobody can predict:
///
/// * the name without its extension is the term — `main.rs` for `main`
/// * the whole name is the term — a file or folder called exactly `main`
/// * the name starts with it — `main_window.rs`
/// * it starts a word inside the name — `my-main.rs`, but not `domain.rs`
/// * anything else that matched at all
///
/// The first two are in that order because of what the alternative did.
/// Ranking the exact name highest was tried first and it is what a scorer
/// "should" do; on this machine it filled the page with `.git/refs/heads/main`
/// and, once those were excluded, with a hundred `android/src/main`
/// directories. Nobody typing `main` wants either. A stem match means someone
/// named a *file* after the thing being searched for, which is a much stronger
/// signal than a directory happening to carry the word.
///
/// Then shorter names first, because a name that is mostly the term is more
/// about the term, and further from home last. Ties fall through to the stored
/// order, which is by date.
///
/// `steps` is [`crate::dirs::steps_of`] for the row's directory — how far it is
/// from being something the user wrote. It is what a name cannot say. Ranking
/// `main` by name alone put a CMake test file under `target/debug/build` first
/// and filled the page from `~/.pub-cache` and `~/.rustup`; of the first two
/// hundred results, 88 were package caches and SDKs against 48 of the user's
/// own work.
///
/// It is bounded so that it can only ever order rows that the name already
/// ties: sixty steps at [`STEP`] each is 480, a long name can cost 255, and
/// the narrowest gap between two rungs is 900. A file in a cache still beats a
/// file whose name answers the query better, every time.
fn relevance(name: &[u8], terms: &[Vec<u8>], steps: u8) -> i64 {
    if terms.is_empty() {
        return 0;
    }
    let stem_end = name
        .iter()
        .rposition(|&b| b == b'.')
        .filter(|&i| i > 0)
        .unwrap_or(name.len());
    let mut best = 0i64;
    for t in terms {
        if t.is_empty() {
            continue;
        }
        let Some(at) = find(name, t) else { continue };
        let score = if stem_end < name.len() && name[..stem_end] == t[..] {
            // `main` in `main.rs`: someone named a file after this.
            4000
        } else if name == &t[..] {
            3000
        } else if at == 0 {
            2000
        } else if !name[at - 1].is_ascii_alphanumeric() {
            1000
        } else {
            100
        };
        best = best.max(score);
    }
    // Up to 255 characters of name and 480 of distance work against it, and
    // neither can overturn a rung: the gap between rungs is 900 at its
    // narrowest and 255 + 480 is 735.
    best - (name.len().min(255) as i64) - i64::from(steps) * STEP
}

/// What one step away from home costs.
///
/// Eight, so that sixty steps — the most the table records — stay under the
/// gap between two rungs of the name score even after a long name has taken
/// its 255.
const STEP: i64 = 8;

/// Where `needle` first appears in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    memchr::memmem::find(haystack, needle)
}

/// The extension of a name, as bytes, by the same rule as `scour_core`.
fn ext_bytes(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|&b| b == b'.') {
        Some(i) if i > 0 && i + 1 < name.len() && name.len() - i - 1 <= 12 => &name[i + 1..],
        _ => b"",
    }
}

/// Can `cmp value` hold for any number in `[lo, hi]`?
fn admits(cmp: Cmp, value: i64, span: i64, lo: i64, hi: i64) -> bool {
    match cmp {
        // A dated `=` is a half-open window, not a point.
        Cmp::Eq if span > 1 => value <= hi && value.saturating_add(span) > lo,
        Cmp::Eq => lo <= value && value <= hi,
        Cmp::Lt => lo < value,
        Cmp::Le => lo <= value,
        Cmp::Gt => hi > value,
        Cmp::Ge => hi >= value,
    }
}

/// The blocks a search has to look at, if the trigram index can say.
///
/// Only a clause that is one plain `NameHas` counts. An OR would need the union
/// of its alternatives and a negation inverts the question, and neither is
/// worth the risk here: getting this wrong in the narrowing direction loses
/// files silently. Several such clauses intersect, because every one of them
/// has to hold.
fn narrow_to_blocks(clauses: &[Clause], seg: &Segment<'_>) -> Option<Vec<u32>> {
    // `SCOUR_NO_TRIGRAM=1` turns the filter off, and it stays because the
    // filter was twice claimed — in this file's own notes — not to pay for
    // itself, and both times the claim was wrong. Measured on 2,137,518
    // entries with the switch, one segment, twenty rounds each:
    //
    //   rapor      14.11 ms    against  37.97 ms
    //   belge       9.22               37.70
    //   main        5.62               36.23
    //   fatura      0.85               35.02
    //   kütüphane   0.06               35.50
    //
    // Between 2.7× and 590×. What made the earlier arithmetic wrong was
    // comparing against a sequential scan measured *before* names were stored
    // folded, and then against a busy index with five segments and a watcher
    // running. A switch is cheaper than either mistake.
    if std::env::var_os("SCOUR_NO_TRIGRAM").is_some() {
        return None;
    }
    let mut out: Option<Vec<u32>> = None;
    for c in clauses {
        let [(false, test)] = &c.alts[..] else {
            continue;
        };
        let Some(needle) = filterable(test) else {
            continue;
        };
        let Some(blocks) = seg.tri.candidates(&needle) else {
            continue;
        };
        out = Some(match out {
            None => blocks,
            Some(have) => intersect_blocks(&have, &blocks),
        });
    }
    out
}

/// A string every matching name must contain, if there is one.
///
/// The only thing being claimed is containment, and each of these is a
/// straightforward consequence of what the test means. Anything less certain
/// does not belong here: over-narrowing loses files and reports nothing.
fn filterable(test: &Test) -> Option<Vec<u8>> {
    match test {
        Test::NameHas(n) => Some(n.folded().to_vec()),
        // A name with extension `pdf` contains `.pdf`, because an extension is
        // only an extension when something precedes the dot. One extension
        // only: a list would need the union of its lists, not the intersection.
        Test::Ext(list) => match &list[..] {
            [only] => Some(format!(".{only}").into_bytes()),
            _ => None,
        },
        // A name matching `rap*or` contains `rap` and contains `or`. The
        // longest run is the most selective of them.
        Test::NameGlob(p) => p
            .split(['*', '?'])
            .max_by_key(|run| run.len())
            .filter(|run| run.len() >= 3)
            .map(|run| run.as_bytes().to_vec()),
        _ => None,
    }
}

fn intersect_blocks(a: &[u32], b: &[u32]) -> Vec<u32> {
    let mut out = Vec::with_capacity(a.len().min(b.len()));
    let (mut i, mut j) = (0usize, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                out.push(a[i]);
                i += 1;
                j += 1;
            }
        }
    }
    out
}

/// Alternatives ORed together; `bool` is negation.
#[derive(Debug)]
struct Clause {
    alts: Vec<(bool, Test)>,
}

/// A query, compiled.
#[derive(Debug, Default)]
pub struct Plan {
    clauses: Vec<Clause>,
    /// Blocks the walk has to visit, when the trigram index could say.
    ///
    /// `None` is "all of them", which is what every query did before this
    /// existed and what a query with no usable trigram still does.
    candidates: Option<Vec<u32>>,
}

impl Plan {
    /// Compile an [`Ast`] against a segment.
    ///
    /// Everything that can be resolved once is resolved here — a `under:` path
    /// becomes a range of directory numbers, a date becomes a bound — so that
    /// the per-row work is comparisons and nothing else.
    pub fn compile(ast: &Ast, seg: &Segment<'_>) -> Result<Plan, scour_core::Error> {
        let mut clauses = Vec::with_capacity(ast.groups.len());
        for group in &ast.groups {
            let mut alts = Vec::with_capacity(group.alts.len());
            for (negated, m) in &group.alts {
                alts.push((*negated, compile_match(m, seg)?));
            }
            clauses.push(Clause { alts });
        }
        // Cheapest clause first. A clause is only as cheap as its dearest
        // alternative, because an OR has to try them until one succeeds.
        clauses.sort_by_key(|c| c.alts.iter().map(|(_, t)| t.cost()).max().unwrap_or(0));
        let candidates = narrow_to_blocks(&clauses, seg);
        Ok(Plan {
            clauses,
            candidates,
        })
    }

    pub fn is_empty(&self) -> bool {
        self.clauses.is_empty()
    }

    /// The blocks the filter chose, for measurement.
    pub fn candidate_blocks(&self) -> Option<&[u32]> {
        self.candidates.as_deref()
    }

    /// Could any row of this block match?
    ///
    /// Two comparisons against the range the block holds, which is stored and
    /// needs no decoding. It can only reject: a block whose range admits the
    /// filter has every row tested exactly as before, so this turns misses into
    /// skips and never a match into a miss.
    fn block_possible(&self, seg: &Segment<'_>, block: usize) -> bool {
        for c in &self.clauses {
            let [(false, test)] = &c.alts[..] else {
                continue;
            };
            let admits = match test {
                Test::Never => false,
                Test::Num {
                    field,
                    cmp,
                    value,
                    span,
                } => match seg.cols.block_range(*field, block) {
                    Some((lo, hi)) => admits(*cmp, *value, *span, lo, hi),
                    None => true,
                },
                Test::DirIn(scope) => match seg.cols.block_range(Field::DirId, block) {
                    Some((lo, hi)) if lo >= 0 => scope.intersects(lo as u32, hi as u32),
                    _ => true,
                },
                // The same zone map, read as a range of discriminants: a block
                // holding only kinds 10 and 11 cannot answer `kind:image`.
                Test::KindIn(mask) => match seg.cols.block_range(Field::Kind, block) {
                    Some((lo, hi)) if (0..16).contains(&lo) && (0..16).contains(&hi) => {
                        let span = ((1u32 << (hi - lo + 1)) - 1) << lo;
                        u32::from(*mask) & span != 0
                    }
                    _ => true,
                },
                _ => true,
            };
            if !admits {
                return false;
            }
        }
        true
    }

    /// Does answering this require reading names at all?
    ///
    /// `kind:image size:>10mb` does not, and the difference is the whole inner
    /// loop: without this the walk reads every name in the index — a `memchr`
    /// and twenty bytes of memory traffic a row — to hand it to tests that
    /// never look at it.
    fn needs_name(&self) -> bool {
        self.clauses.iter().flat_map(|c| &c.alts).any(|(_, t)| {
            matches!(
                t,
                Test::NameHas(_) | Test::NameGlob(_) | Test::Ext(_) | Test::PathHas(_)
            )
        })
    }

    /// Does this row match?
    ///
    /// `name` is passed in because the caller already has it — the walk reads
    /// names sequentially, which is the whole reason the arena has no offsets.
    /// `name` is the row's name **already folded** — what the walk yields.
    pub fn accepts(&self, seg: &Segment<'_>, row: usize, name: &[u8], fold: &mut Folded) -> bool {
        for clause in &self.clauses {
            let mut any = false;
            for (negated, test) in &clause.alts {
                if evaluate(test, seg, row, name, fold) != *negated {
                    any = true;
                    break;
                }
            }
            if !any {
                return false;
            }
        }
        true
    }
}

fn compile_match(m: &Match, seg: &Segment<'_>) -> Result<Test, scour_core::Error> {
    use scour_core::TimeField;
    Ok(match m {
        Match::NameContains(t) => Test::NameHas(Needle::new(t)),
        // `*.rs` is the overwhelmingly common wildcard, and it is exactly an
        // extension test — which reads one short string instead of running a
        // pattern matcher over the whole name. Measured at 175 ms against 47.
        Match::NameGlob(p) => match p.strip_prefix("*.") {
            Some(ext) if !ext.is_empty() && !ext.contains(['*', '?', '.']) => {
                Test::Ext(vec![ext.to_owned()])
            }
            _ => Test::NameGlob(p.clone()),
        },
        Match::PathContains(t) => Test::PathHas(Needle::new(t)),
        Match::Ext(list) => Test::Ext(list.clone()),
        Match::IsDir(want) => Test::Num {
            field: Field::IsDir,
            cmp: Cmp::Eq,
            value: i64::from(*want),
            span: 1,
        },
        Match::Kind(k) => Test::KindIn(k.iter().fold(0u16, |m, k| m | 1 << k.as_u8())),
        Match::Size(cmp, v) => Test::Num {
            field: Field::Size,
            cmp: *cmp,
            value: *v,
            span: 1,
        },
        Match::Time(f, cmp, v) => Test::Num {
            field: match f {
                TimeField::Modified => Field::Mtime,
                TimeField::Created => Field::Ctime,
                TimeField::Accessed => Field::Atime,
            },
            cmp: *cmp,
            value: *v,
            // Equality on a timestamp means the calendar day it names.
            span: 86_400,
        },
        // Both resolve to a range of directory numbers, once, here. This is
        // what makes scoping to a folder a comparison rather than a scan —
        // and it is the same range a subtree delete uses.
        // Both resolve to a set of directory numbers. `under:` matches a row
        // whose directory is the named one *or* any beneath it — a file
        // sitting directly in the folder is under it. The folder's own row is
        // not, because its directory is its parent.
        Match::Under(d) => {
            let scope = seg.dirs.subtree(d);
            if scope.is_empty() {
                Test::Never
            } else {
                Test::DirIn(scope)
            }
        }
        Match::ParentIs(d) => match seg.dirs.exact(d) {
            Some(id) => Test::DirIn(DirScope {
                own: Some(id),
                below: id..id,
            }),
            None => Test::Never,
        },
        Match::ContentContains(_) => return Err(scour_core::Error::ContentNotIndexed),
    })
}

/// Test one condition against one row.
///
/// `name` arrives **folded**, out of the second arena, so nothing here folds
/// it again — that fold was three quarters of the inner loop and now happens
/// once when the segment is written. `fold` survives for the one thing that
/// cannot be prepared in advance: the path, which is built here.
fn evaluate(test: &Test, seg: &Segment<'_>, row: usize, name: &[u8], fold: &mut Folded) -> bool {
    match test {
        Test::Never => false,
        Test::Num {
            field,
            cmp,
            value,
            span,
        } => {
            let got = seg.num(*field, row);
            match cmp {
                Cmp::Eq if *span > 1 => got >= *value && got < value + span,
                c => c.holds(got, *value),
            }
        }
        Test::DirIn(scope) => scope.contains(seg.num(Field::DirId, row) as u32),
        Test::KindIn(mask) => {
            let k = seg.num(Field::Kind, row);
            (0..16).contains(&k) && mask & 1 << k != 0
        }
        Test::Ext(list) => {
            // The extension of a folded name is a folded extension, so this
            // is a comparison and nothing else.
            let ext = ext_bytes(name);
            !ext.is_empty() && list.iter().any(|e| e.as_bytes() == ext)
        }
        Test::NameHas(n) => n.found_in(name),
        Test::NameGlob(p) => match std::str::from_utf8(name) {
            Ok(name) => scour_query::glob_matches(p, name),
            Err(_) => false,
        },
        Test::PathHas(n) => {
            // The dearest test, and the reason it is sorted last: it builds a
            // string. Everything else reads what is already there.
            // The *spelled* name, because a path is shown as well as matched,
            // and folded here because it was built here.
            let raw = seg.names.get(row).unwrap_or_default();
            let path = seg.path(row, raw);
            n.found_in_raw(path.as_bytes(), fold)
        }
    }
}

/// What a search asks for, and what it is allowed to spend.
///
/// `count_cap` bounds the walk **only for the stored order**. Every other
/// order has to visit every match before it can name the top forty, so there
/// the cap bounds the reported total and nothing else. That asymmetry is the
/// honest shape of the trade and is why [`Found::early_exit`] is reported
/// rather than assumed.
#[derive(Debug, Clone, Copy)]
pub struct Wanted {
    pub sort: SortKey,
    pub descending: bool,
    pub offset: usize,
    pub limit: usize,
    /// Stop counting matches here.
    pub count_cap: usize,
}

#[derive(Debug, Default)]
pub struct Found {
    pub hits: Vec<Hit>,
    pub total: u64,
    pub capped: bool,
    /// Whether the walk was able to stop early.
    pub early_exit: bool,
    pub rows_visited: u64,
    /// Rows built into a `Hit` — the expensive part, since each reconstructs a
    /// front-coded path. Reported because "why is this sort slow" is otherwise
    /// a guess between the walk, the selection and this.
    pub rows_built: u64,
}

/// Walk the segment and answer.
pub fn run(seg: &Segment<'_>, plan: &Plan, want: Wanted) -> Found {
    run_with(seg, plan, want, None)
}

/// The same walk, with a veto over rows that match but must not be shown.
///
/// This exists for exactly one thing: a removal has to disappear from searches
/// the moment it is applied, but erasing it from a segment is work saved for
/// the next commit. Between those two moments the row is still in the file and
/// still matches, and something has to say so.
///
/// The veto runs *after* the query has accepted a row, not before, because it
/// is the dearer of the two — it reconstructs the row's identity or its path —
/// and on any real query the filters have already rejected almost everything.
///
/// `None` rather than a closure that always says no, because the difference is
/// visible: with no veto and no test that reads names, the walk never touches
/// the name arena.
pub fn run_with(
    seg: &Segment<'_>,
    plan: &Plan,
    want: Wanted,
    mut conceals: Option<&mut dyn FnMut(&Segment<'_>, usize, &[u8]) -> bool>,
) -> Found {
    let has_veto = conceals.is_some();
    let mut fold = Folded::new();
    let mut counted = 0usize;
    let mut visited = 0u64;
    // Row numbers, not rows: four bytes each until the page is decided, which
    // is what keeps a query matching a million entries from building a million
    // strings.
    let mut kept: Vec<u32> = Vec::new();
    // The same idea for the orders that have to see everything: a sort value
    // and a row number, never a row. Materialising each match to sort it was
    // measured at 16.5 ms where this measures a fraction of it — the cost is
    // not the comparison, it is reconstructing a front-coded path per match to
    // then throw all but forty of them away.
    let mut keyed: Vec<(SortValue, u32)> = Vec::new();

    // The terms relevance scores against, folded, collected once. `filterable`
    // already answers "what must every matching name contain", which is the
    // same question — a term that cannot be claimed as containment cannot be
    // scored against either.
    let score_terms: Vec<Vec<u8>> = if want.sort == SortKey::Relevance {
        plan.clauses
            .iter()
            .flat_map(|c| &c.alts)
            .filter(|(neg, _)| !neg)
            .filter_map(|(_, t)| filterable(t))
            .collect()
    } else {
        Vec::new()
    };

    // The one order the row layout already satisfies. Everything else has to
    // see every match before it knows which forty win.
    //
    // Relevance with nothing to score against is that same order and not a
    // coincidence: every row gets the same number, so the walk would visit
    // three million of them to hand back a page the layout was already
    // holding. It is not a corner case either — it is what a search window
    // shows the instant it opens, and it cost 121 ms of full scan before this
    // line existed, on the one frame a person is actually watching for.
    let stored_order = (want.sort == SortKey::Modified && want.descending)
        || (want.sort == SortKey::Relevance && want.descending && score_terms.is_empty());
    let need = want.offset + want.limit;
    let mut done = false;

    let mut visit = |row: usize, name: &[u8]| -> bool {
        visited += 1;
        if !seg.is_alive(row) || !plan.accepts(seg, row, name, &mut fold) {
            return true;
        }
        if let Some(veto) = conceals.as_deref_mut()
            && veto(seg, row, name)
        {
            return true;
        }
        counted += 1;
        if stored_order {
            if kept.len() < need {
                kept.push(row as u32);
            }
            // Both conditions must hold: enough rows for the page, and enough
            // counted for the total to be honest. Stopping on the first alone
            // is what made the engine this replaces report a page and a lie.
            if kept.len() >= need && counted >= want.count_cap {
                done = true;
                return false;
            }
        } else {
            // Any other order has to see every match before it knows which
            // forty win, so the count cap must not stop the walk here.
            //
            // It did, briefly, and the result was quietly wrong: "the largest
            // forty" became "the largest forty among the five hundred newest",
            // which is a plausible-looking answer to a different question. The
            // verifier missed it because the test asked for an uncapped count,
            // where the two happen to agree.
            keyed.push((
                sort_value(seg, row, name, want.sort, &score_terms),
                row as u32,
            ));
        }
        true
    };

    // Which blocks are worth opening at all. Two filters, and both can only
    // remove: the trigram index says which blocks could contain the text, and
    // the zone map says which could satisfy the numbers. What survives is
    // walked exactly as it always was.
    let blocks = blocks_worth_opening(seg, plan);

    // Nothing here reads a name, so nothing here reads the arena.
    //
    // The *sort* has to be asked too, and forgetting to was a real bug: the
    // row-driven path hands `accepts` an empty name, which is correct when no
    // test reads one — but `sort_value` was reading it as well, so every row
    // sorted by name got the same key. The answer stayed right, because
    // everything then tied and `sort_hits` compared the real names, and the
    // cost was the whole corpus: 1,117,687 rows built to return forty.
    let sort_reads_name =
        !stored_order && matches!(want.sort, SortKey::Name | SortKey::Ext | SortKey::Path);
    let by_row = !plan.needs_name() && !has_veto && !sort_reads_name;
    let mut i = 0usize;
    'runs: while i < blocks.len() {
        // Adjacent blocks are walked as one, so a dense set costs no more
        // seeking than a full walk would.
        let mut j = i;
        while j + 1 < blocks.len() && blocks[j + 1] == blocks[j] + 1 {
            j += 1;
        }
        let from = blocks[i] as usize * BLOCK;
        let to = ((blocks[j] as usize + 1) * BLOCK).min(seg.rows());
        if by_row {
            for row in from..to {
                if !visit(row, b"") {
                    break 'runs;
                }
            }
        } else {
            let mut go = true;
            // The **folded** arena: a search matches folded text, and folding
            // it here instead was 24.4 ns of the 40.5 a row used to cost.
            seg.folded.walk_range(from, to, |row, name| {
                go = visit(row, name);
                go
            });
            if !go {
                break 'runs;
            }
        }
        i = j + 1;
    }

    if !stored_order {
        kept = narrow(&mut keyed, need, want.descending, key_is_exact(want.sort));
    }

    // Materialise. Only now, and only what can appear: reading a row means
    // building its path, which is the expensive part of the whole operation.
    let rows_built = kept.len() as u64;
    let mut hits: Vec<Hit> = kept
        .into_iter()
        .filter_map(|row| {
            let row = row as usize;
            seg.names.get(row).map(|n| seg.hit(row, n))
        })
        .collect();
    if !stored_order {
        // Within one segment the rows already carry their score in `keyed`;
        // this path is the one that did not sort, so it scores from scratch.
        let owned: Vec<String> = score_terms
            .iter()
            .map(|t| String::from_utf8_lossy(t).into_owned())
            .collect();
        let terms: Vec<&str> = owned.iter().map(String::as_str).collect();
        sort_hits(&mut hits, want.sort, want.descending, &terms);
    }
    let hits: Vec<Hit> = hits
        .into_iter()
        .skip(want.offset)
        .take(want.limit)
        .collect();

    Found {
        hits,
        total: counted.min(want.count_cap) as u64,
        capped: counted >= want.count_cap,
        early_exit: stored_order && done,
        rows_visited: visited,
        rows_built,
    }
}

/// What a row sorts by, without its row being built.
///
/// Three shapes, and the middle one is the interesting one. Sorting a million
/// rows by name used to allocate a million short `Vec`s — 1,666 ms on a real
/// index. `Head` is the first eight bytes of the folded name packed
/// big-endian into a `u64`, which orders identically to the bytes it came
/// from and costs nothing: rows that tie on it are the few that share eight
/// bytes of name, and only those are compared properly.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum SortValue {
    Num(i64),
    /// The first sixteen bytes, big-endian. Exact for an extension, which is
    /// at most twelve bytes by definition; abbreviated for a name, where
    /// equality means "might be equal" and the boundary group has to be kept
    /// and compared for real.
    Head(u128),
    Text(Vec<u8>),
}

/// The first sixteen bytes, big-endian, so that comparing the numbers is
/// comparing the bytes. Shorter input is padded with zeroes, which sorts before
/// anything — the same as a shorter string.
fn head(bytes: &[u8]) -> u128 {
    let mut v = [0u8; 16];
    let n = bytes.len().min(16);
    v[..n].copy_from_slice(&bytes[..n]);
    u128::from_be_bytes(v)
}

/// Is this key exact, or only the beginning of one?
///
/// Only the name is abbreviated. An extension is at most twelve bytes — that
/// is what makes it an extension — so sixteen holds all of it, and sorting
/// `ext:rs` by extension stops being a single tie group of seventy thousand.
fn key_is_exact(key: SortKey) -> bool {
    key != SortKey::Name
}

/// `name` is the row's folded name, as the walk yields it.
fn sort_value(
    seg: &Segment<'_>,
    row: usize,
    name: &[u8],
    key: SortKey,
    terms: &[Vec<u8>],
) -> SortValue {
    match key {
        // Already folded, which is what the terms are, plus the directory's
        // recorded distance — one byte read.
        SortKey::Relevance => {
            SortValue::Num(relevance(name, terms, seg.dirs.steps(seg.dir_id(row))))
        }
        SortKey::Name => SortValue::Head(head(name)),
        SortKey::Ext => SortValue::Head(head(ext_bytes(name))),
        SortKey::Path => {
            let raw = seg.names.get(row).unwrap_or_default();
            SortValue::Text(seg.path(row, raw).into_bytes())
        }
        SortKey::Size => SortValue::Num(seg.num(Field::Size, row)),
        SortKey::Modified => SortValue::Num(seg.num(Field::Mtime, row)),
        SortKey::Created => SortValue::Num(seg.num(Field::Ctime, row)),
        SortKey::Accessed => SortValue::Num(seg.num(Field::Atime, row)),
        SortKey::Kind => SortValue::Num(seg.num(Field::Kind, row)),
        SortKey::Items => SortValue::Num(seg.num(Field::Items, row)),
        SortKey::Mode => SortValue::Num(seg.num(Field::Mode, row)),
        SortKey::Uid => SortValue::Num(seg.num(Field::Uid, row)),
        SortKey::Gid => SortValue::Num(seg.num(Field::Gid, row)),
        SortKey::Disk => SortValue::Num(seg.num(Field::Disk, row)),
    }
}

/// The rows that can still reach the page, given only their sort values.
///
/// The whole tie group at the boundary comes too, and that is not a nicety: the
/// final order breaks ties on the path, so a row tied with the last one may
/// still displace it. Cutting at exactly `need` would return a page that is
/// deterministic, plausible, and not the one brute force produces — timestamps
/// tie in the thousands on a real filesystem.
fn narrow(keyed: &mut [(SortValue, u32)], need: usize, desc: bool, exact: bool) -> Vec<u32> {
    if need == 0 || keyed.is_empty() {
        return Vec::new();
    }
    if keyed.len() <= need {
        return keyed.iter().map(|(_, row)| *row).collect();
    }
    // The row number is the second key, and it is not a formality: rows are
    // stored newest-first with the path breaking *that*, so ordering ties by
    // row is ordering them the way the final sort will. It also makes the
    // comparison total, which is what removes the tie group.
    //
    // Removing it matters. Sorting by `kind` puts a hundred thousand rows at
    // the same value, and keeping that whole group — which breaking ties on
    // the path required — cost 73 ms where this costs four.
    let cmp = |a: &(SortValue, u32), b: &(SortValue, u32)| {
        if desc { b.0.cmp(&a.0) } else { a.0.cmp(&b.0) }.then(a.1.cmp(&b.1))
    };
    // Selection, not a sort: finding which forty win out of two hundred
    // thousand does not require ordering the rest, and `sort_hits` orders the
    // survivors anyway.
    let k = need - 1;
    keyed.select_nth_unstable_by(k, cmp);
    let (top, rest) = keyed.split_at(need);
    let mut out: Vec<u32> = top.iter().map(|(_, row)| *row).collect();
    if !exact {
        // An abbreviated key only says the first eight bytes agree. The rows
        // that share them still have to be compared properly, and there are
        // few of them.
        let boundary = &top[k].0;
        out.extend(rest.iter().filter(|(v, _)| v == boundary).map(|(_, r)| *r));
    }
    out
}

/// Deterministic ordering, with an explicit tie-break on the path.
///
/// Timestamps tie constantly — a package install stamps thousands of files at
/// one instant — so without a second key the same query returns a different
/// page each time.
/// Which blocks are worth opening at all.
///
/// Two filters, and both can only remove: the trigram index says which blocks
/// could contain the text, and the zone map which could satisfy the numbers.
///
/// **Shared, because the two callers diverging is what went wrong.** A search
/// narrowed and a facet did not — it walked every row of every segment, so the
/// sidebar cost more than the list it describes, and the whole point of the
/// design is that a sidebar is recomputed on every keystroke.
pub(crate) fn blocks_worth_opening(seg: &Segment<'_>, plan: &Plan) -> Vec<u32> {
    let n_blocks = seg.rows().div_ceil(BLOCK);
    match &plan.candidates {
        Some(c) => c
            .iter()
            .copied()
            .filter(|&b| seg.block_alive(b as usize) && plan.block_possible(seg, b as usize))
            .collect(),
        None => (0..n_blocks as u32)
            .filter(|&b| seg.block_alive(b as usize) && plan.block_possible(seg, b as usize))
            .collect(),
    }
}

/// Every matching row of one segment, narrowed the same way a search is.
///
/// For the questions that are about the whole matching set rather than about a
/// page of it — how many, of what kind, how old. `f` is given the row; a
/// caller that wants the name reads it, because most of them do not and the
/// arena is the expensive part.
///
/// Returns false if `f` asked it to stop.
pub fn walk_matches(seg: &Segment<'_>, plan: &Plan, mut f: impl FnMut(usize) -> bool) -> bool {
    let mut fold = Folded::new();
    let blocks = blocks_worth_opening(seg, plan);
    let by_row = !plan.needs_name();
    let mut i = 0usize;
    while i < blocks.len() {
        let mut j = i;
        while j + 1 < blocks.len() && blocks[j + 1] == blocks[j] + 1 {
            j += 1;
        }
        let from = blocks[i] as usize * BLOCK;
        let to = ((blocks[j] as usize + 1) * BLOCK).min(seg.rows());
        if by_row {
            for row in from..to {
                if seg.is_alive(row) && plan.accepts(seg, row, b"", &mut fold) && !f(row) {
                    return false;
                }
            }
        } else {
            let mut go = true;
            // The **folded** arena, exactly as a search walks it: `accepts`
            // compares against folded needles, and handing it the spelled name
            // means `RAPOR.pdf` quietly does not match `rapor`.
            seg.folded.walk_range(from, to, |row, name| {
                if seg.is_alive(row) && plan.accepts(seg, row, name, &mut fold) {
                    go = f(row);
                }
                go
            });
            if !go {
                return false;
            }
        }
        i = j + 1;
    }
    true
}

pub(crate) fn sort_hits(hits: &mut [Hit], key: SortKey, desc: bool, terms: &[&str]) {
    use scour_core::text::{DefaultFolder, Folder};

    // Text keys are computed once a row, not once a comparison. Folding inside
    // the comparator costs O(n log n) folds to produce a page of forty, which
    // measured 260 ms where this measures a fraction of it.
    let mut keyed: Vec<(Option<String>, i64, Hit)> = std::mem::take(&mut hits.to_vec())
        .into_iter()
        .map(|h| {
            let k = match key {
                SortKey::Name => Some(DefaultFolder.fold(h.name())),
                SortKey::Ext => Some(scour_core::ext_of(h.name())),
                _ => None,
            };
            // Scored again here rather than carried: this merges rows from
            // several segments, each of which scored against the same terms,
            // so recomputing is cheaper than widening `Hit` to hold a number
            // no client has any use for.
            let r = if key == SortKey::Relevance {
                let folded: Vec<Vec<u8>> = terms
                    .iter()
                    .map(|t| DefaultFolder.fold(t).into_bytes())
                    .collect();
                // The distance is recomputed from the path rather than read
                // from a table, because this merges rows from several segments
                // and a `Hit` carries no directory number. Same function, same
                // answer.
                let steps = crate::dirs::steps_of(crate::dirs::dir_part(&h.path));
                relevance(DefaultFolder.fold(h.name()).as_bytes(), &folded, steps)
            } else {
                0
            };
            (k, r, h)
        })
        .collect();

    keyed.sort_unstable_by(|(ka, ra, a), (kb, rb, b)| {
        let o = match key {
            SortKey::Relevance => ra.cmp(rb),
            SortKey::Name | SortKey::Ext => ka.cmp(kb),
            SortKey::Path => a.path.cmp(&b.path),
            SortKey::Size => a.meta.size.cmp(&b.meta.size),
            SortKey::Modified => a.meta.mtime.cmp(&b.meta.mtime),
            SortKey::Created => a.meta.ctime.cmp(&b.meta.ctime),
            SortKey::Accessed => a.meta.atime.cmp(&b.meta.atime),
            SortKey::Kind => a.kind.cmp(&b.kind),
            SortKey::Items => a.meta.items.cmp(&b.meta.items),
            SortKey::Mode => a.meta.mode.cmp(&b.meta.mode),
            SortKey::Uid => a.meta.uid.cmp(&b.meta.uid),
            SortKey::Gid => a.meta.gid.cmp(&b.meta.gid),
            SortKey::Disk => a.meta.disk.cmp(&b.meta.disk),
        };
        let o = if desc { o.reverse() } else { o };
        // The stored order breaks the tie: newest first, then path. Not the
        // path alone — on a low-cardinality key like `kind` that makes the
        // whole result set one tie group, and an engine then has to look at
        // every row of it to name the first forty.
        o.then_with(|| b.meta.mtime.cmp(&a.meta.mtime))
            .then_with(|| a.path.cmp(&b.path))
    });
    for (slot, (_, _, h)) in hits.iter_mut().zip(keyed) {
        *slot = h;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_needle_finds_what_a_plain_lowercase_contains_would() {
        let mut fold = Folded::new();
        for (name, needle) in [
            ("main.rs", "main"),
            ("MAIN.RS", "main"),
            ("Rapor-2026.pdf", "rapor"),
            ("rapor", "rapor"),
            ("a", "ab"),
            ("abc", "bc"),
            ("abc", "d"),
            ("", "x"),
            ("READ[ME]", "d[m"),
            ("READ{ME}", "d[m"),
        ] {
            let n = Needle::new(needle);
            let want = name.to_ascii_lowercase().contains(needle);
            assert_eq!(
                n.found_in_raw(name.as_bytes(), &mut fold),
                want,
                "{name:?} contains {needle:?}"
            );
        }
    }

    #[test]
    fn a_non_ascii_name_still_goes_through_the_real_folding() {
        let mut fold = Folded::new();
        // The Turkish rule: the query is folded by the parser, the name here.
        assert!(Needle::new("istanbul").found_in_raw("İSTANBUL.txt".as_bytes(), &mut fold));
        assert!(Needle::new("isparta").found_in_raw("ısparta.md".as_bytes(), &mut fold));
        assert!(Needle::new("öğüt").found_in_raw("Öğüt.docx".as_bytes(), &mut fold));
        assert!(!Needle::new("zzz").found_in_raw("Öğüt.docx".as_bytes(), &mut fold));
    }

    #[test]
    fn the_byte_extension_is_the_one_the_core_defines() {
        for name in [
            "main.rs",
            "a.tar.gz",
            ".bashrc",
            "noext",
            "trailing.",
            "x.averyverylongextension",
            "UPPER.PDF",
            "İstanbul.TXT",
        ] {
            assert_eq!(
                ext_bytes(name.as_bytes()),
                scour_core::ext_str(name).as_bytes(),
                "{name:?}"
            );
        }
    }
}

#[cfg(test)]
mod relevance_tests {
    use super::relevance;

    fn score(name: &str, term: &str) -> i64 {
        at(name, term, 0)
    }

    /// The same, for a name that many steps from home.
    fn at(name: &str, term: &str, steps: u8) -> i64 {
        relevance(name.as_bytes(), &[term.as_bytes().to_vec()], steps)
    }

    #[test]
    fn a_file_named_after_the_term_beats_a_folder_that_merely_is_it() {
        // The ordering that had to be measured to be believed. Ranking the
        // exact name highest filled the page with `.git/refs/heads/main` and
        // then with a hundred `android/src/main` directories.
        assert!(score("main.rs", "main") > score("main", "main"));
        assert!(score("main", "main") > score("main_window.rs", "main"));
    }

    #[test]
    fn a_word_boundary_beats_the_middle_of_a_word() {
        assert!(score("my-main.rs", "main") > score("domain.rs", "main"));
    }

    #[test]
    fn the_shorter_of_two_equal_matches_wins() {
        assert!(score("main.rs", "main") > score("mainly-about-something.rs", "main"));
    }

    #[test]
    fn length_never_overturns_a_rung() {
        // 900 is the narrowest gap between rungs and a name is capped at 255,
        // so no amount of length can promote a weaker match.
        let long_stem = format!("{}.rs", "main");
        let short_prefix = "mainx";
        assert!(score(&long_stem, "main") > score(short_prefix, "main"));
    }

    #[test]
    fn a_query_with_no_terms_scores_everything_alike() {
        assert_eq!(relevance(b"anything.txt", &[], 40), 0);
    }

    #[test]
    fn the_nearer_of_two_equal_names_wins() {
        // `~/Projeler/x/src/main.rs` against the same name buried in a cache.
        assert!(at("main.rs", "main", 5) > at("main.rs", "main", 12));
    }

    #[test]
    fn distance_never_overturns_a_rung() {
        // The whole point of the bound, tested at the narrowest gap there is:
        // a word boundary is 1000 and the middle of a word is 100. A match on
        // the boundary, as far away as the table can record and with a name
        // long enough to take the whole length penalty, still wins.
        let far_and_long = format!("my-main{}.rs", "x".repeat(250));
        assert!(at("my-main.rs", "main", 60) > at("domain.rs", "main", 0));
        assert!(at(&far_and_long, "main", 60) > at("domain.rs", "main", 0));
    }
}
