//! An [`Ast`] turned into something tantivy can run.
//!
//! Almost every term lowers exactly. A substring becomes a phrase query over
//! consecutive positional trigrams, which was verified against a linear scan to
//! return precisely the same set; extensions, kinds and directory-ness become
//! term queries; sizes and dates become range queries over fast columns.
//!
//! Wildcards are the exception, and the only place this returns something
//! approximate. `*a?c*` cannot be expressed as postings, so the lowering
//! produces a *superset* — the literal runs it does contain — and hands back a
//! predicate to apply afterwards. The common shape `*.ext` is rewritten into an
//! extension term instead and needs no predicate at all, which is why the
//! approximate path is rarely taken.

use std::ops::Bound;

use scour_core::{Ast, Cmp, Error, Match, Result, TimeField};
use tantivy::query::{
    AllQuery, BooleanQuery, Occur, PhraseQuery, Query, QueryClone, RangeQuery, Scorer, TermQuery,
    Weight,
};
use tantivy::schema::{IndexRecordOption, Schema};
use tantivy::{DocSet, Score, Term};

use crate::schema::{IndexOptions, MIN_TERM_CHARS, field};

/// A query plus whatever it could not express.
pub struct Lowered {
    pub query: Box<dyn Query>,
    /// Wildcard patterns to check against the folded name. `bool` is negation.
    /// Empty means [`Lowered::query`] answers the question by itself.
    pub globs: Vec<(bool, String)>,
}

// `Box<dyn Query>` is not `Debug`; what a reader wants to see is whether the
// postings answer the question by themselves, and if not, what is left over.
impl std::fmt::Debug for Lowered {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Lowered")
            .field("exact", &self.is_exact())
            .field("globs", &self.globs)
            .finish()
    }
}

impl Lowered {
    pub fn is_exact(&self) -> bool {
        self.globs.is_empty()
    }

    /// Does a candidate survive the predicates the query could not express?
    pub fn accepts(&self, folded_name: &str) -> bool {
        self.globs
            .iter()
            .all(|(neg, p)| scour_query::glob_matches(p, folded_name) != *neg)
    }
}

pub fn lower(ast: &Ast, schema: &Schema, opts: &IndexOptions) -> Result<Lowered> {
    if ast.needs_content() && !opts.index_content {
        return Err(Error::ContentNotIndexed);
    }
    if ast.is_empty() {
        return Ok(Lowered {
            query: Box::new(AllQuery),
            globs: Vec::new(),
        });
    }

    let mut globs = Vec::new();
    let mut groups: Vec<(Occur, Box<dyn Query>)> = Vec::new();

    for group in &ast.groups {
        let mut alts: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for (negated, m) in &group.alts {
            match lower_match(m, schema, opts)? {
                Lowering::Exact(q) => {
                    alts.push((Occur::Should, if *negated { negate(q) } else { q }));
                }
                Lowering::Approximate { narrow, pattern } => {
                    // A negated wildcard cannot narrow anything: the superset
                    // says which documents *might* match, and "might not
                    // match" is not a subset of anything. So it contributes no
                    // postings and lives entirely in the predicate.
                    if *negated {
                        globs.push((true, pattern));
                        alts.push((Occur::Should, Box::new(AllQuery)));
                    } else {
                        globs.push((false, pattern));
                        alts.push((Occur::Should, narrow));
                    }
                }
            }
        }
        // A single alternative needs no wrapper; several are OR-ed.
        let q: Box<dyn Query> = match alts.len() {
            1 => alts
                .pop()
                .map(|(_, q)| q)
                .unwrap_or_else(|| Box::new(AllQuery)),
            _ => Box::new(BooleanQuery::new(alts)),
        };
        groups.push((Occur::Must, q));
    }

    let query: Box<dyn Query> = match groups.len() {
        1 => groups
            .pop()
            .map(|(_, q)| q)
            .unwrap_or_else(|| Box::new(AllQuery)),
        _ => Box::new(BooleanQuery::new(groups)),
    };
    Ok(Lowered { query, globs })
}

