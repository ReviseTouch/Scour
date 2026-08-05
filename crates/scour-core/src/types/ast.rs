//! What a query means, once the syntax is gone.
//!
//! The parser lives in `scour-query`; the shape it produces lives here, because
//! both the parser and every [`Index`] implementation have to agree on it. An
//! index lowers this tree into whatever its engine actually speaks — tantivy
//! phrase queries, SQL, a linear scan — and nothing above it needs to know
//! which.
//!
//! [`Index`]: crate::traits::Index

use serde::{Deserialize, Serialize};

use super::Kind;

/// A comparison operator on a numeric or temporal field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cmp {
    Lt,
    Le,
    Gt,
    Ge,
    /// Equality. On a timestamp this means "within that calendar day", because
    /// nobody asks for a file modified at exactly one second past midnight.
    Eq,
}

impl Cmp {
    /// Does `lhs <op> rhs` hold?
    pub fn holds(self, lhs: i64, rhs: i64) -> bool {
        match self {
            Cmp::Lt => lhs < rhs,
            Cmp::Le => lhs <= rhs,
            Cmp::Gt => lhs > rhs,
            Cmp::Ge => lhs >= rhs,
            Cmp::Eq => lhs == rhs,
        }
    }

    pub fn symbol(self) -> &'static str {
        match self {
            Cmp::Lt => "<",
            Cmp::Le => "<=",
            Cmp::Gt => ">",
            Cmp::Ge => ">=",
            Cmp::Eq => "=",
        }
    }
}

/// Which timestamp a temporal term is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TimeField {
    Modified,
    Created,
    Accessed,
}

/// One condition.
// Externally tagged, which for these shapes is both the most compact JSON and
// the most readable: `{"name_contains": "report"}`, `{"size": ["gt", 1048576]}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Match {
    /// Substring of the case-folded name. The everyday case.
    NameContains(String),
    /// Wildcard pattern over the case-folded name, anchored end to end —
    /// `*.rs` matches the whole name, not a fragment of it. This is Everything's
    /// behaviour and users rely on it.
    NameGlob(String),
    /// Substring of the case-folded path.
    PathContains(String),
    /// Anywhere below this directory.
    ///
    /// Not a substring test: the index holds every ancestor directory of every
    /// entry as its own token, so scoping a search to a folder is one term
    /// rather than a scan. `/a` does not match `/ab`, and the directory itself
    /// is not among its own descendants.
    ///
    /// The value is a path and is compared as the filesystem stores it, not
    /// case-folded — unlike a name, which is.
    Under(String),
    /// Directly inside this directory, one level down.
    ParentIs(String),
    /// Any one of these extensions, without the dot.
    Ext(Vec<String>),
    IsDir(bool),
    Size(Cmp, i64),
    /// Any one of these kinds.
    ///
    /// A list rather than one kind, because a single word names several:
    /// `kind:media` has to go on meaning audio *or* video *or* the retired
    /// discriminant an older index still holds, and `kind:text` names four at
    /// once. See [`Kind::from_name`].
    Kind(Vec<Kind>),
    Time(TimeField, Cmp, i64),
    /// Substring of the *contents* of a document.
    ///
    /// Parsed and represented from the first day so that the shape of the
    /// language does not have to change when content indexing arrives. An index
    /// that was not built with content enabled rejects it with
    /// [`Error::ContentNotIndexed`], which is a far better answer than silently
    /// returning nothing.
    ///
    /// [`Error::ContentNotIndexed`]: crate::types::Error::ContentNotIndexed
    ContentContains(String),
    /// How many components the path has, counted from the root.
    ///
    /// `find -maxdepth` counts from where the walk started; an index has no
    /// start, so this counts `/`. `/home` is 1 and `/home/u/a.rs` is 3, which
    /// makes `under:/home/u depth:<=3` the way to say "not below this folder".
    Depth(Cmp, i64),
    /// The name matches this regular expression.
    ///
    /// Anchored nowhere: `regex:^main` and `regex:rs$` both say what they look
    /// like, exactly as `grep -E` does. Matched against the **folded** name,
    /// so it is case-insensitive like everything else — including for Turkish
    /// dotted and dotless i, which no `(?i)` flag gets right.
    Regex(String),
    /// How many characters the name has. Everything spells it `len:`.
    NameLen(Cmp, i64),
    /// The name contains this, **spelled exactly like this**.
    ///
    /// Everything's `case:`. The index keeps both the name as written and the
    /// folded one, so this is a comparison against the first rather than a
    /// second index — the only cost is that it reads the arena the folded
    /// search would have skipped.
    NameContainsCased(String),
    /// A stored number compared to a value: `uid:1000`, `gid:>100`.
    ///
    /// The index has held these columns since the first version and nothing
    /// could ask about them. They cost nothing to keep — a column that barely
    /// varies packs to almost zero — so the only thing missing was a way to
    /// say it.
    Num(NumField, Cmp, i64),
    /// A stored number masked and compared: permissions, and the type bits.
    ///
    /// One variant for the whole family because that is what it is. `find`
    /// spells the three cases `-perm 644`, `-perm -200` and `-perm /222`, and
    /// they are exactly *equal after masking*, *all of these bits*, and *any
    /// of these bits*:
    ///
    /// | query | mask | want | any |
    /// |---|---|---|---|
    /// | `perm:644` | `0o7777` | `0o644` | false |
    /// | `perm:-200` | `0o200` | `0o200` | false |
    /// | `perm:/222` | `0o222` | — | true |
    /// | `type:l` | `0o170000` | `0o120000` | false |
    /// | `suid:` | `0o4000` | `0o4000` | false |
    Bits {
        field: NumField,
        mask: i64,
        want: i64,
        /// `mask & value != 0` rather than `mask & value == want`.
        any: bool,
    },
}

