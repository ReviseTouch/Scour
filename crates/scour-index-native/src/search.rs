//! Answering a query by walking rows.
//!
//! There is no posting list here. A search starts at row zero and walks
//! forward, and the reason that is not absurd is the row order: rows are
//! stored newest-first, so the default view — the newest forty — stops after
//! forty matches. On the common query the walk never gets past the first
//! page's worth of rows.
//!
//! Sorting by another *number* stops early too, for a different reason. Each
//! block records the minimum and maximum of every column, so the blocks can be
//! put in the order of what they can reach and opened best first — and once
//! the page's worst row beats everything the next block could hold, there is
//! nothing left to look at. See [`zone_order`].
//!
//! Name, extension and path have no useful numeric bound, so their row orders
//! are stored beside each segment. A broad query can then read positions until
//! its page is full. A text query still walks the folded arena sequentially
//! after the trigram filter narrows it; jumping through random names would
//! surrender the locality the arena was designed for.
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
    /// search walks**, and the reason it exists is measured: folding at query
    /// time was three quarters of the inner loop.
    pub folded: NameArena<'a>,
    pub cols: ColumnBlocks<'a>,
    pub dirs: DirTable<'a>,
    pub tri: TrigramIndex<'a>,
    /// The rows in ascending path order, which lets a page ordered by path be
    /// a read of two hundred rows instead of a key built for every match.
    ///
    /// `None` for a segment written before it existed, and that costs
    /// correctness nothing: the walk falls back to building a key a match, as
    /// every index did until now. See [`crate::order`].
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

    /// The identity of a row, which is its source and its path.
    ///
    /// Derived rather than stored: three columns used to hold whatever the
    /// source called the entry, and a row that is a *name* cannot be
    /// identified by anything else without the two disagreeing — which is
    /// precisely what left 267 rows at one path. See [`crate::ids`].
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

    /// Is this row the entry at `path`, exactly?
    ///
    /// The confirmation behind every probe of the id table. The table's key is
    /// half a digest, so a probe answers with candidates; this is what makes a
    /// collision unable to do any harm.
    ///
    /// `dirs` is a cache and not an optimisation to be skipped. Reconstructing
    /// a directory from the front-coded table means decoding up to a whole
    /// restart block into a fresh `String`, and a rescan confirms *every* entry
    /// it re-indexes — which measured at **2.7 µs an entry** against 1.1 before
    /// any of this. The same few thousand directories answer all of it, so they
    /// are decoded once each per pass.
    pub fn is_at(
        &self,
        dirs: &mut std::collections::HashMap<u32, String>,
        row: usize,
        source: SourceId,
        path: &str,
    ) -> bool {
        // **A rejection, not a guarantee**, and worth saying so because it
        // reads like one. The key a probe is made with is `key_of(source,
        // path)` and the source is mixed into it, so another source's row for
        // the same path has a different key and is never a candidate here. The
        // two comparisons below are what make a collision harmless; this one
        // only makes the common rejection cheaper than reading a name.
        //
        // Checked by deleting it: no answer changes, on a two-source index with
        // one path in both — see `one_path_under_two_sources_is_two_rows`.
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

    /// Does this row already say exactly what an entry says?
    ///
    /// Asked once a row that [`Segment::is_at`] has just confirmed, so it runs
    /// on the view that confirmation already opened. That order is the whole
    /// design: opening a view *per entry* to answer this instead — four headers
    /// parsed, a trigram index mapped, for six integer comparisons — measured a
    /// rescan of two million untouched rows at 6.65 s against the 3.04 s it was
    /// trying to beat.
    ///
    /// `atime` is not compared. It moves when a file is *read*, so including it
    /// would call almost everything changed and the answer would always be no.
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
    /// A column masked and compared: permissions, and the type bits.
    ///
    /// Its own test rather than a `Num` with arithmetic around it, because the
    /// zone map cannot help here — a block whose modes run from 0o100644 to
    /// 0o100755 could hold a setuid file or not, and the minimum and maximum
    /// say nothing about a bit in the middle. So this one always looks at the
    /// row, and saying that in the type is better than discovering it.
    Bits {
        field: Field,
        mask: i64,
        want: i64,
        any: bool,
    },
    /// The path is this deep, counting components from the root.
    ///
    /// Carries the depth of every directory, built once per segment when a
    /// query asks and never otherwise — half a megabyte at 255,089
    /// directories. A row's depth is its directory's plus one.
    Depth {
        depths: std::sync::Arc<[u16]>,
        cmp: Cmp,
        value: i64,
    },
    /// The name is this many characters long.
    NameLen { cmp: Cmp, value: i64 },
    /// The name contains this, spelled exactly.
    ///
    /// The **written** arena, not the folded one — which is why it is priced
    /// above `NameHas`: the walk is already carrying the folded name, so this
    /// is the one name test that costs a second lookup. A plain `String`
    /// rather than a `Needle`, because a `Needle` folds what it is given and
    /// folding is the thing this test exists to avoid.
    NameHasCased(String),
    /// The name matches this pattern.
    ///
    /// The most expensive test there is, and priced that way: it runs an
    /// automaton over every name that reaches it, where `NameHas` is a
    /// substring search that skips. Nothing narrows it — a regular expression
    /// says nothing a trigram index can read — so it is the test to put last.
    Regex(Box<regex::Regex>),
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
    ///
    /// `dirs` is whether a directory can have one. Asked as `ext:` it cannot —
    /// `Trabzon 2. Grup` is a folder, not a file of type ` grup`, and on a
    /// volume written from Windows a dot in a folder name is ordinary. Asked
    /// as `*.rs` it can, because that is a pattern over the name and a folder
    /// called `mod.rs` does match it. Two questions that share an answer for
    /// files and part company on directories, which is why the flag is here
    /// and not at the two call sites deciding separately.
    Ext { list: Vec<String>, dirs: bool },
    /// The whole path contains this, case-folded. The most expensive test.
    /// Which directories hold the term, and what a row's name has to start
    /// with for a match that straddles the last separator. See [`PathSet`].
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

    /// The needle as the parser folded it — what the trigram index is keyed on.
    fn folded(&self) -> &[u8] {
        self.finder.needle()
    }
}

