//! Answering a query by walking rows.
//!
//! No posting list: rows are stored newest-first, so the default view stops after
//! forty matches; a numeric order stops once the page's worst row beats any
//! unopened block ([`zone_order`]); text orders read stored row lists.

use std::cell::Cell;

use memchr::memmem::Finder;
use scour_core::{Ast, Cmp, Entry, EntryId, Hit, Kind, Match, Meta, SortKey, SourceId};

use crate::columns::{BLOCK, ColumnBlocks, Field};
use crate::dirs::{DirScope, DirTable};
use crate::extension_order::ExtensionOrder;
use crate::name_order::NameOrder;
use crate::names::{Folded, NameArena};
use crate::order::PathOrder;
use crate::trigram::TrigramIndex;

/// The files a search reads, opened together.
#[derive(Debug, Clone, Copy)]
pub struct Segment<'a> {
    /// Names as the filesystem spells them. Read for the rows that are shown.
    pub names: NameArena<'a>,
    /// The same names, folded when the segment was written. **This is what a
    /// search walks**: folding at query time was three quarters of the inner loop.
    pub folded: NameArena<'a>,
    pub cols: ColumnBlocks<'a>,
    pub dirs: DirTable<'a>,
    pub tri: TrigramIndex<'a>,
    /// The rows in ascending path order, which lets a page ordered by path be a
    /// read of two hundred rows instead of a key built for every match. `None`
    /// falls back to building a key a match. See [`crate::order`].
    pub porder: Option<PathOrder<'a>>,
    /// The rows in ascending folded-name order. Missing on legacy segments;
    /// those use the keyed full walk and produce the same answer.
    pub norder: Option<NameOrder<'a>>,
    /// The rows in ascending folded-extension order. Missing on legacy
    /// segments, which fall back to the keyed full walk.
    pub eorder: Option<ExtensionOrder<'a>>,
    /// One bit a row, set when the row is still live.
    pub alive: &'a [u8],
}

/// Either persisted text order; both have the same grouped-row operations.
#[derive(Clone, Copy)]
enum GroupedPositions<'a> {
    Name(NameOrder<'a>),
    Extension(ExtensionOrder<'a>),
}

impl GroupedPositions<'_> {
    fn rows(self) -> usize {
        match self {
            GroupedPositions::Name(order) => order.rows(),
            GroupedPositions::Extension(order) => order.rows(),
        }
    }

    fn at(self, i: usize) -> Option<u32> {
        match self {
            GroupedPositions::Name(order) => order.at(i),
            GroupedPositions::Extension(order) => order.at(i),
        }
    }

    fn group_at_or_before(self, i: usize) -> Option<usize> {
        match self {
            GroupedPositions::Name(order) => order.group_at_or_before(i),
            GroupedPositions::Extension(order) => order.group_at_or_before(i),
        }
    }
}

impl<'a> Segment<'a> {
    pub fn rows(&self) -> usize {
        self.cols.rows().min(self.names.rows())
    }

    /// Is any row of this block still live? Four bytes of the bitmap, read once
    /// instead of thirty-two times: a segment a sweep emptied was otherwise
    /// walked in full on every query, 1,204,270 rows to return nothing.
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

    pub(crate) fn num(&self, field: Field, row: usize) -> i64 {
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

    pub fn source_of(&self, row: usize) -> SourceId {
        SourceId(self.num(Field::Source, row) as u32)
    }

    /// The identity of a row, which is its source and its path. Derived rather
    /// than stored: identity columns that can disagree with the name left 267
    /// rows at one path. See [`crate::ids`].
    pub fn entry_id(&self, row: usize, name: &str) -> EntryId {
        EntryId::path_hash(self.source_of(row), &self.path(row, name))
    }

    /// A path split the way a row stores it: the directory, then the name.
    pub fn split_path(path: &str) -> (&str, &str) {
        match path.rsplit_once('/') {
            Some(("", name)) => ("/", name),
            Some((parent, name)) => (parent, name),
            None => ("", path),
        }
    }

    /// Is this row the entry at `path`, exactly? The confirmation behind every
    /// probe of the id table, whose key is half a digest. `dirs` is not optional:
    /// decoding a directory per confirmation measured 2.7 µs an entry against 1.1.
    pub fn is_at(
        &self,
        dirs: &mut std::collections::HashMap<u32, String>,
        row: usize,
        source: SourceId,
        path: &str,
    ) -> bool {
        // **A rejection, not a guarantee.** The probe key mixes the source in, so
        // another source's row for the same path is never a candidate here; the
        // two comparisons below are what make a collision harmless.
        if self.source_of(row) != source {
            return false;
        }
        let (parent, name) = Segment::split_path(path);
        // The name first: a byte comparison against the arena, and it rejects
        // almost every candidate a digest collision produces.
        if self.names.get(row) != Some(name) {
            return false;
        }
        let dir = self.num(Field::DirId, row) as u32;
        let known = dirs
            .entry(dir)
            .or_insert_with(|| self.dirs.get(dir).unwrap_or_default());
        known == parent
    }

    /// Does this row already say exactly what an entry says? Asked on the view
    /// [`Segment::is_at`] opened — one view per entry measured a rescan of two
    /// million untouched rows at 6.65 s against 3.04. `atime` moves on a read.
    pub fn same_meta(&self, row: usize, meta: &Meta, is_dir: bool) -> bool {
        self.num(Field::Size, row) == meta.size
            && self.num(Field::Mtime, row) == meta.mtime
            && self.num(Field::Ctime, row) == meta.ctime
            && self.num(Field::Mode, row) == meta.mode
            && self.num(Field::Uid, row) == meta.uid
            && self.num(Field::Gid, row) == meta.gid
            && (self.num(Field::IsDir, row) != 0) == is_dir
    }

    pub(crate) fn hit(&self, row: usize, name: &str) -> Hit {
        let path = self.path(row, name);
        Hit {
            // From the path that has just been built, rather than by building
            // it a second time.
            id: EntryId::path_hash(self.source_of(row), &path),
            // Filled by the engine, which is the layer that knows whether
            // anybody asked for it.
            under: None,
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
                links: self.num(Field::Links, row).max(1),
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

/// **Not boxed, and clippy is told so.** A `Test` is evaluated per row of a walk
/// that reaches two million: the largest variant is a few words wide, where
/// boxing would cost an allocation and a pointer chase on every row.
#[allow(clippy::large_enum_variant)]
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
    /// A column masked and compared: permissions, and the type bits. Its own test
    /// because the zone map cannot help — a block whose modes run from 0o100644
    /// to 0o100755 says nothing about a bit in the middle.
    Bits {
        field: Field,
        mask: i64,
        want: i64,
        any: bool,
    },
    /// The path is this deep, counting components from the root. Carries the
    /// depth of every directory, built once per segment when a query asks — half
    /// a megabyte at 255,089 directories.
    Depth {
        depths: std::sync::Arc<[u16]>,
        cmp: Cmp,
        value: i64,
    },
    /// The name is this many characters long.
    NameLen { cmp: Cmp, value: i64 },
    /// The name contains this, spelled exactly. The **written** arena, not the
    /// folded one, which is why it is priced above `NameHas`: the walk already
    /// carries the folded name, so this costs a second lookup.
    NameHasCased(String),
    /// The name matches this pattern. The most expensive test there is and priced
    /// that way: an automaton over every name that reaches it, and nothing a
    /// trigram index can read narrows a regular expression.
    Regex(Box<regex::Regex>),
    /// The kind column is any one of these. A bitset over the discriminants, so
    /// the test stays a shift and a mask however many kinds were named.
    KindIn(u16),
    /// The name contains this, case-folded. A prebuilt searcher rather than the
    /// string: `str::contains` constructs a Two-Way searcher on every call, and
    /// this is called once a row where the walk cannot stop early.
    NameHas(Needle),
    /// The name matches this wildcard pattern, anchored end to end.
    NameGlob(String),
    /// The extension, taken from the name, is one of these. `dirs` is whether a
    /// directory can have one: asked as `ext:` it cannot — `Trabzon 2. Grup` is a
    /// folder, not a file of type ` grup` — and asked as `*.rs` it can.
    Ext { list: Vec<String>, dirs: bool },
    /// Which directories hold the term, and what a row's name has to start with
    /// for a match that straddles the last separator. See [`PathSet`].
    PathIn(Box<PathSet>),
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
            Test::Num { .. } | Test::DirIn(_) | Test::KindIn(_) | Test::Bits { .. } => 1,
            Test::Depth { .. } => 2,
            Test::Ext { .. } => 3,
            Test::NameHas(_) | Test::NameGlob(_) => 10,
            Test::NameLen { .. } => 4,
            Test::NameHasCased(_) => 20,
            // Two bitmap lookups and, rarely, a prefix test — dearer than a
            // number and cheaper than a name search.
            Test::PathIn(_) => 8,
            Test::Regex(_) => 60,
        }
    }
}

/// A needle the query owns, prepared once: `str::contains` constructs a Two-Way
/// searcher on every call, and folding the row into a buffer before searching
/// keeps SIMD on both the fold and the search.
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

