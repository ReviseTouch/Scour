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

use crate::columns::{ColumnBlocks, Field};
use crate::dirs::{DirScope, DirTable};
use crate::names::{Folded, NameArena};
use crate::trigram::TrigramIndex;

/// The files a search reads, opened together.
#[derive(Debug, Clone, Copy)]
pub struct Segment<'a> {
    pub names: NameArena<'a>,
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
            Test::Num { .. } | Test::DirIn(_) => 1,
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

    fn found_in(&self, hay: &[u8], fold: &mut Folded) -> bool {
        self.finder.find(fold.fold_bytes(hay)).is_some()
    }

    /// The needle as the parser folded it — what the trigram index is keyed on.
    fn folded(&self) -> &[u8] {
        self.finder.needle()
    }
}

/// The extension of a name, as bytes, by the same rule as `scour_core`.
fn ext_bytes(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|&b| b == b'.') {
        Some(i) if i > 0 && i + 1 < name.len() && name.len() - i - 1 <= 12 => &name[i + 1..],
        _ => b"",
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
        Match::Kind(k) => Test::Num {
            field: Field::Kind,
            cmp: Cmp::Eq,
            value: k.as_u8() as i64,
            span: 1,
        },
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
        Test::Ext(list) => {
            // Folded into the buffer rather than into a fresh `String`. This
            // runs once a row, so allocating here was measured as most of what
            // an `ext:` filter costs on a query the walk cannot stop early.
            let raw = ext_bytes(name);
            if raw.is_empty() {
                return false;
            }
            let folded = fold.fold_bytes(raw);
            list.iter().any(|e| e.as_bytes() == folded)
        }
        Test::NameHas(n) => n.found_in(name, fold),
        Test::NameGlob(p) => match std::str::from_utf8(name) {
            Ok(name) => scour_query::glob_matches(p, fold.fold(name)),
            Err(_) => false,
        },
        Test::PathHas(n) => {
            // The dearest test, and the reason it is sorted last: it builds a
            // string. Everything else reads what is already there.
            let path = seg.path(row, &String::from_utf8_lossy(name));
            n.found_in(path.as_bytes(), fold)
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

    // The one order the row layout already satisfies. Everything else has to
    // see every match before it knows which forty win.
    let stored_order = want.sort == SortKey::Modified && want.descending;
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
            keyed.push((sort_value(seg, row, name, want.sort, &mut fold), row as u32));
        }
        true
    };

    match &plan.candidates {
        // Nothing here reads a name, so nothing here reads the arena.
        None if !plan.needs_name() && !has_veto => {
            for row in 0..seg.rows() {
                if !visit(row, b"") {
                    break;
                }
            }
        }
        None => seg.names.walk(0, visit),
        // Runs of adjacent blocks are walked as one, so a dense candidate set
        // costs no more seeking than a full walk would.
        Some(blocks) => {
            let mut i = 0usize;
            while i < blocks.len() {
                let mut j = i;
                while j + 1 < blocks.len() && blocks[j + 1] == blocks[j] + 1 {
                    j += 1;
                }
                let from = blocks[i] as usize * crate::columns::BLOCK;
                let to = (blocks[j] as usize + 1) * crate::columns::BLOCK;
                let mut go = true;
                seg.names.walk_range(from, to, |row, name| {
                    go = visit(row, name);
                    go
                });
                if !go {
                    break;
                }
                i = j + 1;
            }
        }
    }

    if !stored_order {
        kept = narrow(&mut keyed, need, want.descending);
    }

    // Materialise. Only now, and only what can appear: reading a row means
    // building its path, which is the expensive part of the whole operation.
    let mut hits: Vec<Hit> = kept
        .into_iter()
        .filter_map(|row| {
            let row = row as usize;
            seg.names.get(row).map(|n| seg.hit(row, n))
        })
        .collect();
    if !stored_order {
        sort_hits(&mut hits, want.sort, want.descending);
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
    }
}

