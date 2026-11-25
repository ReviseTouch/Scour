//! What the folder list's buttons write: "Add" puts a folder into the place
//! group, "Exclude" writes or takes away its `!under:`, and where the caret
//! rests afterwards so the lens hanging from the group does not move.

use super::group::{Pick, is_place, norm, place_toggle, quoted_path, remove_term, runs, scope};
use super::{Term, place_parts, replace, terms};
use crate::disks::lies_under;
use crate::path::{full_form, home_form};

/// "Add": the folder into the place group, and the caret after it. A member
/// it lies in, or that lies in it, an empty `under:`, and `typing` (the lens's
/// term while letters after its last `/` are typed) give way to it in place;
/// else it joins. Never takes a folder out; drops the folder's own `!under:`.
pub fn place_add(text: &str, home: &str, path: &str, typing: Option<&Term>) -> (String, usize) {
    let want = norm(home, path);
    let new = format!("under:{}", quoted_path(&home_form(home, &want)));
    let typing = typing.filter(|t| is_place(t) && !place_parts(&t.value).1.is_empty());
    let Some(run) = runs(text).into_iter().find(|r| r.iter().any(is_place)) else {
        return unexclude(at_end(text, &new), home, &want);
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
    at_end(&text, &new)
}

/// Where the caret rests after a press of the place lens wrote `text`: at the
/// end of the group's first member, which starts where the group did. With
/// no group left, at an empty `under:` already there, or at one written at
/// `at`, where the group was — so the lens stays open on a place.
pub fn place_anchor(text: &str, home: &str, at: usize) -> (String, usize) {
    if let Some(h) = scope(text, home).within.first() {
        return (text.to_string(), h.term.end);
    }
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
fn at_end(text: &str, new: &str) -> (String, usize) {
    let gap = if text.is_empty() || text.ends_with(char::is_whitespace) {
        ""
    } else {
        " "
    };
    let out = format!("{text}{gap}{new}");
    let n = out.len();
    (out, n)
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
