//! What a panel counts its choices against: the query less every term of
//! its own field, so each choice is weighed as though it were the only one.

use super::group::runs;
use super::{Term, terms};

/// Whether a term is `empty:` (or the service's `bos:`), `!` or not. The lens
/// knows no such field, so its [`Term::field`] is `None`.
pub fn is_empty_term(text: &str, t: &Term) -> bool {
    let raw = text[t.start..t.end].trim_start_matches('!');
    raw.strip_suffix(':')
        .is_some_and(|name| matches!(name.to_lowercase().as_str(), "empty" | "bos"))
}

/// Whether `t` is of the panel `field` (canonical: `dm`, `size`, `kind`,
/// `ext`), written with `!` or not. The size panel's field holds `empty:`.
pub fn of_field(text: &str, t: &Term, field: &str) -> bool {
    t.field == Some(field) || (field == "size" && is_empty_term(text, t))
}

/// The query less every run that holds a term of `field`, `!` or not: what
/// the panel's counts are asked over. A whole run goes, as in
/// [`without_places`](super::without_places), so no `|` is left to join a
/// stranger; the runs left are joined by one space.
pub fn without_field(text: &str, field: &str) -> String {
    runs(text)
        .into_iter()
        .filter(|r| !r.iter().any(|t| of_field(text, t, field)))
        .map(|r| &text[r[0].start..r[r.len() - 1].end])
        .collect::<Vec<_>>()
        .join(" ")
}

/// The terms of `field` in the query, in order.
pub fn field_terms(text: &str, field: &str) -> Vec<Term> {
    terms(text)
        .into_iter()
        .filter(|t| of_field(text, t, field))
        .collect()
}

/// `term` at the end of `text`, one space before it whatever `text` ends
/// with, and the caret after it.
pub(super) fn appended(text: &str, term: &str) -> (String, usize) {
    let base = text.trim_end();
    let out = if base.is_empty() {
        term.to_string()
    } else {
        format!("{base} {term}")
    };
    let caret = out.len();
    (out, caret)
}

/// `rest` and one more term, one space between: a count's query.
pub fn with_term(rest: &str, term: &str) -> String {
    match (rest.trim(), term.trim()) {
        ("", t) => t.to_string(),
        (r, "") => r.to_string(),
        (r, t) => format!("{r} {t}"),
    }
}