/// Which directories a path term can be answered from.
///
/// **A path is a directory, a separator and a name.** So "the path holds `t`"
/// is true in exactly two ways, and both of them are questions about the
/// directory table rather than about rows:
///
/// * the directory holds `t` — then every row in it matches, whatever it is
///   called;
/// * `t` straddles the separator — the directory ends with everything before
///   `t`'s last slash and the name begins with what comes after it.
///
/// The table is read once for a query and answers every row of the segment,
/// which is where the time goes: 313,641 directories against 3,019,671 rows,
/// and the directory is where nearly all of a path is.
#[derive(Debug)]
pub struct PathSet {
    /// Directories holding the term anywhere in them.
    whole: Vec<bool>,
    /// Directories ending with the part of the term before its last slash.
    ending: Vec<bool>,
    /// What a name must start with, for those.
    tail: Vec<u8>,
    /// The term again, when it could be in the **name** — which it can be
    /// whenever it holds no separator. `path:rapor` matches
    /// `/home/u/x/rapor.pdf` on the name alone, and a set of directories
    /// cannot see that. The brute-force comparison in `tests/smoke.rs` caught
    /// this the first time it was left out.
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
        // `*.rs` is the overwhelmingly common wildcard, and it is exactly an
        // extension test — which reads one short string instead of running a
        // pattern matcher over the whole name. Measured at 175 ms against 47.
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
        // directories than rows.** Building every row's path to look in it
        // cost 1.66 s over three million rows; reading the directory table
        // once and then a number per row costs 120 ms for the same answer.
        // Measured — `examples/pathcost.rs`.
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
        // Straight onto the generic column test — these columns have been in
        // every index since the first version and only the language was
        // missing.
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
        // The written spelling, compared as written.  is
        // for folded text; this is the one test that is not about folding.
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
            // column read is the cheap half of this test and only happens for
            // a name that already looked like a match.
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
            // And the term can straddle the one separator between the
            // directory and the name: the directory ends with what comes
            // before the term's last slash, the name begins with the rest.
            set.ending.get(dir).copied().unwrap_or(false) && name.starts_with(&set.tail)
        }
    }
}

/// What a search asks for, and what it is allowed to spend.
///
/// `count_cap` bounds the walk **only once the page has been decided** —
/// which is immediately for the stored order, and for a numeric order as soon
/// as [`zone_order`] runs out of blocks that could reach the page. Sorted by a
/// a text order without its persisted row list it never does: that fallback
/// has to visit every match before it can name the top forty, so there the cap
/// bounds the reported total and nothing else.
///
/// The two are separate obligations and conflating them is a known way to be
/// wrong: the page may stop early, the count may not, and a version that let
/// the cap stop the selection returned "the largest forty among the five
/// hundred newest". [`Found::early_exit`] is reported rather than assumed for
/// the same reason.
#[derive(Debug, Clone, Copy)]
pub struct Wanted {
    pub sort: SortKey,
    pub descending: bool,
    pub offset: usize,
    pub limit: usize,
    /// Stop counting matches here.
    pub count_cap: usize,
    /// Answer with [`Found::ranked`] instead of [`Found::hits`].
    ///
    /// For a caller with several segments to merge: it decides the page and
    /// builds only that, rather than each segment building a page's worth of
    /// rows for a merge to discard.
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
    /// Whether the walk was able to stop early.
    ///
    /// Two ways to earn it and no others: the stored order stops as soon as it
    /// has a page and a believable total, and a numeric order stops as soon as
    /// the blocks it has not opened cannot reach the page.
    pub early_exit: bool,
    pub rows_visited: u64,
    /// Rows built into a `Hit` — the expensive part, since each reconstructs a
    /// front-coded path. Reported because "why is this sort slow" is otherwise
    /// a guess between the walk, the selection and this.
    pub rows_built: u64,
}