/// A column a query can ask a number about.
///
/// Deliberately not every column. `DirId` is an implementation detail and
/// `Source` is one the user did not choose; these are the ones a person or a
/// script has a reason to name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NumField {
    Mode,
    Uid,
    Gid,
    /// Children, for a directory. `-1` where it was never counted.
    Items,
    /// Blocks on disk, which is not the size for a sparse or compressed file.
    Disk,
}

/// Alternatives that are OR-ed together. `bool` is negation, per alternative.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Group {
    pub alts: Vec<(bool, Match)>,
}

/// A whole query: groups AND-ed together.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Ast {
    pub groups: Vec<Group>,
}

impl Ast {
    pub fn is_empty(&self) -> bool {
        self.groups.is_empty()
    }

    /// Visit every `Match` in the tree, negated or not.
    pub fn matches(&self) -> impl Iterator<Item = &Match> {
        self.groups
            .iter()
            .flat_map(|g| g.alts.iter().map(|(_, m)| m))
    }

    /// Does this query ask about document contents?
    pub fn needs_content(&self) -> bool {
        self.matches()
            .any(|m| matches!(m, Match::ContentContains(_)))
    }

    /// The plain positive name terms, at least `min_chars` characters long.
    ///
    /// These are the terms an index can use to *narrow* before evaluating the
    /// rest — a term that is negated, or one of several alternatives, cannot
    /// narrow anything, so it is not offered.
    pub fn narrowing_terms(&self, min_chars: usize) -> Vec<&str> {
        self.groups
            .iter()
            .filter(|g| g.alts.len() == 1)
            .filter_map(|g| match &g.alts[0] {
                (false, Match::NameContains(t)) if t.chars().count() >= min_chars => {
                    Some(t.as_str())
                }
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ast(alts: Vec<Vec<(bool, Match)>>) -> Ast {
        Ast {
            groups: alts.into_iter().map(|alts| Group { alts }).collect(),
        }
    }

    #[test]
    fn narrowing_terms_are_plain_positive_and_long_enough() {
        let q = ast(vec![
            vec![(false, Match::NameContains("report".into()))],
            vec![(true, Match::NameContains("tmp".into()))], // negated
            vec![(false, Match::NameContains("ab".into()))], // too short
            vec![
                (false, Match::NameContains("one".into())),
                (false, Match::NameContains("two".into())),
            ], // alternatives
            vec![(false, Match::Ext(vec!["pdf".into()]))],
        ]);
        assert_eq!(q.narrowing_terms(3), vec!["report"]);
    }

    #[test]
    fn content_is_detected_anywhere_in_the_tree() {
        assert!(!ast(vec![vec![(false, Match::IsDir(true))]]).needs_content());
        assert!(
            ast(vec![
                vec![(false, Match::IsDir(false))],
                vec![(true, Match::ContentContains("secret".into()))],
            ])
            .needs_content()
        );
    }

    #[test]
    fn comparison_semantics() {
        assert!(Cmp::Ge.holds(5, 5));
        assert!(!Cmp::Gt.holds(5, 5));
        assert_eq!(Cmp::Le.symbol(), "<=");
    }
}