enum Lowering {
    Exact(Box<dyn Query>),
    /// A superset, plus the pattern that has to be checked afterwards.
    Approximate {
        narrow: Box<dyn Query>,
        pattern: String,
    },
}

fn lower_match(m: &Match, schema: &Schema, opts: &IndexOptions) -> Result<Lowering> {
    let f = |name: &str| {
        schema.get_field(name).map_err(|_| Error::IndexCorrupt {
            detail: format!("missing field {name}"),
        })
    };

    Ok(match m {
        Match::NameContains(t) => Lowering::Exact(trigram_phrase(f(field::NAME_NORM)?, t)?),

        Match::PathContains(t) => {
            if !opts.index_paths {
                return Err(Error::Unsupported {
                    what: "path search on an index built without paths",
                });
            }
            Lowering::Exact(trigram_phrase(f(field::PATH_NORM)?, t)?)
        }

        Match::ContentContains(t) => Lowering::Exact(trigram_phrase(f(field::CONTENT)?, t)?),

        Match::NameGlob(p) => {
            // `*.rs` is the overwhelmingly common wildcard, and it is exactly
            // an extension test — which the index answers from a term.
            if let Some(ext) = p.strip_prefix("*.")
                && !ext.contains(['*', '?', '.'])
                && !ext.is_empty()
            {
                return Ok(Lowering::Exact(ext_query(
                    f(field::EXT)?,
                    std::slice::from_ref(&ext.to_owned()),
                )));
            }
            Lowering::Approximate {
                narrow: glob_superset(f(field::NAME_NORM)?, p),
                pattern: p.clone(),
            }
        }

        Match::Ext(list) => Lowering::Exact(ext_query(f(field::EXT)?, list)),

        Match::IsDir(want) => Lowering::Exact(Box::new(TermQuery::new(
            Term::from_field_i64(f(field::IS_DIR)?, i64::from(*want)),
            IndexRecordOption::Basic,
        ))),

        Match::Kind(k) => Lowering::Exact(Box::new(TermQuery::new(
            Term::from_field_i64(f(field::KIND)?, k.as_u8() as i64),
            IndexRecordOption::Basic,
        ))),

        Match::Size(cmp, v) => Lowering::Exact(i64_range(f(field::SIZE)?, *cmp, *v, 1)),

        Match::Time(tf, cmp, v) => {
            let name = match tf {
                TimeField::Modified => field::MTIME,
                TimeField::Created => field::CTIME,
                TimeField::Accessed => field::ATIME,
            };
            // On a timestamp, equality means the calendar day it names.
            Lowering::Exact(i64_range(f(name)?, *cmp, *v, 86_400))
        }
    })
}

/// "Contains this substring", as consecutive trigrams at consecutive positions.
fn trigram_phrase(field: tantivy::schema::Field, folded: &str) -> Result<Box<dyn Query>> {
    let chars: Vec<char> = folded.chars().collect();
    if chars.len() < MIN_TERM_CHARS {
        return Err(Error::QueryTooShort {
            need: MIN_TERM_CHARS,
        });
    }
    let terms: Vec<Term> = chars
        .windows(3)
        .map(|w| Term::from_field_text(field, &w.iter().collect::<String>()))
        .collect();
    Ok(match terms.len() {
        1 => Box::new(TermQuery::new(
            terms.into_iter().next().expect("one term"),
            IndexRecordOption::WithFreqsAndPositions,
        )),
        _ => Box::new(PhraseQuery::new(terms)),
    })
}

/// The postings a wildcard pattern is guaranteed to be found in.
///
/// Each literal run of three or more characters must appear somewhere in the
/// name, so their phrase queries are AND-ed. A pattern with no such run — `a*b`,
/// `??x` — narrows nothing, and the predicate does all the work.
fn glob_superset(field: tantivy::schema::Field, pattern: &str) -> Box<dyn Query> {
    let runs: Vec<Box<dyn Query>> = pattern
        .split(['*', '?'])
        .filter(|r| r.chars().count() >= MIN_TERM_CHARS)
        .filter_map(|r| trigram_phrase(field, r).ok())
        .collect();
    match runs.len() {
        0 => Box::new(AllQuery),
        1 => runs.into_iter().next().expect("one run"),
        _ => Box::new(BooleanQuery::new(
            runs.into_iter().map(|q| (Occur::Must, q)).collect(),
        )),
    }
}