/// Walk the segment and answer.
pub fn run(seg: &Segment<'_>, plan: &Plan, want: Wanted) -> Found {
    run_with(seg, plan, want, None, &[])
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
    // What each directory row has under it, by row, sorted — see `sizes.rs`.
    //
    // **Only `sort:size` reads it**, and only for rows that are directories.
    // Empty means the caller has not built it, and then a folder sorts by its
    // own `Size` column, which is what it did before folder sizes existed.
    folders: &[(u32, i64)],
) -> Found {
    let has_veto = conceals.is_some();
    let mut fold = Folded::new();
    // A `Cell` rather than a plain counter because the block loop below reads
    // it while the closure that increments it is alive. Shared, not borrowed:
    // `get`/`set` on a `usize` compile to the same load and store a captured
    // `&mut` would.
    let counted = Cell::new(0usize);
    let mut visited = 0u64;
    // Row numbers, not rows: four bytes each until the page is decided, which
    // is what keeps a query matching a million entries from building a million
    // strings.
    let mut kept: Vec<u32> = Vec::new();
    /* **A page's worth of candidates, not a corpus's.**
     *
     * A sort value and a row number, never a row: materialising each match to
     * sort it was measured at 16.5 ms where this measures a fraction of it —
     * the cost is not the comparison, it is reconstructing a front-coded path
     * per match to then throw all but forty of them away.
     *
     * And it holds `need` of them and not one per match. It grew per match
     * until now, which is 2,234,583 entries on a real index: a `SortValue` is
     * 32 bytes because one of its shapes is a `Vec`, so the buffer alone was
     * ~90 MB, and doubling it meant holding the old copy and the new one at
     * once. Sorted by path each entry also owned a string, and that measured
     * 559 MB of peak RSS against a 190 MB index — which in the service, where
     * glibc keeps freed arenas, is the gigabyte the owner reported.
     *
     * [`narrow`] is the trim, and it is the same call that decides the page at
     * the end. That is deliberate: a separate bounded structure is a second
     * place for the boundary tie group to be got wrong, and the group is the
     * whole difficulty here — see the note there. */
    let mut keyed: Vec<(SortValue, u32)> = Vec::new();
    /* The worst row the page holds, in key space, as of the last trim.
     *
     * What a text key gets instead of a zone map: no stored number bounds a
     * name, so no *block* can be skipped, but the row in hand can be — see
     * [`out_of_page`]. Left `None` under [`zone_order`], where the same
     * boundary is already carried by `worst` and rejecting rows here would
     * only make that one staler. */
    let mut bar: Option<(SortValue, u32)> = None;
    /* The key of the row being looked at, refilled rather than rebuilt.
     *
     * Ordering by path builds a string a row; the buffer inside this is the
     * one that gets built into, and only a row that beats `bar` is copied out
     * of it. */
    let mut scratch = SortValue::Num(0);
    /* Directories already rebuilt, for the one ordering that asks per row.
    See `DirPaths`. */
    let mut dir_paths = DirPaths::default();

    // The terms relevance scores against, folded, collected once.
    //
    // **The same rule the merge uses, and it has to be.** A search over several
    // segments ranks twice: each segment picks the best `need` of its own rows,
    // and `sort_hits` then orders what they all handed over. If the two score
    // against different terms, the page is *chosen* by one ranking and
    // *ordered* by another — and what comes back is neither.
    //
    // That is what `filterable` did here. It answers "what must every matching
    // name contain", which for `ext:rs` is `.rs` — true, and useless as a
    // score, because every match contains it by definition. So a segment
    // ranked its rows by name length and directory depth, handed over the
    // twenty shortest-named shallowest, and the merge — which scores against
    // `narrowing_terms`, where an `ext:` filter contributes nothing — reordered
    // those twenty by date and called them the twenty newest. They were not.
    // Asking the same query for twenty rows and for a thousand returned
    // different first twenty, which is how it was found.
    //
    // This is `narrowing_terms(1)` expressed over the compiled plan: one
    // alternative, not negated, a plain name containment.
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

    // The one order the row layout already satisfies. Everything else has to
    // see every match before it knows which forty win.
    //
    // Relevance with nothing to score against is that same order and not a
    // coincidence: every row gets the same number, so the walk would visit
    // three million of them to hand back a page the layout was already
    // holding. It is not a corner case either — it is what a search window
    // shows the instant it opens, and it cost 121 ms of full scan before this
    // line existed, on the one frame a person is actually watching for.
    let stored_forward = (want.sort == SortKey::Modified && want.descending)
        // With nothing to score, both relevance directions are one equal-key
        // group. Direction reverses only the primary key; the public tie order
        // stays newest-first/path-first, which is exactly the stored row order.
        || (want.sort == SortKey::Relevance && score_terms.is_empty());

    // How many rows the page can possibly reach. Zero is a count and nothing
    // else, which several decisions below turn on.
    let need = want.offset + want.limit;

    // Which blocks are worth opening at all. Two filters, and both can only
    // remove: the trigram index says which blocks could contain the text, and
    // the zone map says which could satisfy the numbers. What survives is
    // walked exactly as it always was.
    //
    // Hoisted above the walk because how much of the segment survived is what
    // decides whether the path order can be streamed — see `path_stream`.
    let blocks = blocks_worth_opening(seg, plan);

    /* **Ordering by path is a stored order too, when the segment has one.**
     *
     * `porder` lists the rows in path order, so the page is the first `need`
     * of them that are live and match — read in order, stop when full, exactly
     * as the row numbering answers "the newest two hundred". No key is built
     * for a row that is not on the page, which is the whole of the 303 ms.
     *
     * Two conditions, and each of them is protecting something:
     *
     * **The walk must not need names.** The folded arena stores no offsets and
     * is read sequentially; `porder` visits rows in an order that has nothing
     * to do with row numbers, so there is no walking it. This is the same
     * condition the backwards walk lives under, for the same reason.
     *
     * **The filters must not have narrowed anything much.** Streaming ignores
     * the block filter — it may, since both filters can only remove rows a
     * test would reject anyway — but a query the trigram index or the zone map
     * has cut to a handful of blocks would then read the whole order to find
     * its few matches, where the ordinary walk reads the handful. So it is
     * taken only when at least half the blocks survived, which bounds the
     * worst case at twice the rows the ordinary walk would visit and leaves
     * the case it exists for — a window opened on everything — untouched.
     * Every text query narrows through `needs_name` above; what is left here
     * is `size:`, `kind:`, `dm:`, `under:` and the empty query.
     *
     * A veto is allowed. Position order is still the right order after rows
     * are removed from it; the folded name is fetched only for the positions
     * visited so a general veto receives the same input as the ordinary walk. */
    let n_blocks = seg.rows().div_ceil(BLOCK);
    let path_stream = want.sort == SortKey::Path
        && need > 0
        && !plan.needs_name()
        && seg.porder.is_some_and(|o| o.rows() == seg.rows())
        && blocks.len().saturating_mul(2) >= n_blocks;
    // The name equivalent of `path_stream`. Its order is folded at build time,
    // exactly as both the keyed walk and the cross-segment merge compare it.
    // A query that itself reads names stays on the sequential arena walk; the
    // trigram filter normally narrows that query, while jumping through the
    // name arena in sorted-row order would discard its locality.
    let name_stream = want.sort == SortKey::Name
        && need > 0
        && !plan.needs_name()
        && seg.norder.is_some_and(|o| o.rows() == seg.rows())
        && blocks.len().saturating_mul(2) >= n_blocks;
    // Extensions have the same grouped tie semantics as names, but far fewer
    // primary values. Persisting the positions is what avoids a corpus walk;
    // the boundary map keeps descending order from reversing the large ties.
    let extension_stream = want.sort == SortKey::Ext
        && need > 0
        && !plan.needs_name()
        && seg.eorder.is_some_and(|o| o.rows() == seg.rows())
        && blocks.len().saturating_mul(2) >= n_blocks;
    let text_stream = name_stream || extension_stream;
    let position_stream = path_stream || text_stream;

    // Whether the walk has to read names at all — decided here rather than
    // below because the direction depends on it. The rest of the reasoning is
    // at the walk.
    //
    // A streamed text order reads none during the walk: the order is stored,
    // so a name is wanted only for rows that end up as merge candidates.
    let sort_reads_name = !stored_forward
        && !position_stream
        && matches!(want.sort, SortKey::Name | SortKey::Ext | SortKey::Path);
    let by_row = !plan.needs_name() && !has_veto && !sort_reads_name;

    /* **Oldest-first is the same walk backwards.**
     *
     * Rows are stored newest-first, so newest-first stops at the first page
     * and oldest-first used to visit every match: measured through the bridge
     * on 2.24 M rows, 60–82 ms against 1,321–2,168 ms, every run. It is also
     * the order a window opens in as soon as somebody has clicked the heading
     * once, so it was a three-second cold open and a list that could not keep
     * up with a scroll.
     *
     * **Only when the walk does not read names.** The folded arena is read
     * sequentially — that is why it stores no offsets — so there is no
     * backwards over it. That leaves text queries walking forwards, which is
     * the right way round anyway: a query with a word in it matches few rows,
     * and it is the queries that match *everything* that cannot afford a full
     * pass. An empty query, `kind:`, `size:`, `dm:` all qualify.
     */
    let backwards = want.sort == SortKey::Modified && !want.descending && by_row;
    // A streamed path order belongs here and not beside the keyed orders: the
    // rows arrive already in the order that was asked for, so there is nothing
    // to select and no boundary to keep — which is also what removes the tie
    // group, since two rows at one position is not a thing `porder` can hold.
    let stored_order = stored_forward || backwards || position_stream;
    /* The length that triggers the next trim of `keyed`. A page's worth to
     * begin with, because the first trim is what produces a boundary at all;
     * after that, twice whatever the trim left — so every trim is paid for by
     * that many pushes and the amortised cost per row stays constant. */
    let mut trim_at = need;
    /* Is the sort key the whole answer, or only the start of one?
     *
     * The single most dangerous question on this walk, and it is asked here so
     * that the trim and the final selection cannot answer it differently. See
     * [`key_is_exact`]. */
    let exact = key_is_exact(want.sort);
    let mut done = false;

    /* **A number the rows are not stored in can still stop early.**
     *
     * Not because of the row order — there is nothing to exploit there — but
     * because every block already records the minimum and maximum of every
     * column. Put the blocks in the order of what they can reach, open them
     * best first, and the page is decided long before the corpus has been
     * looked at. See [`zone_order`] for the ordering and [`out_of_reach`] for
     * the rule that ends it.
     *
     * `need == 0` is a count and nothing else, so there is no page to bound
     * and the walk is only an obligation to the total. */
    let bounded = !stored_order && need > 0 && sort_field(want.sort).is_some();
    /* The worst row the page currently holds, once it holds a page of them.
     *
     * In the block order's key space, where smaller is better whichever
     * direction was asked for — see [`zone_key`]. `None` until enough matches
     * have been seen to fill the page, because until then nothing is out of
     * reach of anything. */
    let worst: Cell<Option<(i64, u32)>> = Cell::new(None);
    /* Set when no unopened block can reach the page.
     *
     * What it licenses is narrow and worth stating: from here the walk cannot
     * change *which* rows are returned, so the only thing left to walk for is
     * the count — and the count may stop at `count_cap`. Before it is set, the
     * cap must not stop anything. */
    let closed = Cell::new(false);
    /* The date the page ends on, once there is a page.
     *
     * A backwards walk yields dates in order but paths *reversed* within a
     * date, because inside a segment the row number is the path order. The
     * merge sorts ties by path, so a page whose edge falls inside a group of
     * files sharing a second would otherwise be given the wrong members of it
     * to choose from — the last paths rather than the first. So the walk keeps
     * going to the end of that group and hands the whole of it over. On a
     * corpus where thousands of files share a date — a checkout, an unpacked
     * archive — that is the difference between right and plausible. */
    let mut edge: Option<i64> = None;

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
            // counted for the total to be honest. Stopping on the first alone
            // is what made the engine this replaces report a page and a lie.
            if kept.len() >= need && counted.get() >= want.count_cap {
                done = true;
                return false;
            }
        } else if !closed.get() {
            // Any other order has to see every match before it knows which
            // forty win, so the count cap must not stop the walk here.
            //
            // It did, briefly, and the result was quietly wrong: "the largest
            // forty" became "the largest forty among the five hundred newest",
            // which is a plausible-looking answer to a different question. The
            // verifier missed it because the test asked for an uncapped count,
            // where the two happen to agree.
            //
            // **A count is not a page.** With nothing to fill there is nothing
            // to key, and `/api/count` sorted by name built one per match — a
            // string per match, ordering by path — for `narrow` to throw every
            // one of them away. The row is already counted above; this only
            // declines to key it, so the total and the walk are untouched.
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
            // **A page's worth, not a corpus's.**
            //
            // Throwing rows away is only safe when the key is the whole
            // answer: an abbreviated name key ties rows that are not equal,
            // and the whole tied group has to survive for a real comparison.
            // That is `narrow`'s job and the reason the trim *is* `narrow`
            // rather than a truncation written out again here.
            //
            // One selection per `need` pushes is amortised constant, and it
            // leaves the boundary sitting at `need - 1` — where the next block
            // is measured against it under [`zone_order`], and the next row
            // under [`out_of_page`].
            if keyed.len() >= trim_at {
                narrow(&mut keyed, need, want.descending, exact);
                trim_at = keyed.len().saturating_mul(2);
                if bounded {
                    if let (SortValue::Num(v), at) = &keyed[need - 1] {
                        worst.set(Some((zone_key(*v, want.descending), *at)));
                    }
                } else {
                    // **The row gate, and only where the block gate cannot
                    // run.** Under `zone_order` the boundary is refreshed
                    // every `need` *matches*; rejecting rows here would make
                    // it every `need` *improvements* instead, which on a walk
                    // that opens the best block first is far rarer — a staler
                    // `worst` skips fewer blocks, and that is the optimisation
                    // this must not pay for.
                    bar = Some(keyed[need - 1].clone());
                }
            }
        } else if counted.get() >= want.count_cap {
            // The page can no longer change and the total is believed. Both,
            // and in that order: `closed` is the block loop's statement that
            // nothing left can reach the page, and the cap is what makes the
            // number beside it honest.
            done = true;
            return false;
        }
        true
    };

    // Nothing here reads a name, so nothing here reads the arena.
    //
    // The *sort* has to be asked too, and forgetting to was a real bug: the
    // row-driven path hands `accepts` an empty name, which is correct when no
    // test reads one — but `sort_value` was reading it as well, so every row
    // sorted by name got the same key. The answer stayed right, because
    // everything then tied and `sort_hits` compared the real names, and the
    // cost was the whole corpus: 1,117,687 rows built to return forty.
    // `sort_reads_name` and `by_row` are decided above, where the direction
    // needs them.
    //
    // `rev` is only ever asked for by the backwards walk, which is only ever
    // on when no name has to be read — the folded arena stores no offsets, so
    // there is no walking it from the far end.
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
    // [`Found::early_exit`] reports. `visit` sets `done` when the count cap
    // ends a walk; this catches the other way out, where the block order ran
    // out of anything in reach and the total was already believed.
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
        /* Ascending text keys are the position list as written. Descending
         * reads primary-key groups from the end but each group forwards. The
         * direction changes only the primary key; ties remain newest-first and
         * then path-first in both directions. */
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
        /* **The page is the first `need` positions that survive.**
         *
         * Read in order and stop — the same bargain the row numbering makes
         * for a date, and the reason this file exists. What it gives up is
         * locality: consecutive positions are rows scattered through the
         * segment, so every liveness bit and every column read is a jump.
         * That is paid for many times over by not making the read at all —
         * whole table, page of two hundred, this is 200 rows against
         * 2,235,402.
         *
         * The blocks the filters chose are ignored here, which is sound
         * because both filters can only *remove* rows `accepts` would have
         * rejected anyway — never add one. What they would have saved is
         * bounded by the condition on `path_stream` above.
         *
         * Descending is the same positions read from the far end, and unlike
         * the backwards date walk it needs no tie group carried along: two
         * rows never share a position.
         */
        let n = positions.rows();
        for i in 0..n {
            let at = if want.descending { n - 1 - i } else { i };
            // A position naming a row this segment does not hold is damage the
            // reader has already refused to pass on; skipping it keeps the
            // rest of the order readable.
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
    // **Best block first.** The blocks are opened in the order of what
    // their stored maximum — or minimum, ascending — says they could
    // contribute, and the walk ends the moment the page's worst row beats
    // everything the next one could hold.
    //
    // Nothing about *which* rows win is different here. Every row that is
    // opened goes through the same `visit`: the same liveness bit, the
    // same conditions, the same veto, and the same selection. What changes
    // is only how many blocks are opened at all.
    else if let Some(field) = sort_field(want.sort).filter(|_| bounded) {
        /* **The ordering is built when it can pay for itself, and not
         * before.**
         *
         * It costs a pass over the candidate blocks — a range read each
         * and a sort — and it saves nothing until there is a page for a
         * block to be out of reach *of*. A term the trigram filter has
         * already narrowed to fewer matches than a page never gets one,
         * and building the order anyway measured 9.6–10.9 ms against
         * 8.6–9.0 on `rapor` sorted by size: a pass over thirty thousand
         * blocks, spent to skip none of them.
         *
         * So the walk starts in block order like every other one, and
         * reorders what is left of the candidates the moment the page
         * first fills.
         *
         * A block at a time rather than in coalesced runs, because the
         * check belongs between blocks and a run here is the whole index.
         * It costs nothing measurable: `walk_range` reaches a block
         * boundary through the offset table, and the same corpus walked
         * one block at a time against runs of thousands measured 142–148
         * ms against 148–154. */
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
        // **The count is a separate obligation, and this is where it is
        // paid.** The page is settled; the total printed beside it is not,
        // and no shortcut here may be allowed to guess at it. So what is
        // left of the blocks is walked for the count alone — back in block
        // order, because from here the reads are sequential again and
        // there is no reason to keep jumping.
        //
        // `closed` is what tells `visit` that these rows can only be
        // counted, never selected, which is also what lets `count_cap`
        // stop the walk at last.
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
    // Every other order walks the candidates as it always did: forwards,
    // in row order, or from the far end when the stored order is being
    // read backwards.
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
    // selection — the rows arrived in it — but it still has to say what it
    // sorts by, because whoever merges this segment with another cannot see
    // the row order that made it true.
    /* **And this is where a streamed path order pays for itself twice.**
     *
     * A key is built here for the rows that were *kept* and for no others —
     * `need` of them, not one per match — and for `Path` that key is the real
     * joined path, exactly as it always was. So the merge across segments
     * compares paths and needs to learn nothing: positions are a segment's own
     * and never leave it, and `key_is_exact`, `narrow` and the comparator in
     * `index.rs` are all untouched by any of this.
     *
     * That is the difference between 2,235,402 keys and two hundred. */
    let ranked: Vec<(SortValue, u32)> = if stored_order {
        kept.iter()
            .map(|&row| {
                // A streamed text order needs a merge key only for the page,
                // so these are random arena reads for `need` rows rather than
                // a sequential read and key build for the whole corpus.
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

    // **Keys and row numbers, for a caller that has other segments to merge
    // this with.** Building a row means reconstructing its front-coded path,
    // and the merge throws away all but the page: at offset 100,000 the index
    // built 401,438 paths to return sixty, which took 1.43 s. So the segment
    // hands over what it takes to *order* a row and nothing that it takes to
    // *show* one, and the caller builds the window it ends up with.
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
    // **And the backwards walk sorts too.** Its rows arrive in date order with
    // the paths reversed inside a date, because inside a segment the row
    // number is the path order. `kept` is a page and a tie group, so this is a
    // sort of dozens rather than of a corpus.
    //
    // **So does a streamed path order**, and for one case only: two rows can
    // spell the same path — one path under two sources is two rows — and the
    // stored order breaks that tie by row where `sort_hits` breaks it by date,
    // which read backwards is the other way round. Every caller that merges
    // segments takes `rank_only` and never reaches this, so what it costs is a
    // sort of `need` rows in a test.
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

/// What a row sorts by, without its row being built.
///
/// Three shapes, and the middle one is the interesting one. Sorting a million
/// rows by name used to allocate a million short `Vec`s — 1,666 ms on a real
/// index. `Head` is the first eight bytes of the folded name packed
/// big-endian into a `u64`, which orders identically to the bytes it came
/// from and costs nothing: rows that tie on it are the few that share eight
/// bytes of name, and only those are compared properly.
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

/// Is this key exact, or only the beginning of one?
///
/// Names are abbreviated. Extensions usually fit too, but eligibility is
/// twelve bytes in the original spelling and Unicode folding can expand those
/// bytes beyond sixteen. Both therefore keep their boundary group and the
/// cross-segment merge compares the complete key for equal heads.
///
/// ## The path is not on this list, and it was tried twice
///
/// Ordering by path is the dearest sort there is, and abbreviating its key the
/// way the name's is abbreviated is the obvious way to make it cheap. It was
/// written — `Head` of the first sixteen bytes of the joined path, `Path` added
/// to the exception here — and it fails twice over.
///
/// **It answers the wrong question.** Saying a key is inexact does not say what
/// it is inexact *about*. The merge in `index.rs` resolves an equal key by
/// comparing the two rows' **folded names**, because until now the only
/// inexact key was the name and that was the whole truth of it. The extension
/// case now names its own resolver; give this a path key and it would silently
/// order the tie group by name: on a corpus where
/// `/home/u/.cache/M` is sixteen bytes exactly, every row under it tied, and
/// the page came back alphabetical by file name inside a directory prefix.
/// Two tests caught it — `many_segments_answer_exactly_what_one_would` and
/// `a_rebuild_folds_everything_into_one_segment_and_changes_no_answer` — and
/// they caught it as "several segments disagree with one", which is not what it
/// was: one segment was wrong too, in the same way, and the reference was what
/// disagreed.
///
/// **And it is not even reliably faster.** Sixteen bytes of a *path* is mostly
/// the directory, and paths share directories by the hundred thousand, so the
/// boundary tie group is unbounded in a way a name's never is. Measured on
/// 2,234,580 rows against its own baseline in the same run, path descending:
/// 661 ms became 588 at offset 0 and **1,101 at offset 19,800**, where the
/// exact key holds 670. The offset that was worth optimising is the one it made
/// worse.
///
/// What the sort actually cost was reading each row's spelled name — see
/// [`DirPaths::spelled`]. Fixing that keeps the key exact and takes 665 ms to
/// 424, at every offset.
///
/// Anyone who wants to try the abbreviated key again has to teach the merge
/// what the key is an abbreviation *of*, first.
///
/// **And the rest of the cost is gone by storing the order rather than the
/// key.** A segment lists its rows in path order, the walk reads that list
/// until the page is full, and the key — still the whole path, still exact —
/// is built for the two hundred rows that were kept. See [`crate::order`].
pub(crate) fn key_is_exact(key: SortKey) -> bool {
    !matches!(key, SortKey::Name | SortKey::Ext)
}

/// The directory paths this walk has already rebuilt, by directory number.
///
/// **Front coding is cheap to store and not cheap to read one row of.** A
/// `DirTable` entry is its predecessor truncated and extended, so
/// [`DirTable::get`] decodes forward from the nearest restart — up to
/// `RESTART - 1` steps, two varints and a copy each — and allocates a `String`
/// at the end of it. That is the right trade for a table read a few times and
/// the wrong one for a table read once per row, which is exactly what ordering
/// by path does.
///
/// This corpus is 2,241,762 rows in 257,167 directories: **every directory
/// rebuilt 8.7 times over**, and a fresh allocation for each. Holding what has
/// been rebuilt turns that back into once.
///
/// Sized to the table and filled as it goes, so a query that touches a corner
/// of the tree pays for the corner. Empty for every other ordering — nothing
/// but `Path` asks.
///
/// The *name* half of the same join is here for the same reason, and turned out
/// to be the larger of the two. See [`DirPaths::spelled`].
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

    /// The row's name as the filesystem spells it, read forward.
    ///
    /// **Not [`NameArena::get`]**, which restarts from the block offset and so
    /// pays up to thirty-one NUL scans for a name the previous call left it one
    /// scan short of. A path sort asks once a matching row, in row order, and
    /// on 2,234,580 rows that difference measured **320 ms of 665** — the
    /// largest single item in the sort, and larger than the key allocation
    /// everyone assumed it was behind. That allocation is real and is worth
    /// 73 ms; this was worth four times as much and needed no change to what a
    /// key *is*, which is why it went first.
    ///
    /// The folded name the walk already carries cannot stand in for it: a path
    /// is ordered as it is *spelled*, both by [`sort_hits`] and by the
    /// brute-force reference, and `B` sorts before `a` spelled where it sorts
    /// after folded.
    ///
    /// What is left to take, in order of what it is worth: the 73 ms of per-row
    /// `String` (one arena a segment, keyed by offset and length — exact, but
    /// the comparator in `index.rs` has to be able to reach the arena, so
    /// `SortValue`, `Ranked` and the merge all move), and then a stored
    /// path-order column, which makes this a k-way merge and costs a rebuild.
    ///
    /// [`Test::PathHas`] builds a whole path a row as well, and pays the same
    /// block scan. It is not given a reader because `evaluate` carries no cache
    /// to keep one in, and a `path:` query is narrowed by the trigram filter
    /// first, so few rows reach it. If that stops being true, this is the thing
    /// to reach for.
    fn spelled<'a>(&mut self, seg: &Segment<'a>, row: usize) -> &'a str {
        self.names.at(&seg.names, row)
    }
}

/// `name` is the row's folded name, as the walk yields it.
///
/// For a caller holding one row rather than walking a corpus. The walk uses
/// [`sort_value_into`], which reuses the buffer a path key is built in.
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

/// The same, written into a value the walk hands back on the next row.
///
/// **Only the path key cares**, and it is the one that matters: its key is a
/// string, so a fresh one per row is an allocation per row — 2,234,583 of them
/// on an unfiltered query, of which two hundred are kept. The buffer that
/// arrives in `out` is emptied and refilled instead, and [`out_of_page`] reads
/// it in place; a row that beats the page's worst is cloned out of it and
/// nothing else is.
///
/// Every other key is a number that replaces what was there, so `out` is just
/// where it lands.
///
/// **A number does not need the round trip, and taking it out is not worth the
/// branch.** Sending an `i64` through memory to be copied back looks like work
/// the orders that were already fast are paying for nothing, so the walk was
/// given a second arm that called [`sort_value`] and pushed the result
/// outright. Interleaved against this one on the orders that visit the most
/// rows — `size`, `kind` and `created` at offset 19,800, where a page is twenty
/// thousand rows — the two were indistinguishable: 19.3 ms against 19.4, 13.0
/// against 12.7, 7.2 against 7.4, each the best of two on a busy machine. The
/// arm was removed and this note left in its place.
// One argument over the limit, and it is the buffer this whole function exists
// for. Gathering them into a struct would put a lifetime on the walk's hottest
// call for the sake of a count.
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
        // **A folder sorts by what is under it**, when that is known. Its own
        // `Size` is its entry table — four kilobytes — so ordering by that put
        // every folder behind every file larger than a block, and on a page of
        // two hundred rows the folders simply were not there. The number on
        // screen and the order now come from one place.
        //
        // Empty table, or a row that is not a directory: the column, as
        // before.
        SortKey::Size => SortValue::Num({
            let own = seg.num(Field::Size, row);
            // **The `IsDir` read first, and it is not a micro-optimisation.**
            // Without it every one of two million file rows pays a failed
            // binary search over a quarter of a million directories, to learn
            // what one column read already said. Measured on an unfiltered
            // size sort: 183 ms with the search first, 138 ms with the column
            // first, over the same 2.2 M rows.
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

/// The column a numeric order reads, when there is one.
///
/// These are the keys whose value is a number a row already stores, which is
/// what lets the zone map — the minimum and maximum of a block, written when
/// the segment was — say what a block could contribute to a page without
/// decoding any of it. `Name`, `Ext` and `Path` are text, and `Relevance` is a
/// property of the query rather than of the row, so no stored column bounds
/// any of them. Broad text orders use their persisted positions; this keyed
/// path remains for legacy segments and name-reading queries.
///
/// `Size` is here even though a *directory* sorts by what is under it rather
/// than by its own column: [`zone_order`] widens the block's range to cover
/// the rollups, so the bound stays a bound. See the note there.
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
/// direction was asked for.
///
/// The complement rather than the negation, because `-i64::MIN` is not an
/// `i64` and a `ctime` is whatever the filesystem put there. `!v` reverses the
/// order of every `i64` — `a < b` exactly when `!a > !b` — and cannot
/// overflow, so descending and ascending become the same comparison and
/// [`out_of_reach`] needs no direction at all.
fn zone_key(value: i64, desc: bool) -> i64 {
    if desc { !value } else { value }
}

/// The candidate blocks in the order a numeric sort wants them, best first,
/// each with what it can reach.
///
/// This is the whole of the optimisation. A block records the minimum and
/// maximum of every column, so the best a block can offer a descending sort is
/// its maximum and an ascending one its minimum — and sorting the blocks on
/// that puts the page's rows in the first few. Seventy thousand blocks on this
/// index, and the pass decodes not one row.
///
/// Ties on the reach are broken by the block number, ascending, and that is
/// load-bearing rather than tidy. The page's second key is the row, so two
/// rows with the same value are separated by which comes first — and stopping
/// is only sound if every block still unopened holds rows *after* the ones
/// already held. Sorting `kind` puts a hundred thousand rows at one value;
/// without this the walk would return the right values from the wrong rows.
///
/// **`Size` is not simply the `Size` column.** A directory sorts by what is
/// under it — see [`sort_value`] — which its own column knows nothing about,
/// so the column's range is widened by the rollups of the directory rows the
/// block holds. A looser bound, not a wrong one: it can only pull a block
/// earlier than it needed to be. `folders` is sorted by row and the blocks
/// ascend, so one cursor walks it once instead of a binary search per block.
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
    // The derived order on the pair is already the one wanted — reach first,
    // block number to break it — which is why the key is complemented rather
    // than compared through a closure that has to ask the direction per
    // comparison. Seventy thousand pairs, sorted in about a millisecond.
    out.sort_unstable();
    out
}

/// Can any row of this block still reach the page?
///
/// `worst` is the worst row the page currently holds and `reach` the best this
/// block could hold, both in [`zone_key`] space where smaller is better. So no
/// row here can displace anything when the page's worst already beats the
/// block's best — or when they are equal and every row here comes later, which
/// is the case the row tie-break decides.
///
/// Because [`zone_order`] sorts by reach and then by block, one block being
/// out of reach means every block after it is too.
fn out_of_reach(worst: Option<(i64, u32)>, reach: i64, block: u32) -> bool {
    match worst {
        // Not a page yet, so nothing to be out of reach of.
        None => false,
        Some((held, row)) => {
            held < reach || (held == reach && (row as usize) < block as usize * BLOCK)
        }
    }
}

/// The order a page is chosen in: the sort value, then the row.
///
/// The row number is the second key, and it is not a formality: rows are
/// stored newest-first with the path breaking *that*, so ordering ties by row
/// is ordering them the way the final sort will. It also makes the comparison
/// total, which is what removes the tie group.
///
/// Removing it matters. Sorting by `kind` puts a hundred thousand rows at the
/// same value, and keeping that whole group — which breaking ties on the path
/// required — cost 73 ms where this costs four.
fn page_order(desc: bool) -> impl Fn(&(SortValue, u32), &(SortValue, u32)) -> std::cmp::Ordering {
    move |a, b| if desc { b.0.cmp(&a.0) } else { a.0.cmp(&b.0) }.then(a.1.cmp(&b.1))
}

/// The rows that can still reach the page, given only their sort values.
///
/// The whole tie group at the boundary comes too, and that is not a nicety: the
/// final order breaks ties on the path, so a row tied with the last one may
/// still displace it. Cutting at exactly `need` would return a page that is
/// deterministic, plausible, and not the one brute force produces — timestamps
/// tie in the thousands on a real filesystem.
///
/// ## Also the trim, which is why it is idempotent
///
/// The walk calls this every so often on a buffer it is still filling, and then
/// once more at the end. Applying it repeatedly has to be the same as applying
/// it once to everything, and it is, for a reason worth writing down: **the
/// page's worst row only ever improves**. So the boundary at the end is at
/// least as good as the boundary at any earlier trim, and a row this dropped
/// then — worse in value than the boundary of the moment — is worse than the
/// final boundary too. It can be in neither the page nor the group tied with
/// its edge.
///
/// The `need` best are kept whatever else happens, which is what makes the
/// first half of that true; the tie group is kept by *value* and not by the
/// row that breaks it, which is what makes the second half true. Keeping only
/// the rows that tie with the boundary **and** beat it on the row number would
/// be tighter, and wrong: the boundary improves, and a row it ties with today
/// may be the boundary itself tomorrow.
///
/// In place rather than by value because the walk calls it inside the loop —
/// `split_off` allocated a second vector every time it was asked.
fn narrow(keyed: &mut Vec<(SortValue, u32)>, need: usize, desc: bool, exact: bool) {
    if need == 0 {
        keyed.clear();
        return;
    }
    // Fewer than a page: nothing to choose between, and no boundary to be had.
    if keyed.len() < need {
        return;
    }
    // Selection, not a sort: finding which forty win out of two hundred
    // thousand does not require ordering the rest, and `sort_hits` orders the
    // survivors anyway.
    //
    // Run even at exactly `need`, where it selects nothing: what it leaves at
    // `need - 1` is the worst row of the page, and both `worst` and `bar` are
    // read from there the moment a page first exists.
    let k = need - 1;
    keyed.select_nth_unstable_by(k, page_order(desc));
    if exact {
        keyed.truncate(need);
        return;
    }
    // An abbreviated key only says the first sixteen bytes agree. The rows
    // that share them still have to be compared properly, and there are few
    // of them.
    //
    // Gathered by swapping rather than by filtering into a second vector: this
    // runs inside the walk now. The order the survivors end up in does not
    // matter — every caller sorts or selects again.
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
/// The text key's answer to [`out_of_reach`], and it saves something different.
/// A zone map bounds a whole *block* by numbers the segment stored, and no such
/// number exists for a name — so nothing here skips a block, and the walk is
/// the same walk. What it skips is the **keeping**: the 32 bytes a candidate
/// occupies, and, ordering by path, the string it owns. On an unfiltered query
/// that is 2,234,583 of each, to return two hundred.
///
/// `bar` is the boundary as of the last trim, and may be several thousand rows
/// out of date. That is safe in one direction only, and the direction is the
/// right one: the page's worst can only improve, so an old boundary rejects
/// less than the current one would, never more.
///
/// **Ties are kept when the key is an abbreviation.** Two names or folded
/// extensions agreeing on sixteen bytes are not necessarily equal, and which
/// one wins is settled later against the complete key — by `sort_hits` here,
/// by the comparator in `index.rs` across segments. Dropping them here is the
/// deterministic, plausible, wrong page that [`narrow`]'s boundary group
/// exists to prevent.
///
/// The comparison is [`page_order`]'s, spelled out against a borrowed key so
/// that nothing has to be cloned to ask. The two disagreeing is the failure
/// this is written next to it to avoid.
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

/// Adjacent blocks as row ranges.
///
/// Coalesced first rather than as the walk goes, so a dense set costs no more
/// seeking than a full walk would, and having them as a list is what lets the
/// backwards walk take the same runs from the far end.
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
