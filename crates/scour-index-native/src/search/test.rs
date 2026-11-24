//! One term compiled against a segment, and a row tested against it.

use super::*;
use crate::columns::Field;
use crate::dirs::DirScope;
use crate::near::{Letters, Near};
use scour_core::text::{Folder, Unaccented};
use scour_core::{Cmp, Match};
use std::borrow::Cow;

/// **Not boxed, and clippy is told so.** A `Test` is evaluated per row of a walk
/// that reaches two million: the largest variant is a few words wide, where
/// boxing would cost an allocation and a pointer chase on every row.
#[allow(clippy::large_enum_variant)]
/// One condition, compiled against this segment.
#[derive(Debug)]
pub(super) enum Test {
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
    /// The name contains this, spelled exactly. Asked of the fold first, which
    /// the walk already carries and the trigram index is keyed on: a spelling is
    /// rebuilt from its block's start, and only for a name whose fold holds it.
    NameHasCased { want: String, folded: Needle },
    /// The name matches this pattern. The most expensive test there is and priced
    /// that way: an automaton over every name that reaches it, and nothing a
    /// trigram index can read narrows a regular expression.
    Regex(Box<regex::Regex>),
    /// The same, of the name with its marks: the pattern has one, or accents
    /// are exact. Dearer again by a spelling read for a name that has any.
    RegexMarked(Box<regex::Regex>),
    /// The name contains this **with its marks** — `şirket`, typed so, or any
    /// term when accents are exact. Asked of the fold first, as `case:` is.
    /// Boxed: two searchers would double what every other test is moved as.
    NameHasMarked(Box<Marked>),
    /// The wildcard pattern, of the name with its marks; `folded` is the
    /// pattern without them, for what the trigram index can say.
    NameGlobMarked { want: String, folded: String },
    /// Some part of the name is within a few typing mistakes of the term.
    NameNear(Box<Near>),
    /// The name holds these letters in this order.
    NameLetters(Letters),
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
    pub(super) fn cost(&self) -> u32 {
        match self {
            Test::Never => 0,
            Test::Num { .. } | Test::DirIn(_) | Test::KindIn(_) | Test::Bits { .. } => 1,
            Test::Depth { .. } => 2,
            Test::Ext { .. } => 3,
            Test::NameHas(_) | Test::NameGlob(_) => 10,
            Test::NameHasMarked(_) | Test::NameGlobMarked { .. } => 12,
            Test::NameLetters(_) => 14,
            Test::NameNear(_) => 30,
            Test::NameLen { .. } => 4,
            Test::NameHasCased { .. } => 20,
            // Two bitmap lookups and, rarely, a prefix test — dearer than a
            // number and cheaper than a name search.
            Test::PathIn(_) => 8,
            Test::Regex(_) => 60,
            Test::RegexMarked(_) => 80,
        }
    }
}

/// The complete folded extension, with eligibility read from the spelling.
pub(crate) fn folded_extension<'a>(seg: &Segment<'_>, row: usize, folded: &'a [u8]) -> &'a [u8] {
    let ext = crate::extension_order::extension(folded);
    if ext.is_empty() || !spelling_is_eligible(seg, row, ext) {
        return b"";
    }
    ext
}