fn ext_query(field: tantivy::schema::Field, list: &[String]) -> Box<dyn Query> {
    if list.is_empty() {
        // `ext:` with nothing after it matches nothing, rather than everything.
        return Box::new(BooleanQuery::new(Vec::new()));
    }
    let clauses: Vec<(Occur, Box<dyn Query>)> = list
        .iter()
        .map(|e| {
            let q: Box<dyn Query> = Box::new(TermQuery::new(
                Term::from_field_text(field, e),
                IndexRecordOption::Basic,
            ));
            (Occur::Should, q)
        })
        .collect();
    Box::new(BooleanQuery::new(clauses))
}

/// A comparison as a range over an i64 fast column.
///
/// `span` is what equality means: one, for a size; a day, for a timestamp.
fn i64_range(field: tantivy::schema::Field, cmp: Cmp, v: i64, span: i64) -> Box<dyn Query> {
    let t = |x: i64| Term::from_field_i64(field, x);
    let (lo, hi) = match cmp {
        Cmp::Lt => (Bound::Unbounded, Bound::Excluded(t(v))),
        Cmp::Le => (Bound::Unbounded, Bound::Included(t(v))),
        Cmp::Gt => (Bound::Excluded(t(v)), Bound::Unbounded),
        Cmp::Ge => (Bound::Included(t(v)), Bound::Unbounded),
        Cmp::Eq if span > 1 => (Bound::Included(t(v)), Bound::Excluded(t(v + span))),
        Cmp::Eq => (Bound::Included(t(v)), Bound::Included(t(v))),
    };
    Box::new(RangeQuery::new(lo, hi))
}

/// "Everything except this". A `MustNot` clause needs something positive to
/// subtract from.
///
/// The excluded side is wrapped, and that wrapper is not cosmetic. Tantivy's
/// `Exclude` asks the excluded scorer whether it contains the *first* document
/// of the positive side — document 0, when that side is `AllQuery` — but a
/// phrase scorer has already advanced to its own first match by the time it is
/// constructed. `PhraseScorer::seek_danger` then asserts that the target is not
/// behind it and panics. [`Guard`] restores the guarded default, which checks
/// the current position before seeking at all.
fn negate(q: Box<dyn Query>) -> Box<dyn Query> {
    Box::new(BooleanQuery::new(vec![
        (Occur::Must, Box::new(AllQuery) as Box<dyn Query>),
        (Occur::MustNot, Box::new(Guard(q)) as Box<dyn Query>),
    ]))
}

/// A query whose scorer tolerates being asked about a document it has already
/// passed.
///
/// Everything is forwarded except `seek_danger`, which is deliberately *not*
/// forwarded: leaving it to the trait's default implementation is the whole
/// point, because that default checks `doc() < target` before seeking, and the
/// implementations that panic are the ones that override it without checking.
#[derive(Debug)]
struct Guard(Box<dyn Query>);

// `Box<dyn Query>` is not `Clone`, so the blanket `QueryClone` impl does not
// apply and this has to be written out.
impl Clone for Guard {
    fn clone(&self) -> Self {
        Guard(self.0.box_clone())
    }
}

impl Query for Guard {
    fn weight(
        &self,
        scoring: tantivy::query::EnableScoring<'_>,
    ) -> tantivy::Result<Box<dyn Weight>> {
        Ok(Box::new(GuardWeight(self.0.weight(scoring)?)))
    }

    fn query_terms<'a>(&'a self, visitor: &mut dyn FnMut(&'a Term, bool)) {
        self.0.query_terms(visitor);
    }
}

struct GuardWeight(Box<dyn Weight>);

impl Weight for GuardWeight {
    fn scorer(
        &self,
        reader: &tantivy::SegmentReader,
        boost: Score,
    ) -> tantivy::Result<Box<dyn Scorer>> {
        Ok(Box::new(GuardScorer(self.0.scorer(reader, boost)?)))
    }

