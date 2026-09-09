//! What a query means, once the syntax is gone. The parser lives in `scour-query`;
//! the shape it produces lives here, because the parser and every
//! [`Index`](crate::traits::Index) implementation have to agree on it.

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
// Externally tagged: `{"name_contains": "report"}`, `{"size": ["gt", 1048576]}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Match {
    /// Substring of the case-folded name. The everyday case.
    NameContains(String),
    /// Wildcard pattern over the case-folded name, anchored end to end as Everything
    /// anchors it: `*.rs` matches the whole name, not a fragment of it.
    NameGlob(String),
    /// Substring of the case-folded path.
    PathContains(String),
    /// Anywhere below this directory. Not a substring test: every ancestor is its own
    /// token, `/a` does not match `/ab`, and a directory is not its own descendant.
    /// The value is compared as the filesystem stores it, not case-folded.
    Under(String),
    /// Directly inside this directory, one level down.
    ParentIs(String),
    /// Any one of these extensions, without the dot.
    Ext(Vec<String>),
    IsDir(bool),
    Size(Cmp, i64),
    /// Any one of these kinds — a list because one word names several: `kind:media`
    /// is audio or video, `kind:text` names four. See [`Kind::from_name`].
    Kind(Vec<Kind>),
    Time(TimeField, Cmp, i64),
    /// Substring of the *contents* of a document. An index built without content
    /// rejects it with [`Error::ContentNotIndexed`](crate::types::Error::ContentNotIndexed)
    /// rather than answering nothing.
    ContentContains(String),
    /// How many components the path has, counted from `/` rather than from where a
    /// walk started: `/home` is 1, `/home/u/a.rs` is 3.
    Depth(Cmp, i64),
    /// The name matches this regular expression, anchored nowhere, as `grep -E` is.
    /// Matched against the folded name, so it is case-insensitive — including for
    /// dotted and dotless i, which no `(?i)` flag gets right.
    Regex(String),
    /// How many characters the name has. Everything spells it `len:`.
    NameLen(Cmp, i64),
    /// The name contains this, **spelled exactly like this**: Everything's `case:`.
    /// Compared against the name as written, which the index keeps beside the folded one.
    NameContainsCased(String),
    /// A stored number compared to a value: `uid:1000`, `gid:>100`.
    Num(NumField, Cmp, i64),
    /// A stored number masked and compared, covering `find`'s three forms: `perm:644`
    /// is mask `0o7777` want `0o644`; `perm:-200` is mask and want `0o200`; `perm:/222`
    /// is mask `0o222` with `any`. `type:l` is mask `0o170000` want `0o120000`.
    Bits {
        field: NumField,
        mask: i64,
        want: i64,
        /// `mask & value != 0` rather than `mask & value == want`.
        any: bool,
    },
}

/// A column a query can ask a number about. Not every column: `DirId` is an
/// implementation detail and `Source` is not something the user chose.
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

    /// The plain positive name terms, at least `min_chars` characters long: a negated
    /// term, or one of several alternatives, narrows nothing and is not offered.
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
