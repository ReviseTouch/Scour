//! Where the caret rests after a panel writes, so the same lens stays open:
//! at the end of the term just written, as `place_settle` rests at the end
//! of the place group. A term that went leaves the lens on the nearest term
//! of its field, or on an empty one written where it stood.

use super::group::runs;
use super::rest::field_terms;
use super::{Term, replace, value_toggle};

/// `with` written in place of `t` — a time or size choice; empty takes the
/// term out — and the caret at its end. A term taken out leaves an empty
/// `field:` where it stood, so the lens stays open on no filter.
pub fn choice_settle(text: &str, t: &Term, with: &str, field: &str) -> (String, usize) {
    let (out, at) = replace(text, t, with);
    if with.is_empty() {
        return rest_empty(&out, at, field);
    }
    (out, at)
}

/// [`value_toggle`], and where the type lens rests after it: on the term of
/// the side just changed nearest the change — after an exclude the `!kind:`
/// or `!ext:` term, else the term without `!`. With none of that side left,
/// on the nearest term of `field` of the other side; with none at all, on an
/// empty `field:` written where the change was. Toggling an extension from
/// the kind tiles rests on the `ext:` term: the shared view, its row in focus.
pub fn value_settle(text: &str, field: &str, value: &str, exclude: bool) -> (String, usize) {
    let (out, at) = value_toggle(text, field, value, exclude);
    let all = field_terms(&out, field);
    let near = |not: bool| {
        all.iter()
            .filter(|t| t.not == not)
            .min_by_key(|t| gap(t, at))
            .map(|t| t.end)
    };
    match near(exclude).or_else(|| near(!exclude)) {
        Some(end) => (out, end),
        None => rest_empty(&out, at, field),
    }
}

fn gap(t: &Term, at: usize) -> usize {
    if (t.start..=t.end).contains(&at) {
        0
    } else {
        t.start.abs_diff(at).min(t.end.abs_diff(at))
    }
}

/// An empty `field:` written at `at` — after the run `at` falls inside, so
/// no `|` is broken — one space either side where text touches it, and the
/// caret after its colon.
pub(super) fn rest_empty(text: &str, at: usize, field: &str) -> (String, usize) {
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    for r in runs(text) {
        let (s, e) = (r[0].start, r[r.len() - 1].end);
        if s < at && at < e {
            at = e;
        }
    }
    let touches = |c: Option<char>| c.is_some_and(|c| !c.is_whitespace());
    let lead = if touches(text[..at].chars().last()) {
        " "
    } else {
        ""
    };
    let tail = if touches(text[at..].chars().next()) {
        " "
    } else {
        ""
    };
    let empty = format!("{field}:");
    let out = format!("{}{lead}{empty}{tail}{}", &text[..at], &text[at..]);
    (out, at + lead.len() + empty.len())
}

#[cfg(test)]
#[path = "settle_tests.rs"]
mod tests;
