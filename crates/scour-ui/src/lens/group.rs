//! Several disks at once: `under:a|under:b`, one run of terms the service
//! reads as "under a or under b". Adding to it, taking one out and reading
//! what it holds, without ever leaving a `|` that would join a stranger.

use super::{Term, replace, terms};
use crate::path::{full_form, home_form, untrailed as trim};

/// How a click on a disk changes the place group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pick {
    /// This disk and no other; on the only one held, none.
    Only,
    /// This one too; on one already held, that one out.
    Also,
}

/// A place the query names, by its full path and the term that names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Held {
    /// `~` written out, no trailing `/`.
    pub path: String,
    pub term: Term,
}

/// What the query's place terms say: where it looks, and where it does not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scope {
    /// The `under:` terms of the place group. Take one out with
    /// [`place_toggle`]`(…, Pick::Also)`.
    pub within: Vec<Held>,
    /// Every `!under:` term standing on its own. Take one out with
    /// [`remove_term`].
    pub outside: Vec<Held>,
}

impl Scope {
    /// The group holds this root, given as a full path.
    pub fn holds(&self, root: &str) -> bool {
        let root = trim(root);
        self.within.iter().any(|h| h.path == root)
    }
}

/// The terms of `text` in runs: two terms are of one run when a `|` stands
/// between them, spaces or not.
pub fn runs(text: &str) -> Vec<Vec<Term>> {
    let mut out: Vec<Vec<Term>> = Vec::new();
    for t in terms(text) {
        match out.last_mut() {
            Some(run) if text[run[run.len() - 1].end..t.start].contains('|') => run.push(t),
            _ => out.push(vec![t]),
        }
    }
    out
}

/// The run that holds `t`; `t` alone when it is in none.
pub fn run_of(text: &str, t: &Term) -> Vec<Term> {
    runs(text)
        .into_iter()
        .find(|r| r.contains(t))
        .unwrap_or_else(|| vec![t.clone()])
}

/// The query less `t` and one `|` beside it, the one after when there is
/// one: what is left of the run means what it meant without `t`.
pub fn remove_term(text: &str, t: &Term) -> (String, usize) {
    let run = run_of(text, t);
    let cut = |a: usize, b: usize| (format!("{}{}", &text[..a], &text[b..]), a);
    match run.iter().position(|m| m == t) {
        Some(k) if k + 1 < run.len() => cut(t.start, run[k + 1].start),
        Some(k) if k > 0 => cut(run[k - 1].end, t.end),
        _ => super::drop_alone(text, t),
    }
}

/// The query with the disk at `root` put into or taken out of the place
/// group, and the caret after what changed. The group is the first run with
/// an `under:` term; with none, the term goes at the end. `!under:` terms and
/// every other run are left as they are.
pub fn place_toggle(text: &str, home: &str, root: &str, pick: Pick) -> (String, usize) {
    let want = norm(home, root);
    let new = format!("under:{}", quoted_path(&home_form(home, &want)));
    let Some(run) = runs(text).into_iter().find(|r| r.iter().any(is_place)) else {
        let gap = if text.is_empty() || text.ends_with(char::is_whitespace) {
            ""
        } else {
            " "
        };
        let out = format!("{text}{gap}{new}");
        let caret = out.len();
        return (out, caret);
    };
    let places: Vec<&Term> = run.iter().filter(|t| is_place(t)).collect();
    let is = |t: &Term| !t.value.is_empty() && norm(home, &t.value) == want;
    let mine = places.iter().find(|t| is(t));
    match pick {
        Pick::Also => {
            if let Some(t) = mine {
                return remove_term(text, t);
            }
            if let Some(t) = places.iter().find(|t| t.value.is_empty()) {
                return replace(text, t, &new);
            }
            let end = places[places.len() - 1].end;
            let out = format!("{}|{new}{}", &text[..end], &text[end..]);
            (out, end + 1 + new.len())
        }
        Pick::Only => {
            let alone = places.iter().all(|t| is(t));
            // Later ones first, so the offsets of the earlier stay true.
            let mut out = (text.to_string(), 0);
            for t in places[1..].iter().rev() {
                out = remove_term(&out.0, t);
            }
            if alone {
                remove_term(&out.0, places[0])
            } else {
                replace(&out.0, places[0], &new)
            }
        }
    }
}

/// A whole tile put into or taken out of the place group: every root it is
/// indexed through, as one click. A tile is held when all its roots are.
/// `Only` leaves nothing else in the group, and on a tile held alone, nothing.
pub fn tile_toggle(text: &str, home: &str, roots: &[String], pick: Pick) -> (String, usize) {
    let want: Vec<String> = roots.iter().map(|r| norm(home, r)).collect();
    let held: Vec<String> = scope(text, home)
        .within
        .into_iter()
        .map(|h| h.path)
        .collect();
    let all_in = !want.is_empty() && want.iter().all(|w| held.contains(w));
    let alone = all_in && held.iter().all(|h| want.contains(h));
    let (out_of, into): (Vec<&String>, Vec<&String>) = match pick {
        Pick::Also if all_in => (want.iter().collect(), Vec::new()),
        Pick::Also => (
            Vec::new(),
            want.iter().filter(|w| !held.contains(w)).collect(),
        ),
        Pick::Only if alone => (held.iter().collect(), Vec::new()),
        Pick::Only => (
            held.iter().filter(|h| !want.contains(h)).collect(),
            want.iter().filter(|w| !held.contains(w)).collect(),
        ),
    };
    let mut out = (text.to_string(), text.len());
    for root in out_of.into_iter().chain(into) {
        out = place_toggle(&out.0, home, root, Pick::Also);
    }
    out
}

/// What the query's place terms hold; see [`Scope`].
pub fn scope(text: &str, home: &str) -> Scope {
    let held = |t: &Term| Held {
        path: norm(home, &t.value),
        term: t.clone(),
    };
    let all = runs(text);
    let within = all
        .iter()
        .find(|r| r.iter().any(is_place))
        .map(|r| {
            r.iter()
                .filter(|t| is_place(t) && !t.value.is_empty())
                .map(held)
                .collect()
        })
        .unwrap_or_default();
    let outside = all
        .iter()
        .filter(|r| r.len() == 1)
        .map(|r| &r[0])
        .filter(|t| t.not && t.field == Some("under") && !t.value.is_empty())
        .map(held)
        .collect();
    Scope { within, outside }
}

/// The query less every run that holds an `under:` or `parent:` term not
/// written with `!`: what the folder list is weighed by and the tiles are
/// counted against. A whole run goes, so no `|` is left to join a stranger;
/// a standalone `!under:` stays. The runs left are joined by one space.
pub fn without_places(text: &str) -> String {
    let place = |t: &Term| !t.not && matches!(t.field, Some("under" | "parent"));
    runs(text)
        .into_iter()
        .filter(|r| !r.iter().any(place))
        .map(|r| &text[r[0].start..r[r.len() - 1].end])
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn is_place(t: &Term) -> bool {
    t.field == Some("under") && !t.not
}

/// A place's full path: `~` written out, no trailing `/`.
pub(super) fn norm(home: &str, p: &str) -> String {
    trim(&full_form(home, p)).to_string()
}

/// A path as a term's value: quoted when a space, `|` or `;` would cut it.
pub(crate) fn quoted_path(p: &str) -> String {
    if p.chars()
        .any(|c| c.is_whitespace() || matches!(c, '|' | ';'))
    {
        format!("\"{p}\"")
    } else {
        p.to_string()
    }
}

#[cfg(test)]
#[path = "group_tests.rs"]
mod tests;