/// What a row sorts by, without its row being built.
///
/// `Text` still allocates — a folded name has to live somewhere — but it is one
/// short string rather than a whole row with its path.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum SortValue {
    Num(i64),
    Text(Vec<u8>),
}

fn sort_value(
    seg: &Segment<'_>,
    row: usize,
    name: &[u8],
    key: SortKey,
    fold: &mut Folded,
) -> SortValue {
    match key {
        SortKey::Name => SortValue::Text(fold.fold_bytes(name).to_vec()),
        SortKey::Ext => SortValue::Text(fold.fold_bytes(ext_bytes(name)).to_vec()),
        SortKey::Path => {
            SortValue::Text(seg.path(row, &String::from_utf8_lossy(name)).into_bytes())
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
fn narrow(keyed: &mut [(SortValue, u32)], need: usize, desc: bool) -> Vec<u32> {
    if need == 0 || keyed.is_empty() {
        return Vec::new();
    }
    if keyed.len() <= need {
        return keyed.iter().map(|(_, row)| *row).collect();
    }
    // Selection, not a sort: finding which forty win out of two hundred
    // thousand does not require ordering the rest, and `sort_hits` orders the
    // survivors anyway.
    //
    // Measured at no difference from a full sort at a million entries — the
    // walk dominates by an order of magnitude — so this is not the reason the
    // query is fast. It is here because it is the same amount of code and it
    // stops mattering later rather than sooner.
    let k = need - 1;
    keyed.select_nth_unstable_by(k, |a, b| if desc { b.0.cmp(&a.0) } else { a.0.cmp(&b.0) });
    let (top, rest) = keyed.split_at(need);
    let boundary = &top[k].0;
    let mut out: Vec<u32> = top.iter().map(|(_, row)| *row).collect();
    out.extend(rest.iter().filter(|(v, _)| v == boundary).map(|(_, r)| *r));
    out
}

/// Deterministic ordering, with an explicit tie-break on the path.
///
/// Timestamps tie constantly — a package install stamps thousands of files at
/// one instant — so without a second key the same query returns a different
/// page each time.
pub(crate) fn sort_hits(hits: &mut [Hit], key: SortKey, desc: bool) {
    use scour_core::text::{DefaultFolder, Folder};

    // Text keys are computed once a row, not once a comparison. Folding inside
    // the comparator costs O(n log n) folds to produce a page of forty, which
    // measured 260 ms where this measures a fraction of it.
    let mut keyed: Vec<(Option<String>, Hit)> = std::mem::take(&mut hits.to_vec())
        .into_iter()
        .map(|h| {
            let k = match key {
                SortKey::Name => Some(DefaultFolder.fold(h.name())),
                SortKey::Ext => Some(scour_core::ext_of(h.name())),
                _ => None,
            };
            (k, h)
        })
        .collect();

    keyed.sort_unstable_by(|(ka, a), (kb, b)| {
        let o = match key {
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
        o.then_with(|| a.path.cmp(&b.path))
    });
    for (slot, (_, h)) in hits.iter_mut().zip(keyed) {
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
                n.found_in(name.as_bytes(), &mut fold),
                want,
                "{name:?} contains {needle:?}"
            );
        }
    }

    #[test]
    fn a_non_ascii_name_still_goes_through_the_real_folding() {
        let mut fold = Folded::new();
        // The Turkish rule: the query is folded by the parser, the name here.
        assert!(Needle::new("istanbul").found_in("İSTANBUL.txt".as_bytes(), &mut fold));
        assert!(Needle::new("isparta").found_in("ısparta.md".as_bytes(), &mut fold));
        assert!(Needle::new("öğüt").found_in("Öğüt.docx".as_bytes(), &mut fold));
        assert!(!Needle::new("zzz").found_in("Öğüt.docx".as_bytes(), &mut fold));
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