    /// The needle as the parser folded it — what the trigram index is keyed on.
    fn folded(&self) -> &[u8] {
        self.finder.needle()
    }
}

/// Which directories a path term can be answered from. **A path is a directory,
/// a separator and a name**, so "the path holds `t`" is the directory holding
/// `t`, or `t` straddling the separator. Read once a query: 313,641 directories
/// against 3,019,671 rows.
#[derive(Debug)]
pub struct PathSet {
    /// Directories holding the term anywhere in them.
    whole: Vec<bool>,
    /// Directories ending with the part of the term before its last slash.
    ending: Vec<bool>,
    /// What a name must start with, for those.
    tail: Vec<u8>,
    /// The term again, when it could be in the **name** — which it can be
    /// whenever it holds no separator. `path:rapor` matches `/home/u/x/rapor.pdf`
    /// on the name alone, and a set of directories cannot see that.
    in_name: Option<Needle>,
}

impl PathSet {
    fn build(term: &str, seg: &Segment<'_>) -> PathSet {
        // The term is already folded by the parser; the table is not, so it is
        // folded here — the same fold the built path used to get.
        let (head, tail) = match term.rsplit_once('/') {
            Some((head, tail)) => (head, tail),
            // No separator at all: nothing straddles, and `whole` answers it.
            None => (term, ""),
        };
        let count = seg.dirs.len();
        let mut set = PathSet {
            whole: vec![false; count],
            ending: vec![false; count],
            tail: tail.as_bytes().to_vec(),
            in_name: None,
        };
        let needle = Needle::new(term);
        let mut fold = Folded::default();
        set.in_name = (!term.contains('/')).then(|| Needle::new(term));
        for id in 0..count {
            let Some(dir) = seg.dirs.get(id as u32) else {
                continue;
            };
            // Folded once and asked twice — the same folding a built path used
            // to get, and the same searcher.
            let folded = fold.fold_bytes(dir.as_bytes());
            set.whole[id] = needle.found_in(folded);
            set.ending[id] = folded.ends_with(head.as_bytes());
        }
        set
    }
}

/// How well a name answers the terms that were typed — the one sort that is a
/// property of the *query*. The weights are ordered, not tuned: each rung is
/// worth more than everything below it can add up to.
///
/// * the name without its extension is the term — `main.rs` for `main`
/// * the whole name is the term
/// * the name starts with it — `main_window.rs`
/// * it starts a word inside the name — `my-main.rs`, but not `domain.rs`
/// * anything else that matched at all
///
/// A stem match outranks an exact name, which filled the page with
/// `.git/refs/heads/main` and `android/src/main`. Then shorter names, then
/// `steps` — [`crate::dirs::steps_of`] — without which 88 of the first two
/// hundred results were caches against 48 of the user's own work. Both are
/// bounded to rows the name already ties: 255 for a name and sixty steps at
/// [`STEP`], against a rung gap of 900.
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
    // 255 characters of name and 480 of distance work against it, and neither
    // can overturn a rung: the narrowest gap between rungs is 900.
    best - (name.len().min(255) as i64) - i64::from(steps) * STEP
}

/// What one step away from home costs. Eight, so that sixty steps — the most the
/// table records — stay under the gap between two rungs even after a long name
/// has taken its 255.
const STEP: i64 = 8;

/// Where `needle` first appears in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    memchr::memmem::find(haystack, needle)
}

/// The complete folded extension, with eligibility read from the spelling.
fn folded_extension<'a>(seg: &Segment<'_>, row: usize, folded: &'a [u8]) -> &'a [u8] {
    let spelled = seg.names.get(row).unwrap_or_default();
    crate::extension_order::folded_extension(spelled.as_bytes(), folded)
}

