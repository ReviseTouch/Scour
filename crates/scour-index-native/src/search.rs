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

use scour_core::{Ast, Cmp, Entry, EntryId, Hit, Key, Kind, Match, Meta, SortKey, SourceId};

use crate::columns::{ColumnBlocks, Field};
use crate::dirs::{DirScope, DirTable};
use crate::names::{Folded, NameArena};

/// The three files plus the liveness bits, opened together.
#[derive(Debug, Clone, Copy)]
pub struct Segment<'a> {
    pub names: NameArena<'a>,
    pub cols: ColumnBlocks<'a>,
    pub dirs: DirTable<'a>,
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
    NameHas(String),
    /// The name matches this wildcard pattern, anchored end to end.
    NameGlob(String),
    /// The extension, taken from the name, is one of these.
    Ext(Vec<String>),
    /// The whole path contains this, case-folded. The most expensive test.
    PathHas(String),
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

/// Alternatives ORed together; `bool` is negation.
#[derive(Debug)]
struct Clause {
    alts: Vec<(bool, Test)>,
}

/// A query, compiled.
#[derive(Debug, Default)]
pub struct Plan {
    clauses: Vec<Clause>,
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
        Ok(Plan { clauses })
    }

    pub fn is_empty(&self) -> bool {
        self.clauses.is_empty()
    }

    /// Does this row match?
    ///
    /// `name` is passed in because the caller already has it — the walk reads
    /// names sequentially, which is the whole reason the arena has no offsets.
    fn accepts(&self, seg: &Segment<'_>, row: usize, name: &str, fold: &mut Folded) -> bool {
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
        Match::NameContains(t) => Test::NameHas(t.clone()),
        // `*.rs` is the overwhelmingly common wildcard, and it is exactly an
        // extension test — which reads one short string instead of running a
        // pattern matcher over the whole name. Measured at 175 ms against 47.
        Match::NameGlob(p) => match p.strip_prefix("*.") {
            Some(ext) if !ext.is_empty() && !ext.contains(['*', '?', '.']) => {
                Test::Ext(vec![ext.to_owned()])
            }
            _ => Test::NameGlob(p.clone()),
        },
        Match::PathContains(t) => Test::PathHas(t.clone()),
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

fn evaluate(test: &Test, seg: &Segment<'_>, row: usize, name: &str, fold: &mut Folded) -> bool {
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
            let ext = scour_core::ext_of(name);
            list.contains(&ext)
        }
        Test::NameHas(t) => fold.fold(name).contains(t.as_str()),
        Test::NameGlob(p) => scour_query::glob_matches(p, fold.fold(name)),
        Test::PathHas(t) => {
            // The dearest test, and the reason it is sorted last: it builds a
            // string. Everything else reads what is already there.
            let path = seg.path(row, name);
            fold.fold(&path).contains(t.as_str())
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
    let mut fold = Folded::new();
    let mut counted = 0usize;
    let mut visited = 0u64;
    // Row numbers, not rows: four bytes each until the page is decided, which
    // is what keeps a query matching a million entries from building a million
    // strings.
    let mut kept: Vec<u32> = Vec::new();

    // The one order the row layout already satisfies. Everything else has to
    // see every match before it knows which forty win.
    let stored_order = want.sort == SortKey::Modified && want.descending;
    let need = want.offset + want.limit;
    let mut done = false;

    seg.names.walk(0, |row, name| {
        visited += 1;
        if !seg.is_alive(row) || !plan.accepts(seg, row, name, &mut fold) {
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
            kept.push(row as u32);
        }
        true
    });

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

/// Deterministic ordering, with an explicit tie-break on the path.
///
/// Timestamps tie constantly — a package install stamps thousands of files at
/// one instant — so without a second key the same query returns a different
/// page each time.
fn sort_hits(hits: &mut [Hit], key: SortKey, desc: bool) {
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
