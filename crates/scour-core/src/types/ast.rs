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
    Kind(Kind),
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
