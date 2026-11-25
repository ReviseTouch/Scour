//! The caret over the query: whether the person is editing the term it is
//! in or it only rests there after a press, whether a lens opens by itself
//! there, and where the arrow keys stop over a place capsule.

use super::capsule::PlaceCap;
use super::group::runs;
use super::{Term, lens_of, opens_itself, term_at};

/// The text's caret, and whether the person is editing at it.
///
/// `editing` is the face's to say: set it when the person types, or clicks
/// into a folder part to change it; clear it when a press of a lens writes
/// the text or the caret moves out of that term. Without it, a caret at a
/// term's end only rests there: a part stays collapsed and no lens opens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Caret {
    pub at: usize,
    pub editing: bool,
}

impl Caret {
    /// Where a press, a click on a part or an arrow key left it.
    pub fn resting(at: usize) -> Caret {
        Caret { at, editing: false }
    }

    /// Where the person is typing.
    pub fn editing(at: usize) -> Caret {
        Caret { at, editing: true }
    }
}

/// The term is being edited: a field with nothing after its `:` yet, or the
/// caret strictly inside it, or at its end while `editing`.
pub(super) fn is_edited(t: &Term, caret: Option<Caret>) -> bool {
    if t.field.is_some() && t.value.is_empty() {
        return true;
    }
    caret.is_some_and(|c| t.start < c.at && (c.at < t.end || (c.at == t.end && c.editing)))
}

/// The term the person is editing at `caret`, if any; see [`Caret`].
pub fn edited_term(text: &str, caret: Caret) -> Option<Term> {
    term_at(text, caret.at).filter(|t| is_edited(t, Some(caret)))
}

/// Whether the lens of the term at the caret opens by itself. A place term
/// drawn in a capsule — a run of `under:` terms, or a standalone `!under:` —
/// opens only while edited, never for a caret resting on a part's edge; it
/// opens on a click, the explicit key, or typing. Any other term as before.
pub fn opens_by_itself(text: &str, caret: Caret) -> bool {
    let Some(t) = term_at(text, caret.at) else {
        return false;
    };
    if !lens_of(text, &t).is_some_and(opens_itself) {
        return false;
    }
    let capsuled = t.field == Some("under")
        && runs(text).into_iter().any(|r| {
            r.contains(&t) && (r.iter().all(|m| m.field == Some("under") && !m.not) || r.len() == 1)
        });
    !capsuled || is_edited(&t, Some(caret))
}

impl PlaceCap {
    /// Where the arrow keys stop in the capsule, in order: its start, the
    /// trailing edge of each collapsed part, every letter of a raw part, and
    /// one stop for each block of hidden parts (drawn at its "+N").
    pub fn stops(&self, text: &str) -> Vec<usize> {
        let mut out = vec![self.start];
        let n = self.parts.len();
        for (k, p) in self.parts.iter().enumerate() {
            let end = p.spot.terms.iter().map(|t| t.end).max().unwrap_or(self.end);
            if !p.shown {
                if k + 1 == n || self.parts[k + 1].shown {
                    out.push(end);
                }
            } else if p.raw {
                for t in &p.spot.terms {
                    out.extend((t.start..=t.end).filter(|&o| text.is_char_boundary(o)));
                }
            } else {
                out.push(end);
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The caret after one arrow key from `at`: the next stop that way, or
    /// none when the key leaves the capsule (the face then moves by a letter).
    pub fn step(&self, text: &str, at: usize, forward: bool) -> Option<usize> {
        if at < self.start || at > self.end {
            return None;
        }
        let stops = self.stops(text);
        if forward {
            stops.into_iter().find(|&s| s > at)
        } else {
            stops.into_iter().rev().find(|&s| s < at)
        }
    }

    /// Each block of hidden parts, as the index of its first part and how
    /// many: one "+N" each, drawn where the block stands.
    pub fn blocks(&self) -> Vec<(usize, usize)> {
        let mut out: Vec<(usize, usize)> = Vec::new();
        let hidden = self.parts.iter().enumerate().filter(|(_, p)| !p.shown);
        for k in hidden.map(|(k, _)| k) {
            match out.last_mut() {
                Some((first, n)) if *first + *n == k => *n += 1,
                _ => out.push((k, 1)),
            }
        }
        out
    }
}

#[cfg(test)]
#[path = "caret_tests.rs"]
mod tests;
