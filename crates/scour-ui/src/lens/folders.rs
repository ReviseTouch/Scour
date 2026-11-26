//! What the folder list's buttons write: "Add" puts a folder into the place
//! group, "Exclude" writes or takes away its `!under:`, and where the caret
//! rests afterwards so the lens hanging from the group does not move.

use super::caret::{Caret, edited_term};
use super::group::{Pick, is_place, norm, place_toggle, quoted_path, remove_term, runs, scope};
use super::{Term, place_parts, replace, terms};
use crate::disks::lies_under;
use crate::path::{full_form, home_form};

/// [`place_add_at`] with `typing` guessed from its letters after the last
/// `/`; superseded by it, which a finished path cannot fool.
pub fn place_add(text: &str, home: &str, path: &str, typing: Option<&Term>) -> (String, usize) {
    let typing = typing.filter(|t| is_place(t) && !place_parts(&t.value).1.is_empty());
    add(text, home, path, typing)
}

/// "Add": the folder into the place group, and the caret after it. A member
/// it lies in, or that lies in it, an empty `under:`, and the member being
/// edited at `caret` ([`edited_term`]) give way to it in place; else it
/// joins. Never takes a folder out; drops the folder's own `!under:`.
pub fn place_add_at(text: &str, home: &str, path: &str, caret: Caret) -> (String, usize) {
    let typing = edited_term(text, caret).filter(is_place);
    add(text, home, path, typing.as_ref())
}

fn add(text: &str, home: &str, path: &str, typing: Option<&Term>) -> (String, usize) {
    let want = norm(home, path);
    let new = format!("under:{}", quoted_path(&home_form(home, &want)));
    let Some(run) = runs(text).into_iter().find(|r| r.iter().any(is_place)) else {
        return unexclude(at_head(text, &new, 0), home, &want);
    };
    let gives_way = |t: &Term| {
        let held = norm(home, &t.value);
        t.value.is_empty()
            || typing == Some(t)
            || lies_under(&want, &held)
            || lies_under(&held, &want)
    };
    let places: Vec<&Term> = run.iter().filter(|t| is_place(t)).collect();
    let give: Vec<&Term> = places.iter().copied().filter(|t| gives_way(t)).collect();
    let out = match give.split_first() {
        None => {
            let end = places[places.len() - 1].end;
            let out = format!("{}|{new}{}", &text[..end], &text[end..]);
            (out, end + 1 + new.len())
        }
        Some((first, rest)) => {
            // Later ones first, so the first one's offsets stay true.
            let mut out = text.to_string();
            for t in rest.iter().rev() {
                out = remove_term(&out, t).0;
            }
            replace(&out, first, &new)
        }
    };
    unexclude(out, home, &want)
}

/// "Exclude": a standalone `!under:` of the folder written at the end, or
/// taken away when it is there. A member of the place group naming that same
/// folder leaves the group first: searching in it and outside it finds nothing.
pub fn place_exclude(text: &str, home: &str, path: &str) -> (String, usize) {
    let want = norm(home, path);
    let s = scope(text, home);
    if let Some(h) = s.outside.iter().find(|h| h.path == want) {
        return remove_term(text, &h.term);
    }
    let text = if s.holds(&want) {
        place_toggle(text, home, &want, Pick::Also).0
    } else {
        text.to_string()
    };
    let new = format!("!under:{}", quoted_path(&home_form(home, &want)));
    // Beside the group and the exclusions wherever they stand, else where
    // the place is said.
    let group = runs(&text)
        .into_iter()
        .find(|r| r.iter().any(is_place))
        .and_then(|r| r.last().map(|t| t.end));
    let excluded = terms(&text)
        .iter()
        .filter(|t| t.not && t.field == Some("under"))
        .map(|t| t.end)
        .max();
    let at = place_head(&text)
        .max(group.unwrap_or(0))
        .max(excluded.unwrap_or(0));
    at_head(&text, &new, at)
}

/// The end of the group's first member, else [`place_settle`]'s empty
/// place; superseded by `place_settle`, which also stays on an exclusion.
pub fn place_anchor(text: &str, home: &str, at: usize) -> (String, usize) {
    if let Some(h) = scope(text, home).within.first() {
        return (text.to_string(), h.term.end);
    }
    empty_place(text, at)
}