/// Compare complete folded extensions without allocating either one.
pub(crate) fn compare_extensions(
    a: &Segment<'_>,
    a_row: usize,
    b: &Segment<'_>,
    b_row: usize,
) -> std::cmp::Ordering {
    let a_folded = a.folded.get(a_row).unwrap_or_default();
    let b_folded = b.folded.get(b_row).unwrap_or_default();
    folded_extension(a, a_row, a_folded.as_bytes()).cmp(folded_extension(
        b,
        b_row,
        b_folded.as_bytes(),
    ))
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

/// The blocks a search has to look at, if the trigram index can say. Only a
/// clause that is one plain `NameHas` counts — an OR needs a union and a negation
/// inverts the question, and narrowing wrongly loses files silently.
fn narrow_to_blocks(clauses: &[Clause], seg: &Segment<'_>) -> Option<Vec<u32>> {
    // `SCOUR_NO_TRIGRAM=1` turns the filter off. On 2,137,518 entries, one
    // segment, twenty rounds each, with against without: `rapor` 14.11 ms/37.97,
    // `main` 5.62/36.23, `kütüphane` 0.06/35.50 — between 2.7× and 590×.
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

/// A string every matching name must contain, if there is one. Containment is
/// the only thing claimed: over-narrowing loses files and reports nothing.
fn filterable(test: &Test) -> Option<Vec<u8>> {
    match test {
        Test::NameHas(n) => Some(n.folded().to_vec()),
        // A name with extension `pdf` contains `.pdf`. One extension only: a list
        // would need the union of its lists, not the intersection.
        Test::Ext { list, .. } => match &list[..] {
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
    /// `None` is "all of them", which a query with no usable trigram still does.
    candidates: Option<Vec<u32>>,
}

impl Plan {
    /// Compile an [`Ast`] against a segment. Everything that can be resolved once
    /// is resolved here — an `under:` path becomes a range of directory numbers —
    /// so the per-row work is comparisons and nothing else.
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

    /// Could any row of this block match? Two comparisons against the range the
    /// block stores. It can only reject, so it turns misses into skips and never
    /// a match into a miss.
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

    /// Does answering this require reading names at all? `kind:image size:>10mb`
    /// does not, and the difference is the whole inner loop: a `memchr` and
    /// twenty bytes of memory traffic a row, for tests that never look at it.
    fn needs_name(&self) -> bool {
        self.clauses.iter().flat_map(|c| &c.alts).any(|(_, t)| {
            matches!(
                t,
                Test::NameHas(_)
                    | Test::NameGlob(_)
                    | Test::Ext { .. }
                    | Test::PathIn(_)
                    | Test::Regex(_)
                    | Test::NameLen { .. }
                    | Test::NameHasCased(_)
            )
        })
    }

    /// Does this row match? `name` is the row's name **already folded**, passed in
    /// because the walk reads names sequentially — which is why the arena has no
    /// offsets. Nothing is folded here.
    pub fn accepts(&self, seg: &Segment<'_>, row: usize, name: &[u8]) -> bool {
        for clause in &self.clauses {
            let mut any = false;
            for (negated, test) in &clause.alts {
                if evaluate(test, seg, row, name) != *negated {
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

/// The column a query names.
fn num_field(f: scour_core::NumField) -> Field {
    use scour_core::NumField as N;
    match f {
        N::Mode => Field::Mode,
        N::Uid => Field::Uid,
        N::Gid => Field::Gid,
        N::Items => Field::Items,
        N::Disk => Field::Disk,
    }
}

fn compile_match(m: &Match, seg: &Segment<'_>) -> Result<Test, scour_core::Error> {
    use scour_core::TimeField;
    Ok(match m {
        Match::NameContains(t) => Test::NameHas(Needle::new(t)),
        // `*.rs` is the common wildcard and is exactly an extension test — one
        // short string read instead of a matcher over the whole name: 175 ms
        // against 47.
        Match::NameGlob(p) => match p.strip_prefix("*.") {
            Some(ext) if !ext.is_empty() && !ext.contains(['*', '?', '.']) => {
                // A pattern over the name, so a directory called `mod.rs` matches.
                Test::Ext {
                    list: vec![ext.to_owned()],
                    dirs: true,
                }
            }
            _ => Test::NameGlob(p.clone()),
        },
        // **A path is a directory and a name, and there are eight times fewer
        // directories than rows.** Building every row's path cost 1.66 s over
        // three million rows; the directory table plus a number per row, 120 ms.
        Match::PathContains(t) => Test::PathIn(Box::new(PathSet::build(t, seg))),
        Match::Ext(list) => Test::Ext {
            list: list.clone(),
            dirs: false,
        },
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
        Match::NameLen(cmp, v) => Test::NameLen {
            cmp: *cmp,
            value: *v,
        },
        Match::NameContainsCased(t) => Test::NameHasCased(t.clone()),
        Match::Depth(cmp, v) => Test::Depth {
            depths: seg.dirs.depths().into(),
            cmp: *cmp,
            value: *v,
        },
        // A pattern that will not compile is not a query that matches
        // everything: it is a query the user has to be told about.
        Match::Regex(p) => match regex::Regex::new(p) {
            Ok(re) => Test::Regex(Box::new(re)),
            Err(e) => {
                return Err(scour_core::Error::QuerySyntax {
                    at: 0,
                    expected: format!("a regular expression: {e}"),
                });
            }
        },
        Match::Num(f, cmp, v) => Test::Num {
            field: num_field(*f),
            cmp: *cmp,
            value: *v,
            span: 1,
        },
        Match::Bits {
            field,
            mask,
            want,
            any,
        } => Test::Bits {
            field: num_field(*field),
            mask: *mask,
            want: *want,
            any: *any,
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
        // Both resolve to a set of directory numbers, once, here — the same range
        // a subtree delete uses. `under:` matches a row whose directory is the
        // named one *or* any beneath it, but not the folder's own row.
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

/// Test one condition against one row. `name` arrives **folded**, out of the
/// second arena, so nothing here folds it again — that fold was three quarters of
/// the inner loop and now happens once, when the segment is written.
fn evaluate(test: &Test, seg: &Segment<'_>, row: usize, name: &[u8]) -> bool {
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
        Test::Depth { depths, cmp, value } => {
            let dir = seg.num(Field::DirId, row) as usize;
            // A row sits one level below the directory holding it.
            let d = depths.get(dir).map_or(0, |&d| i64::from(d) + 1);
            cmp.holds(d, *value)
        }
        Test::NameLen { cmp, value } => {
            // Characters, not bytes: `kütüphane.pdf` is thirteen to a person
            // and fifteen to a byte counter, and the person is asking.
            let n = std::str::from_utf8(name).map_or(name.len(), |s| s.chars().count());
            cmp.holds(n as i64, *value)
        }
        // The written spelling, compared as written: the one test that is not
        // about folding.
        Test::NameHasCased(want) => seg
            .names
            .get(row)
            .is_some_and(|written| written.contains(want.as_str())),
        Test::Regex(re) => match std::str::from_utf8(name) {
            Ok(name) => re.is_match(name),
            Err(_) => false,
        },
        Test::Bits {
            field,
            mask,
            want,
            any,
        } => {
            let got = seg.num(*field, row) & mask;
            if *any { got != 0 } else { got == *want }
        }
        Test::KindIn(mask) => {
            let k = seg.num(Field::Kind, row);
            (0..16).contains(&k) && mask & 1 << k != 0
        }
        Test::Ext { list, dirs } => {
            // A directory has no extension, so `ext:` never matches one. The
            // column read only happens for a name that already looked right.
            let ext = folded_extension(seg, row, name);
            !ext.is_empty()
                && list.iter().any(|e| e.as_bytes() == ext)
                && (*dirs || seg.num(Field::IsDir, row) == 0)
        }
        Test::NameHas(n) => n.found_in(name),
        Test::NameGlob(p) => match std::str::from_utf8(name) {
            Ok(name) => scour_query::glob_matches(p, name),
            Err(_) => false,
        },
        Test::PathIn(set) => {
            let dir = seg.dir_id(row) as usize;
            if set.whole.get(dir).copied().unwrap_or(false) {
                return true;
            }
            // A term with no separator in it can be in the name — the name is
            // part of the path.
            if let Some(needle) = &set.in_name
                && needle.found_in(name)
            {
                return true;
            }
            // And the term can straddle the one separator: the directory ends
            // with what precedes the term's last slash, the name begins with the
            // rest.
            set.ending.get(dir).copied().unwrap_or(false) && name.starts_with(&set.tail)
        }
    }
}

/// What a search asks for, and what it is allowed to spend. `count_cap` bounds the
/// walk **only once the page has been decided**, which a keyed text order never
/// reaches — there it bounds the total alone. Letting it stop the selection
/// returned "the largest forty among the five hundred newest".
#[derive(Debug, Clone, Copy)]
pub struct Wanted {
    pub sort: SortKey,
    pub descending: bool,
    pub offset: usize,
    pub limit: usize,
    /// Stop counting matches here.
    pub count_cap: usize,
    /// Answer with [`Found::ranked`] instead of [`Found::hits`], for a caller with
    /// several segments to merge: it decides the page and builds only that.
    pub rank_only: bool,
}

/// A candidate row, ordered but not built.
#[derive(Debug, Clone)]
pub struct Ranked {
    pub key: SortValue,
    /// What breaks a tie on the key, the same way [`sort_hits`] breaks it.
    pub mtime: i64,
    pub row: u32,
}

#[derive(Debug, Default)]
pub struct Found {
    pub hits: Vec<Hit>,
    /// Set instead of `hits` when [`Wanted::rank_only`] was asked for.
    pub ranked: Vec<Ranked>,
    pub total: u64,
    pub capped: bool,
    /// Whether the walk was able to stop early. Two ways and no others: the
    /// stored order has a page and a believable total, or a numeric order's
    /// unopened blocks cannot reach the page.
    pub early_exit: bool,
    pub rows_visited: u64,
    /// Rows built into a `Hit` — the expensive part, since each reconstructs a
    /// front-coded path.
    pub rows_built: u64,
}

/// Walk the segment and answer.
pub fn run(seg: &Segment<'_>, plan: &Plan, want: Wanted) -> Found {
    run_with(seg, plan, want, None, &[])
}

/// The same walk, with a veto over rows that match but must not be shown: a
/// removal disappears from searches at once, while erasing it from a segment
/// waits for the next commit. The veto runs *after* the query accepts a row,
/// being the dearer of the two.
pub fn run_with(
    seg: &Segment<'_>,
    plan: &Plan,
    want: Wanted,
    mut conceals: Option<&mut dyn FnMut(&Segment<'_>, usize, &[u8]) -> bool>,
    // What each directory row has under it, by row, sorted — see `sizes.rs`.
    // **Only `sort:size` reads it**, and only for directory rows. Empty means the
    // caller has not built it, and a folder then sorts by its own `Size` column.
    folders: &[(u32, i64)],
) -> Found {
    let has_veto = conceals.is_some();
    // A `Cell` rather than a plain counter because the block loop below reads it
    // while the closure that increments it is alive.
    let counted = Cell::new(0usize);
    let mut visited = 0u64;
    // Row numbers, not rows: four bytes each until the page is decided.
    let mut kept: Vec<u32> = Vec::new();
    /* **A page's worth of candidates, not a corpus's.** Materialising each match
     * to sort it measured 16.5 ms, and one `SortValue` per match at 2,234,583
     * entries was ~90 MB — 559 MB of peak RSS sorted by path. */
    let mut keyed: Vec<(SortValue, u32)> = Vec::new();
    /* The worst row the page holds, in key space, as of the last trim: no stored
     * number bounds a name, so no *block* can be skipped, but the row in hand can.
     * `None` under [`zone_order`], where `worst` carries the same boundary. */
    let mut bar: Option<(SortValue, u32)> = None;
    /* The key of the row being looked at, refilled rather than rebuilt: ordering
     * by path builds a string a row, and only a row that beats `bar` is copied
     * out of this buffer. */
    let mut scratch = SortValue::Num(0);
    /* Directories already rebuilt, for the one ordering that asks per row.
    See `DirPaths`. */
    let mut dir_paths = DirPaths::default();

    // The terms relevance scores against, and they have to be the merge's terms:
    // each segment picks the best `need` of its own rows and `sort_hits` orders
    // what they hand over. `filterable` cannot serve — for `ext:rs` it answers
    // `.rs`, which every match contains by definition.
    let score_terms: Vec<Vec<u8>> = if want.sort == SortKey::Relevance {
        plan.clauses
            .iter()
            .filter_map(|c| match &c.alts[..] {
                [(false, Test::NameHas(n))] if !n.folded().is_empty() => Some(n.folded().to_vec()),
                _ => None,
            })
            .collect()
    } else {
        Vec::new()
    };

    // The one order the row layout already satisfies; everything else has to see
    // every match first. Relevance with nothing to score is that same order, and
    // it is what a window shows on opening: 121 ms of full scan before this line.
    let stored_forward = (want.sort == SortKey::Modified && want.descending)
        // With nothing to score, both relevance directions are one equal-key
        // group: direction reverses only the primary key, and the tie order stays
        // newest-first then path-first, which is the stored row order.
        || (want.sort == SortKey::Relevance && score_terms.is_empty());

    // How many rows the page can possibly reach. Zero is a count and nothing
    // else, which several decisions below turn on.
    let need = want.offset + want.limit;

    // Which blocks are worth opening at all — the trigram index and the zone map,
    // both of which can only remove. Hoisted above the walk because how much of
    // the segment survived decides whether the path order can be streamed.
    let blocks = blocks_worth_opening(seg, plan);

    /* **Ordering by path is a stored order too, when the segment has one**: the
     * page is the first `need` positions that are live and match. Two conditions —
     * the walk must not need names, the folded arena having no offsets, and the
     * filters must not have narrowed much, half the blocks surviving bounding the
     * worst case at twice the walk. */
    let n_blocks = seg.rows().div_ceil(BLOCK);
    let path_stream = want.sort == SortKey::Path
        && need > 0
        && !plan.needs_name()
        && seg.porder.is_some_and(|o| o.rows() == seg.rows())
        && blocks.len().saturating_mul(2) >= n_blocks;
    // The name equivalent of `path_stream`, over an order folded at build time as
    // the keyed walk and the merge compare it. A query that reads names stays on
    // the sequential arena walk, whose locality this would discard.
    let name_stream = want.sort == SortKey::Name
        && need > 0
        && !plan.needs_name()
        && seg.norder.is_some_and(|o| o.rows() == seg.rows())
        && blocks.len().saturating_mul(2) >= n_blocks;
    // Extensions have the same grouped tie semantics as names with far fewer
    // primary values; the boundary map keeps descending order from reversing the
    // large ties.
    let extension_stream = want.sort == SortKey::Ext
        && need > 0
        && !plan.needs_name()
        && seg.eorder.is_some_and(|o| o.rows() == seg.rows())
        && blocks.len().saturating_mul(2) >= n_blocks;
    let text_stream = name_stream || extension_stream;
    let position_stream = path_stream || text_stream;

    // Whether the walk has to read names at all — decided here because the
    // direction depends on it. A streamed text order reads none: a name is wanted
    // only for rows that end up as merge candidates.
    let sort_reads_name = !stored_forward
        && !position_stream
        && matches!(want.sort, SortKey::Name | SortKey::Ext | SortKey::Path);
    let by_row = !plan.needs_name() && !has_veto && !sort_reads_name;

    /* **Oldest-first is the same walk backwards**: 60–82 ms against 1,321–2,168 on
     * 2.24 M rows. Only when the walk does not read names — the folded arena
     * stores no offsets, so there is no reading it backwards. */
    let backwards = want.sort == SortKey::Modified && !want.descending && by_row;
    // A streamed path order belongs here: the rows arrive already in the order
    // asked for, so there is nothing to select and no boundary to keep.
    let stored_order = stored_forward || backwards || position_stream;
    /* The length that triggers the next trim of `keyed`: a page's worth, then
     * twice whatever the trim left, so the amortised cost per row stays
     * constant. */
    let mut trim_at = need;
    /* Is the sort key the whole answer, or only the start of one? Asked here so
     * that the trim and the final selection cannot answer it differently. See
     * [`key_is_exact`]. */
    let exact = key_is_exact(want.sort);
    let mut done = false;

    /* **A number the rows are not stored in can still stop early**: every block
     * records the minimum and maximum of every column, so the blocks are opened
     * best first. `need == 0` is a count, with no page to bound. */
    let bounded = !stored_order && need > 0 && sort_field(want.sort).is_some();
    /* The worst row the page currently holds, once it holds a page of them, in
     * the block order's key space where smaller is better — see [`zone_key`].
     * `None` until the page fills, because until then nothing is out of reach. */
    let worst: Cell<Option<(i64, u32)>> = Cell::new(None);
    /* Set when no unopened block can reach the page: the walk can no longer change
     * *which* rows are returned, so the only thing left to walk for is the count,
     * which may stop at `count_cap`. Before it is set, the cap must not. */
    let closed = Cell::new(false);
    /* The date the page ends on, once there is a page. A backwards walk yields
     * dates in order but paths *reversed* within a date, and the merge sorts ties
     * by path, so the walk runs to the end of that group and hands all of it over. */
    let mut edge: Option<i64> = None;

    let mut visit = |row: usize, name: &[u8]| -> bool {
        visited += 1;
        if !seg.is_alive(row) || !plan.accepts(seg, row, name) {
            return true;
        }
        if let Some(veto) = conceals.as_deref_mut()
            && veto(seg, row, name)
        {
            return true;
        }
        counted.set(counted.get() + 1);
        if backwards {
            let when = seg.num(Field::Mtime, row);
            if kept.len() < need {
                kept.push(row as u32);
                if kept.len() == need {
                    edge = Some(when);
                }
            } else if edge == Some(when) {
                // Still inside the group the page ends on. See `edge`.
                kept.push(row as u32);
            } else if counted.get() >= want.count_cap {
                // Past that group, and enough counted for the total to be
                // honest. Both, for the reason the forward walk gives below.
                done = true;
                return false;
            }
        } else if stored_order {
            if kept.len() < need {
                kept.push(row as u32);
            }
            // Both conditions must hold: enough rows for the page, and enough
            // counted for the total to be honest. The first alone is what made
            // the engine this replaces report a page and a lie.
            if kept.len() >= need && counted.get() >= want.count_cap {
                done = true;
                return false;
            }
        } else if !closed.get() {
            // Any other order has to see every match before it knows which forty
            // win, so the count cap must not stop the walk here: it turned "the
            // largest forty" into "the largest forty among the five hundred
            // newest".
            if need == 0 {
                return true;
            }
            sort_value_into(
                &mut scratch,
                seg,
                row,
                name,
                want.sort,
                &score_terms,
                folders,
                &mut dir_paths,
            );
            // Rejected before it is kept, which for a path is before its
            // string is copied out of the buffer it was built in.
            if out_of_page(&bar, &scratch, row as u32, want.descending, exact) {
                return true;
            }
            keyed.push((scratch.clone(), row as u32));
            // **A page's worth, not a corpus's.** Throwing rows away is only safe
            // when the key is the whole answer, and an abbreviated name key ties
            // rows that are not equal — `narrow`'s job. One selection per `need`
            // pushes is amortised constant and leaves the boundary at `need - 1`.
            if keyed.len() >= trim_at {
                narrow(&mut keyed, need, want.descending, exact);
                trim_at = keyed.len().saturating_mul(2);
                if bounded {
                    if let (SortValue::Num(v), at) = &keyed[need - 1] {
                        worst.set(Some((zone_key(*v, want.descending), *at)));
                    }
                } else {
                    // **The row gate, and only where the block gate cannot run.**
                    // Under `zone_order` the boundary is refreshed every `need`
                    // *matches*, and a staler `worst` skips fewer blocks.
                    bar = Some(keyed[need - 1].clone());
                }
            }
        } else if counted.get() >= want.count_cap {
            // The page can no longer change and the total is believed: `closed`
            // is the block loop's statement that nothing left can reach the page,
            // and the cap makes the number beside it honest.
            done = true;
            return false;
        }
        true
    };

    // Nothing here reads a name, and the *sort* has to be asked too: `sort_value`
    // reading the empty name gave every row sorted by name the same key, at a cost
    // of 1,117,687 rows built to return forty.
    let mut walk = |from: usize, to: usize, rev: bool| -> bool {
        if by_row {
            if rev {
                for row in (from..to).rev() {
                    if !visit(row, b"") {
                        return false;
                    }
                }
            } else {
                for row in from..to {
                    if !visit(row, b"") {
                        return false;
                    }
                }
            }
            return true;
        }
        let mut go = true;
        // The **folded** arena: a search matches folded text, and folding
        // it here instead was 24.4 ns of the 40.5 a row used to cost.
        seg.folded.walk_range(from, to, |row, name| {
            go = visit(row, name);
            go
        });
        go
    };

    // Whether any candidate block was left unopened, which is what
    // [`Found::early_exit`] reports; `visit` sets `done` for the other way out.
    let mut skipped = false;
    let grouped_positions = seg
        .norder
        .filter(|_| name_stream)
        .map(GroupedPositions::Name)
        .or_else(|| {
            seg.eorder
                .filter(|_| extension_stream)
                .map(GroupedPositions::Extension)
        });
    if let Some(positions) = grouped_positions {
        /* Ascending text keys are the position list as written; descending reads
         * primary-key groups from the end but each group forwards. Ties stay
         * newest-first then path-first in both directions. */
        if want.descending {
            let mut end = positions.rows();
            while end > 0 {
                let Some(start) = positions.group_at_or_before(end - 1) else {
                    break;
                };
                for i in start..end {
                    let Some(row) = positions.at(i) else {
                        continue;
                    };
                    let name = if has_veto {
                        seg.folded.get(row as usize).unwrap_or_default().as_bytes()
                    } else {
                        b""
                    };
                    if !visit(row as usize, name) {
                        end = 0;
                        break;
                    }
                }
                if end > 0 {
                    end = start;
                }
            }
        } else {
            for i in 0..positions.rows() {
                let Some(row) = positions.at(i) else {
                    continue;
                };
                let name = if has_veto {
                    seg.folded.get(row as usize).unwrap_or_default().as_bytes()
                } else {
                    b""
                };
                if !visit(row as usize, name) {
                    break;
                }
            }
        }
    } else if let Some(positions) = seg.porder.filter(|_| path_stream) {
        /* **The page is the first `need` positions that survive.** What it gives
         * up is locality — consecutive positions are scattered rows — against not
         * making the read at all: two hundred rows against 2,235,402. The block
         * filters are ignored, soundly: both only remove rows `accepts` rejects. */
        let n = positions.rows();
        for i in 0..n {
            let at = if want.descending { n - 1 - i } else { i };
            // A position naming a row this segment does not hold is damage the
            // reader has already refused to pass on.
            let Some(row) = positions.at(at) else {
                continue;
            };
            let name = if has_veto {
                seg.folded.get(row as usize).unwrap_or_default().as_bytes()
            } else {
                b""
            };
            if !visit(row as usize, name) {
                break;
            }
        }
    }
    // **Best block first.** The blocks are opened in the order of what their
    // stored maximum — or minimum, ascending — could contribute, and the walk ends
    // when the page's worst row beats everything the next could hold.
    else if let Some(field) = sort_field(want.sort).filter(|_| bounded) {
        /* **The ordering is built when it can pay for itself, and not before**: it
         * saves nothing until there is a page for a block to be out of reach *of*,
         * and building it anyway measured 9.6–10.9 ms against 8.6–9.0 on an
         * already-narrow `rapor` sorted by size. */
        let mut order: Vec<(i64, u32)> = Vec::new();
        let mut ordered = false;
        let mut at = 0usize;
        let mut stopped = false;
        loop {
            if !ordered && worst.get().is_some() {
                order = zone_order(seg, field, want.descending, &blocks[at..], folders);
                ordered = true;
                at = 0;
            }
            let block = if ordered {
                let Some(&(reach, block)) = order.get(at) else {
                    break;
                };
                if out_of_reach(worst.get(), reach, block) {
                    break;
                }
                block
            } else {
                let Some(&block) = blocks.get(at) else {
                    break;
                };
                block
            };
            at += 1;
            let from = block as usize * BLOCK;
            let to = ((block as usize + 1) * BLOCK).min(seg.rows());
            stopped = !walk(from, to, false);
            if stopped {
                break;
            }
        }
        let mut left: Vec<u32> = if ordered {
            order[at..].iter().map(|&(_, b)| b).collect()
        } else {
            blocks[at..].to_vec()
        };
        // **The count is a separate obligation, and this is where it is paid.**
        // The page is settled and the total is not, so what is left of the blocks
        // is walked for the count alone. `closed` is what lets `count_cap` stop.
        if !stopped && counted.get() < want.count_cap {
            closed.set(true);
            left.sort_unstable();
            for (from, to) in runs_of(&left, seg.rows()) {
                if !walk(from, to, false) {
                    break;
                }
            }
        } else {
            skipped = !left.is_empty();
        }
    }
    // Every other order walks the candidates in row order, or from the far end
    // when the stored order is being read backwards.
    else {
        let mut runs = runs_of(&blocks, seg.rows());
        if backwards {
            runs.reverse();
        }
        for (from, to) in runs {
            if !walk(from, to, backwards) {
                break;
            }
        }
    }

    let done = done || skipped;

    // The candidates, each with what it sorts by. The stored order needs no
    // selection, but still has to say what it sorts by: whoever merges this
    // segment cannot see the row order. Built for the rows *kept* and no others.
    let ranked: Vec<(SortValue, u32)> = if stored_order {
        kept.iter()
            .map(|&row| {
                // A streamed text order needs a merge key only for the page, so
                // these are random arena reads for `need` rows.
                let name = if text_stream {
                    seg.folded.get(row as usize).unwrap_or_default().as_bytes()
                } else {
                    b""
                };
                (
                    sort_value(
                        seg,
                        row as usize,
                        name,
                        want.sort,
                        &score_terms,
                        folders,
                        &mut dir_paths,
                    ),
                    row,
                )
            })
            .collect()
    } else {
        // The last of the trims the walk has been making all along, and the
        // only one on a buffer that never reached `trim_at`.
        narrow(&mut keyed, need, want.descending, exact);
        keyed
    };

    // **Keys and row numbers, for a caller with other segments to merge this
    // with.** Building a row reconstructs its front-coded path, and the merge
    // throws away all but the page: 401,438 paths for sixty, 1.43 s.
    if want.rank_only {
        return Found {
            ranked: ranked
                .into_iter()
                .map(|(key, row)| Ranked {
                    key,
                    // The merge's second key, matching `sort_hits`. One column
                    // read a candidate, and only for candidates.
                    mtime: seg.num(Field::Mtime, row as usize),
                    row,
                })
                .collect(),
            total: counted.get().min(want.count_cap) as u64,
            capped: counted.get() >= want.count_cap,
            early_exit: done,
            rows_visited: visited,
            ..Found::default()
        };
    }
    kept = ranked.into_iter().map(|(_, row)| row).collect();

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
    // **And the backwards walk sorts too**: its rows arrive in date order with the
    // paths reversed inside a date, and `kept` is a page and a tie group. **So
    // does a streamed path order**, where two rows can spell the same path.
    if !stored_order || backwards || position_stream {
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
        total: counted.get().min(want.count_cap) as u64,
        capped: counted.get() >= want.count_cap,
        early_exit: done,
        rows_visited: visited,
        rows_built,
        ..Found::default()
    }
}

/// What a row sorts by, without its row being built. Sorting a million rows by
/// name used to allocate a million short `Vec`s, 1,666 ms; `Head` packs the first
/// bytes of the folded name into a number that orders identically.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SortValue {
    Num(i64),
    /// The first sixteen bytes, big-endian. Abbreviated for names and for the
    /// rare extension whose Unicode fold grows past sixteen bytes; equality
    /// means "might be equal" and the boundary group is compared in full.
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

/// Is this key exact, or only the beginning of one? Names are abbreviated, and so
/// are extensions — eligibility is twelve bytes of the spelling, which folding can
/// expand past sixteen. **The path is not on this list**: the merge resolves an
/// equal key by comparing folded names, so a path key would order its tie group by
/// file name, and sixteen bytes of a path is mostly directory.
pub(crate) fn key_is_exact(key: SortKey) -> bool {
    !matches!(key, SortKey::Name | SortKey::Ext)
}

/// The directory paths this walk has already rebuilt, by directory number.
/// [`DirTable::get`] decodes forward from the nearest restart and allocates, the
/// wrong trade for a table read once per row: 2,241,762 rows in 257,167
/// directories is **every directory rebuilt 8.7 times over**.
#[derive(Default)]
pub(crate) struct DirPaths {
    dirs: Vec<Option<Box<str>>>,
    /// Where the last row's spelled name was found. See [`crate::names::Reader`].
    names: crate::names::Reader,
}

impl DirPaths {
    fn of<'a>(&'a mut self, seg: &Segment<'_>, id: u32) -> &'a str {
        if self.dirs.is_empty() {
            self.dirs.resize_with(seg.dirs.len(), || None);
        }
        let at = id as usize;
        if at >= self.dirs.len() {
            // A number the table does not hold. `get` says the same thing by
            // returning nothing, and a row with no directory is its own name.
            return "";
        }
        if self.dirs[at].is_none() {
            self.dirs[at] = Some(seg.dirs.get(id).unwrap_or_default().into_boxed_str());
        }
        self.dirs[at].as_deref().unwrap_or_default()
    }

    /// The row's name as the filesystem spells it, read forward. **Not
    /// [`NameArena::get`]**, which restarts from the block offset and pays up to
    /// thirty-one NUL scans for a name one scan away: **320 ms of 665** on
    /// 2,234,580 rows. A path is ordered as it is *spelled*, not folded.
    fn spelled<'a>(&mut self, seg: &Segment<'a>, row: usize) -> &'a str {
        self.names.at(&seg.names, row)
    }
}

/// `name` is the row's folded name, as the walk yields it. For a caller holding
/// one row; the walk uses [`sort_value_into`], which reuses the buffer a path key
/// is built in.
fn sort_value(
    seg: &Segment<'_>,
    row: usize,
    name: &[u8],
    key: SortKey,
    terms: &[Vec<u8>],
    folders: &[(u32, i64)],
    dirs: &mut DirPaths,
) -> SortValue {
    let mut out = SortValue::Num(0);
    sort_value_into(&mut out, seg, row, name, key, terms, folders, dirs);
    out
}

/// The same, written into a value the walk hands back on the next row. **Only the
/// path key cares**: a fresh string per row is 2,234,583 allocations on an
/// unfiltered query, of which two hundred are kept, so the buffer in `out` is
/// refilled and [`out_of_page`] reads it in place.
// One argument over the limit, and it is that buffer: gathering them into a
// struct would put a lifetime on the walk's hottest call.
#[allow(clippy::too_many_arguments)]
fn sort_value_into(
    out: &mut SortValue,
    seg: &Segment<'_>,
    row: usize,
    name: &[u8],
    key: SortKey,
    terms: &[Vec<u8>],
    folders: &[(u32, i64)],
    dirs: &mut DirPaths,
) {
    if key == SortKey::Path {
        // Take back whatever buffer the last row was built in. A row keyed by
        // something else, or the first row of a walk, starts with none.
        let mut buf = match std::mem::replace(out, SortValue::Num(0)) {
            SortValue::Text(b) => b,
            _ => Vec::new(),
        };
        buf.clear();
        // The name first, and it has to be: `of` hands back a borrow of the
        // cache, so the reader cannot be asked while that is held.
        let raw = dirs.spelled(seg, row).as_bytes();
        // The same join `Segment::path` makes, over a directory this walk may
        // already have rebuilt. Written into the buffer rather than through
        // `format!`, which allocates twice.
        let dir = dirs.of(seg, seg.dir_id(row));
        match dir {
            "" => buf.extend_from_slice(raw),
            "/" => {
                buf.push(b'/');
                buf.extend_from_slice(raw);
            }
            d => {
                buf.extend_from_slice(d.as_bytes());
                buf.push(b'/');
                buf.extend_from_slice(raw);
            }
        }
        *out = SortValue::Text(buf);
        return;
    }
    *out = match key {
        // Already folded, which is what the terms are, plus the directory's
        // recorded distance — one byte read.
        SortKey::Relevance => {
            SortValue::Num(relevance(name, terms, seg.dirs.steps(seg.dir_id(row))))
        }
        SortKey::Name => SortValue::Head(head(name)),
        SortKey::Ext => SortValue::Head(head(folded_extension(seg, row, name))),
        // Answered above, where the buffer it is built in can be reused.
        SortKey::Path => unreachable!("the path key is written, not returned"),
        // **A folder sorts by what is under it**, when that is known: its own
        // `Size` is its entry table, four kilobytes, which put every folder behind
        // every file larger than a block. Empty table or a file: the column.
        SortKey::Size => SortValue::Num({
            let own = seg.num(Field::Size, row);
            // **The `IsDir` read first, and it is not a micro-optimisation.**
            // Without it two million file rows each pay a failed binary search
            // over a quarter of a million directories: 183 ms against 138.
            if seg.num(Field::IsDir, row) == 0 || folders.is_empty() {
                own
            } else {
                folders
                    .binary_search_by_key(&(row as u32), |(r, _)| *r)
                    .ok()
                    .map(|i| folders[i].1)
                    .unwrap_or(own)
            }
        }),
        SortKey::Modified => SortValue::Num(seg.num(Field::Mtime, row)),
        SortKey::Created => SortValue::Num(seg.num(Field::Ctime, row)),
        SortKey::Accessed => SortValue::Num(seg.num(Field::Atime, row)),
        SortKey::Kind => SortValue::Num(seg.num(Field::Kind, row)),
        SortKey::Mode => SortValue::Num(seg.num(Field::Mode, row)),
        SortKey::Uid => SortValue::Num(seg.num(Field::Uid, row)),
        SortKey::Gid => SortValue::Num(seg.num(Field::Gid, row)),
        SortKey::Disk => SortValue::Num(seg.num(Field::Disk, row)),
    };
}

/// The column a numeric order reads, when there is one: the keys whose value is a
/// number a row stores, which is what lets the zone map bound a block without
/// decoding it. `Size` is here because [`zone_order`] widens the range to cover
/// the folder rollups.
fn sort_field(key: SortKey) -> Option<Field> {
    Some(match key {
        SortKey::Size => Field::Size,
        SortKey::Modified => Field::Mtime,
        SortKey::Created => Field::Ctime,
        SortKey::Accessed => Field::Atime,
        SortKey::Disk => Field::Disk,
        SortKey::Mode => Field::Mode,
        SortKey::Uid => Field::Uid,
        SortKey::Gid => Field::Gid,
        SortKey::Kind => Field::Kind,
        SortKey::Name | SortKey::Ext | SortKey::Path | SortKey::Relevance => return None,
    })
}

/// A sort value as the block order reads it: **smaller is better**, whichever
/// direction was asked for. The complement rather than the negation, because
/// `-i64::MIN` is not an `i64` — so [`out_of_reach`] needs no direction at all.
fn zone_key(value: i64, desc: bool) -> i64 {
    if desc { !value } else { value }
}

/// The candidate blocks in the order a numeric sort wants them, best first — the
/// stored maximum descending, the minimum ascending, decoding not one row. Ties
/// break on the block number ascending, which is load-bearing: stopping is sound
/// only if every unopened block holds rows *after* those already held.
fn zone_order(
    seg: &Segment<'_>,
    field: Field,
    desc: bool,
    blocks: &[u32],
    folders: &[(u32, i64)],
) -> Vec<(i64, u32)> {
    let rollups = field == Field::Size && !folders.is_empty();
    let mut at = 0usize;
    let mut out: Vec<(i64, u32)> = Vec::with_capacity(blocks.len());
    for &block in blocks {
        // A block the header cannot describe is one nothing may be concluded
        // about, so it reaches everything and is opened first.
        let (lo, hi) = seg
            .cols
            .block_range(field, block as usize)
            .unwrap_or((i64::MIN, i64::MAX));
        let mut reach = if desc { hi } else { lo };
        if rollups {
            let from = block as usize * BLOCK;
            let to = from + BLOCK;
            while at < folders.len() && (folders[at].0 as usize) < from {
                at += 1;
            }
            let mut j = at;
            while j < folders.len() && (folders[j].0 as usize) < to {
                reach = if desc {
                    reach.max(folders[j].1)
                } else {
                    reach.min(folders[j].1)
                };
                j += 1;
            }
        }
        out.push((zone_key(reach, desc), block));
    }
    // The derived order on the pair is already the one wanted — reach first, block
    // number to break it — which is why the key is complemented rather than
    // compared through a closure that asks the direction per comparison.
    out.sort_unstable();
    out
}

/// Can any row of this block still reach the page? `worst` and `reach` are both
/// in [`zone_key`] space, where smaller is better; [`zone_order`] sorts by reach
/// and then by block, so one block out of reach means every later one is too.
fn out_of_reach(worst: Option<(i64, u32)>, reach: i64, block: u32) -> bool {
    match worst {
        // Not a page yet, so nothing to be out of reach of.
        None => false,
        Some((held, row)) => {
            held < reach || (held == reach && (row as usize) < block as usize * BLOCK)
        }
    }
}

/// The order a page is chosen in: the sort value, then the row. The row is not a
/// formality — rows are stored newest-first with the path breaking that, so
/// ordering ties by row is ordering them the way the final sort will, and it makes
/// the comparison total: keeping `kind`'s tie group cost 73 ms against four.
fn page_order(desc: bool) -> impl Fn(&(SortValue, u32), &(SortValue, u32)) -> std::cmp::Ordering {
    move |a, b| if desc { b.0.cmp(&a.0) } else { a.0.cmp(&b.0) }.then(a.1.cmp(&b.1))
}

/// The rows that can still reach the page, given only their sort values.
///
/// The whole tie group at the boundary comes too: the final order breaks ties on
/// the path, so a row tied with the last may still displace it, and cutting at
/// exactly `need` returns a page that is deterministic, plausible and wrong.
///
/// Also the trim, so it has to be idempotent — and is, because **the page's worst
/// row only ever improves**. The group is kept by *value*, not by the row that
/// breaks it: a row the boundary ties with today may be the boundary tomorrow.
fn narrow(keyed: &mut Vec<(SortValue, u32)>, need: usize, desc: bool, exact: bool) {
    if need == 0 {
        keyed.clear();
        return;
    }
    // Fewer than a page: nothing to choose between, and no boundary to be had.
    if keyed.len() < need {
        return;
    }
    // Selection, not a sort: which forty win out of two hundred thousand does not
    // require ordering the rest. Run even at exactly `need`, where it leaves the
    // page's worst row at `need - 1` for `worst` and `bar` to read.
    let k = need - 1;
    keyed.select_nth_unstable_by(k, page_order(desc));
    if exact {
        keyed.truncate(need);
        return;
    }
    // An abbreviated key only says the first sixteen bytes agree, and there are
    // few such rows. Gathered by swapping rather than into a second vector,
    // because this runs inside the walk.
    let boundary = keyed[k].0.clone();
    let mut end = need;
    for i in need..keyed.len() {
        if keyed[i].0 == boundary {
            keyed.swap(end, i);
            end += 1;
        }
    }
    keyed.truncate(end);
}

/// Can this row still reach the page, given the worst row the page holds?
///
/// The text key's answer to [`out_of_reach`]: nothing here skips a block, since no
/// stored number bounds a name. What it skips is the **keeping** — 32 bytes a
/// candidate and, ordering by path, the string it owns, 2,234,583 times over.
/// `bar` may be thousands of rows stale, which is safe in the one direction that
/// matters: the page's worst only improves, so an old boundary rejects less.
fn out_of_page(
    bar: &Option<(SortValue, u32)>,
    key: &SortValue,
    row: u32,
    desc: bool,
    exact: bool,
) -> bool {
    let Some((held, at)) = bar else {
        // No page yet, so nothing to be out of.
        return false;
    };
    match if desc { held.cmp(key) } else { key.cmp(held) } {
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Greater => true,
        // Equal on the key: the row number is what `page_order` breaks it
        // with, and it may only be trusted when the key is the whole answer.
        std::cmp::Ordering::Equal => exact && row > *at,
    }
}

/// Which blocks are worth opening at all: the trigram index says which blocks
/// could contain the text, the zone map which could satisfy the numbers, and both
/// can only remove. **Shared, because the two callers diverging is what went
/// wrong** — a search narrowed and a facet did not.
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

/// Adjacent blocks as row ranges. Coalesced first rather than as the walk goes,
/// so a dense set costs no more seeking than a full walk, and the backwards walk
/// can take the same runs from the far end.
fn runs_of(blocks: &[u32], rows: usize) -> Vec<(usize, usize)> {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    let mut i = 0usize;
    while i < blocks.len() {
        let mut j = i;
        while j + 1 < blocks.len() && blocks[j + 1] == blocks[j] + 1 {
            j += 1;
        }
        runs.push((
            blocks[i] as usize * BLOCK,
            ((blocks[j] as usize + 1) * BLOCK).min(rows),
        ));
        i = j + 1;
    }
    runs
}

/// Every matching row of one segment, narrowed the same way a search is, for the
/// questions about the whole matching set rather than a page of it. `f` is given
/// the row; a caller that wants the name reads it. False if `f` stopped it.
pub fn walk_matches(seg: &Segment<'_>, plan: &Plan, mut f: impl FnMut(usize) -> bool) -> bool {
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
                if seg.is_alive(row) && plan.accepts(seg, row, b"") && !f(row) {
                    return false;
                }
            }
        } else {
            let mut go = true;
            // The **folded** arena, exactly as a search walks it: the spelled
            // name means `RAPOR.pdf` quietly does not match `rapor`.
            seg.folded.walk_range(from, to, |row, name| {
                if seg.is_alive(row) && plan.accepts(seg, row, name) {
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

    // Text keys are computed once a row, not once a comparison: folding inside
    // the comparator costs O(n log n) folds for a page of forty, and measured
    // 260 ms.
    let mut keyed: Vec<(Option<String>, i64, Hit)> = std::mem::take(&mut hits.to_vec())
        .into_iter()
        .map(|h| {
            let k = match key {
                SortKey::Name => Some(DefaultFolder.fold(h.name())),
                SortKey::Ext => Some(scour_core::ext_of(h.name())),
                _ => None,
            };
            // Scored again here rather than carried: this merges rows from
            // several segments, each scored against the same terms.
            let r = if key == SortKey::Relevance {
                let folded: Vec<Vec<u8>> = terms
                    .iter()
                    .map(|t| DefaultFolder.fold(t).into_bytes())
                    .collect();
                // The distance is recomputed from the path because a `Hit` carries
                // no directory number. Same function, same answer.
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
            SortKey::Mode => a.meta.mode.cmp(&b.meta.mode),
            SortKey::Uid => a.meta.uid.cmp(&b.meta.uid),
            SortKey::Gid => a.meta.gid.cmp(&b.meta.gid),
            SortKey::Disk => a.meta.disk.cmp(&b.meta.disk),
        };
        let o = if desc { o.reverse() } else { o };
        // The stored order breaks the tie: newest first, then path. Not the path
        // alone — on a low-cardinality key like `kind` that makes the whole result
        // set one tie group.
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
                n.found_in(fold.fold_bytes(name.as_bytes())),
                want,
                "{name:?} contains {needle:?}"
            );
        }
    }

    #[test]
    fn a_non_ascii_name_still_goes_through_the_real_folding() {
        let mut fold = Folded::new();
        // The Turkish rule: the query is folded by the parser, the name here.
        let folded = |fold: &mut Folded, text: &str| -> Vec<u8> {
            fold.fold_bytes(text.as_bytes()).to_vec()
        };
        assert!(Needle::new("istanbul").found_in(&folded(&mut fold, "İSTANBUL.txt")));
        assert!(Needle::new("isparta").found_in(&folded(&mut fold, "ısparta.md")));
        assert!(Needle::new("öğüt").found_in(&folded(&mut fold, "Öğüt.docx")));
        assert!(!Needle::new("zzz").found_in(&folded(&mut fold, "Öğüt.docx")));
    }

    #[test]
    fn the_folded_extension_is_the_one_the_core_defines() {
        use scour_core::text::{DefaultFolder, Folder};

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
            let folded = DefaultFolder.fold(name);
            let want = scour_core::ext_of(name);
            assert_eq!(
                crate::extension_order::folded_extension(name.as_bytes(), folded.as_bytes()),
                want.as_bytes(),
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
        // Ranking the exact name highest filled the page with
        // `.git/refs/heads/main` and a hundred `android/src/main` directories.
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
        // The bound at the narrowest gap there is: a word boundary is 1000 and
        // the middle of a word is 100. A match on the boundary, as far away as
        // the table records and with the whole length penalty, still wins.
        let far_and_long = format!("my-main{}.rs", "x".repeat(250));
        assert!(at("my-main.rs", "main", 60) > at("domain.rs", "main", 0));
        assert!(at(&far_and_long, "main", 60) > at("domain.rs", "main", 0));
    }
}