/// Whether the extension counts, which its *spelling's* length decides. The
/// fold says so where the row's spelling is capitals at most; the spelling,
/// rebuilt from its block's start, is read for the rest.
fn spelling_is_eligible(seg: &Segment<'_>, row: usize, ext: &[u8]) -> bool {
    if seg.names.is_as_long(row) {
        return ext.len() <= 12;
    }
    let spelled = seg.names.get(row).unwrap_or_default();
    crate::extension_order::extension_is_eligible(spelled.as_bytes())
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
pub(super) fn admits(cmp: Cmp, value: i64, span: i64, lo: i64, hi: i64) -> bool {
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

/// The row's name with case set aside and marks kept: the fold itself where
/// the spelling is the fold or capitals of it, the spelling folded for the rest.
fn with_marks<'a>(seg: &Segment<'_>, row: usize, name: &'a [u8]) -> Cow<'a, [u8]> {
    if seg.names.is_as_long(row) {
        return Cow::Borrowed(name);
    }
    match seg.names.cased(row) {
        Some(cased) => Cow::Owned(cased),
        None => Cow::Borrowed(name),
    }
}

fn compile_regex(pattern: &str) -> Result<Box<regex::Regex>, scour_core::Error> {
    // A pattern that will not compile is not a query that matches everything:
    // it is a query the user has to be told about.
    regex::Regex::new(pattern)
        .map(Box::new)
        .map_err(|e| scour_core::Error::QuerySyntax {
            at: 0,
            expected: format!("a regular expression: {e}"),
        })
}

/// `exact` is the query's: a letter and the letter with a mark are different.
/// Without it only a term that carries a mark is asked of names with theirs.
/// `strict` is how a mistake is forgiven — see [`crate::near::Rule`].
pub(super) fn compile_match(
    m: &Match,
    exact: bool,
    strict: bool,
    seg: &Segment<'_>,
) -> Result<Test, scour_core::Error> {
    use scour_core::TimeField;
    let plain = |t: &str| Unaccented::of_folded(t).into_owned();
    let marked = |t: &str| exact || Unaccented::of_folded(t) != t;
    Ok(match m {
        Match::NameContains(t) if marked(t) => Test::NameHasMarked(Box::new(Marked {
            want: Needle::new(t),
            folded: Needle::new(&plain(t)),
        })),
        Match::NameContains(t) => Test::NameHas(Needle::new(t)),
        Match::NameNear(t, edits) => match Near::new(
            &plain(t),
            *edits,
            if strict {
                crate::near::Rule::Strict
            } else {
                crate::near::Rule::WordStart
            },
        ) {
            Some(near) if *edits > 0 => Test::NameNear(Box::new(near)),
            _ => Test::NameHas(Needle::new(&plain(t))),
        },
        Match::NameLetters(t) => Test::NameLetters(Letters::new(&plain(t))),
        // `*.rs` is the common wildcard and is exactly an extension test — one
        // short string read instead of a matcher over the whole name: 175 ms
        // against 47.
        Match::NameGlob(p) => match p.strip_prefix("*.") {
            Some(ext) if !ext.is_empty() && !ext.contains(['*', '?', '.']) => {
                // A pattern over the name, so a directory called `mod.rs` matches.
                Test::Ext {
                    list: vec![plain(ext)],
                    dirs: true,
                }
            }
            _ if marked(p) => Test::NameGlobMarked {
                want: p.clone(),
                folded: plain(p),
            },
            _ => Test::NameGlob(p.clone()),
        },
        // **A path is a directory and a name, and there are eight times fewer
        // directories than rows.** Building every row's path cost 1.66 s over
        // three million rows; the directory table plus a number per row, 120 ms.
        Match::PathContains(t) => {
            Test::PathIn(Box::new(PathSet::build(t, &plain(t), marked(t), seg)))
        }
        // An extension is compared without its marks, whatever the query says.
        Match::Ext(list) => Test::Ext {
            list: list.iter().map(|e| plain(e)).collect(),
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
        Match::NameContainsCased(t) => Test::NameHasCased {
            want: t.clone(),
            folded: Needle::new(&Unaccented.fold(t)),
        },
        Match::Depth(cmp, v) => Test::Depth {
            depths: seg.dirs.depths().into(),
            cmp: *cmp,
            value: *v,
        },
        Match::Regex(p) if marked(p) => Test::RegexMarked(compile_regex(p)?),
        Match::Regex(p) => Test::Regex(compile_regex(p)?),
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
pub(super) fn evaluate(test: &Test, seg: &Segment<'_>, row: usize, name: &[u8]) -> bool {
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
                Cmp::Eq if *span > 1 => got >= *value && got < value.saturating_add(*span),
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
            // and fifteen to a byte counter, and the person is asking. Of the
            // spelling: `ß` folds to two letters and a combining mark to none.
            let chars = |s: &[u8]| std::str::from_utf8(s).map_or(s.len(), |s| s.chars().count());
            let n = if seg.names.is_as_many_letters(row) {
                chars(name)
            } else {
                seg.names
                    .get(row)
                    .map_or(chars(name), |s| s.chars().count())
            };
            cmp.holds(n as i64, *value)
        }
        // The written spelling, compared as written. Folding is a character at
        // a time, so a name that holds it holds its fold.
        Test::NameHasCased { want, folded } => {
            folded.found_in(name)
                && seg
                    .names
                    .get(row)
                    .is_some_and(|written| written.contains(want.as_str()))
        }
        Test::Regex(re) => match std::str::from_utf8(name) {
            Ok(name) => re.is_match(name),
            Err(_) => false,
        },
        Test::RegexMarked(re) => match std::str::from_utf8(&with_marks(seg, row, name)) {
            Ok(name) => re.is_match(name),
            Err(_) => false,
        },
        Test::NameHasMarked(m) => {
            m.folded.found_in(name) && m.want.found_in(&with_marks(seg, row, name))
        }
        Test::NameGlobMarked { want, .. } => {
            match std::str::from_utf8(&with_marks(seg, row, name)) {
                Ok(name) => scour_query::glob_matches(want, name),
                Err(_) => false,
            }
        }
        Test::NameNear(near) => near.found_in(name),
        Test::NameLetters(letters) => letters.found_in(name),
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
            let ext = crate::extension_order::extension(name);
            !ext.is_empty()
                && list.iter().any(|e| e.as_bytes() == ext)
                && spelling_is_eligible(seg, row, ext)
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
            let ending = set.ending.get(dir).copied().unwrap_or(false);
            if set.marked {
                if set.in_name.is_none() && !ending {
                    return false;
                }
                let name = with_marks(seg, row, name);
                return set.in_name.as_ref().is_some_and(|n| n.found_in(&name))
                    || (ending && name.starts_with(&set.tail));
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
            ending && name.starts_with(&set.tail)
        }
    }
}