    fn explain(
        &self,
        reader: &tantivy::SegmentReader,
        doc: tantivy::DocId,
    ) -> tantivy::Result<tantivy::query::Explanation> {
        self.0.explain(reader, doc)
    }
}

struct GuardScorer(Box<dyn Scorer>);

impl DocSet for GuardScorer {
    fn advance(&mut self) -> tantivy::DocId {
        self.0.advance()
    }

    fn seek(&mut self, target: tantivy::DocId) -> tantivy::DocId {
        let doc = self.0.doc();
        if doc >= target {
            doc
        } else {
            self.0.seek(target)
        }
    }

    fn doc(&self) -> tantivy::DocId {
        self.0.doc()
    }

    fn size_hint(&self) -> u32 {
        self.0.size_hint()
    }

    fn cost(&self) -> u64 {
        self.0.cost()
    }
}

impl Scorer for GuardScorer {
    fn score(&mut self) -> Score {
        self.0.score()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scour_query::parse_at;

    fn schema() -> Schema {
        crate::schema::build_schema(&IndexOptions::default())
    }

    fn lowered(q: &str) -> Result<Lowered> {
        lower(&parse_at(q, 0), &schema(), &IndexOptions::default())
    }

    #[test]
    fn ordinary_queries_lower_exactly() {
        for q in [
            "rapor",
            "ext:pdf",
            "size:>1mb",
            "dm:7d",
            "kind:code",
            "folder:",
            "path:src",
            "rapor ext:pdf",
            "abc|def",
            "!tmp",
            "dc:2026-01-31",
        ] {
            assert!(
                lowered(q).expect(q).is_exact(),
                "{q} should need no predicate"
            );
        }
    }

    #[test]
    fn a_dot_extension_wildcard_becomes_an_extension_term() {
        // The common case, and the reason the approximate path is rare.
        let l = lowered("*.rs").expect("lower");
        assert!(
            l.is_exact(),
            "*.rs is an extension test, not a pattern match"
        );
    }

    #[test]
    fn a_real_wildcard_keeps_a_predicate() {
        let l = lowered("rap*or?").expect("lower");
        assert!(!l.is_exact());
        assert!(l.accepts("raportor1"));
        assert!(
            !l.accepts("raportor"),
            "the trailing ? demands one more character"
        );
    }

    #[test]
    fn a_negated_wildcard_is_checked_the_other_way_round() {
        let l = lowered("!*.tmp.bak").expect("lower");
        assert!(!l.is_exact());
        assert!(l.accepts("keep.rs"));
        assert!(!l.accepts("x.tmp.bak"));
    }

    #[test]
    fn short_terms_are_refused_rather_than_guessed_at() {
        assert_eq!(lowered("ab").unwrap_err(), Error::QueryTooShort { need: 3 });
        assert_eq!(
            lowered("path:ab").unwrap_err(),
            Error::QueryTooShort { need: 3 }
        );
        // But a short term inside a field that does not need trigrams is fine.
        assert!(lowered("ext:rs").is_ok());
        assert!(lowered("size:>0").is_ok());
    }

    #[test]
    fn content_is_refused_unless_the_index_has_it() {
        assert_eq!(
            lowered("content:gizli").unwrap_err(),
            Error::ContentNotIndexed
        );
        let with = IndexOptions {
            index_content: true,
            ..Default::default()
        };
        assert!(
            lower(
                &parse_at("content:gizli", 0),
                &crate::schema::build_schema(&with),
                &with
            )
            .is_ok()
        );
    }

    #[test]
    fn path_search_is_refused_on_an_index_without_paths() {
        let lean = IndexOptions {
            index_paths: false,
            ..Default::default()
        };
        let err = lower(
            &parse_at("path:src", 0),
            &crate::schema::build_schema(&lean),
            &lean,
        )
        .unwrap_err();
        assert_eq!(err.code(), "unsupported");
    }

    #[test]
    fn an_empty_query_matches_everything() {
        let l = lowered("").expect("lower");
        assert!(l.is_exact());
        assert!(l.accepts("anything at all"));
    }
}