/// Where the caret rests, as [`Caret::resting`], after a press of the place
/// lens wrote `text`. `not`: the lens was on an `!under:` and stays on the
/// one nearest `at`. Otherwise at the end of the group's last `under:` — the
/// capsule's trailing edge. With no group left, at an empty `under:` already
/// there, or at one written at `at`, where the group was.
pub fn place_settle(text: &str, at: usize, not: bool) -> (String, usize) {
    let all = terms(text);
    let excluded = all.iter().filter(|t| t.not && t.field == Some("under"));
    if let Some(t) = excluded
        .min_by_key(|t| t.start.abs_diff(at))
        .filter(|_| not)
    {
        return (text.to_string(), t.end);
    }
    if let Some(run) = runs(text).into_iter().find(|r| r.iter().any(is_place)) {
        let last = run.iter().rev().find(|t| is_place(t)).map_or(0, |t| t.end);
        return (text.to_string(), last);
    }
    empty_place(text, at)
}

/// An empty `under:` already in the text, or one written at `at`.
fn empty_place(text: &str, at: usize) -> (String, usize) {
    let all = terms(text);
    if let Some(t) = all.iter().find(|t| is_place(t) && t.value.is_empty()) {
        return (text.to_string(), t.end);
    }
    let mut at = at.min(text.len());
    while !text.is_char_boundary(at) {
        at -= 1;
    }
    if let Some(t) = all.iter().find(|t| t.start < at && at < t.end) {
        at = t.end;
    }
    let pad = |c: Option<char>| {
        if c.is_some_and(|c| !c.is_whitespace()) {
            " "
        } else {
            ""
        }
    };
    let (lead, tail) = (
        pad(text[..at].chars().last()),
        pad(text[at..].chars().next()),
    );
    let out = format!("{}{lead}under:{tail}{}", &text[..at], &text[at..]);
    (out, at + lead.len() + "under:".len())
}

/// The folder a place term's list shows, as a full path, and what is typed
/// after its last `/` to narrow it. A term with no folder yet shows `home`,
/// or `/` without one.
pub fn place_at(value: &str, home: &str) -> (String, String) {
    let (base, part) = place_parts(value);
    let shown = match (base.is_empty(), home.is_empty()) {
        (true, true) => "/".to_string(),
        (true, false) => home.to_string(),
        _ => full_form(home, &base),
    };
    (shown, part)
}

/// `new` as a term of its own at the end, and the caret after it.
/// Where the query's place ends: the group and the exclusions standing first,
/// with nothing but spaces and `|` between them; nought when it does not
/// start with one.
pub fn place_head(text: &str) -> usize {
    let mut end = 0;
    for t in terms(text) {
        let between = &text[end..t.start];
        let placed = is_place(&t) || (t.not && t.field == Some("under"));
        if !placed || !between.chars().all(|c| c.is_whitespace() || c == '|') {
            break;
        }
        end = t.end;
    }
    end
}

/// `new` written where the place is said: first in the query, or after the
/// place already there (`head`); the caret after it.
pub fn at_head(text: &str, new: &str, head: usize) -> (String, usize) {
    let (before, after) = text.split_at(head.min(text.len()));
    let lead = if before.is_empty() { "" } else { " " };
    let tail = if after.is_empty() || after.starts_with(char::is_whitespace) {
        ""
    } else {
        " "
    };
    let at = before.len() + lead.len() + new.len();
    (format!("{before}{lead}{new}{tail}{after}"), at)
}

/// The standalone `!under:` of `want` taken out, the caret kept on its term.
fn unexclude((text, caret): (String, usize), home: &str, want: &str) -> (String, usize) {
    let Some(h) = scope(&text, home)
        .outside
        .into_iter()
        .find(|h| h.path == want)
    else {
        return (text, caret);
    };
    let (out, cut) = remove_term(&text, &h.term);
    let gone = text.len() - out.len();
    let caret = if cut < caret { caret - gone } else { caret };
    (out, caret)
}

#[cfg(test)]
#[path = "folders_tests.rs"]
mod tests;
