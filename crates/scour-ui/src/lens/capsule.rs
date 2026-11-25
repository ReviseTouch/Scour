//! A run of `under:` terms drawn as one capsule: `under:/etc|under:~|…` is
//! the system disk, not five capsules. The text stays the only model; this
//! says which bytes each part of the capsule stands for, never how wide.

use super::group::{Held, is_place, norm, runs};
use super::spots::{Spot, gather};
use crate::disks::Tile;
use crate::path::home_form;

/// One step of a folder's path, with the path that reaches it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    pub label: String,
    /// Full, `~` written out.
    pub path: String,
}

/// The steps of `path` from `~` (when it lies in `home`) or from `/`.
pub fn crumbs(path: &str, home: &str) -> Vec<Crumb> {
    let shown = home_form(home, path);
    let (mut at, first, rest) = match shown.strip_prefix('~') {
        Some(rest) if !home.is_empty() => (home.to_string(), "~", rest.to_string()),
        _ => (String::new(), "/", path.to_string()),
    };
    let top = if at.is_empty() { "/" } else { at.as_str() }.to_string();
    let mut out = vec![Crumb {
        label: first.into(),
        path: top,
    }];
    for seg in rest.split('/').filter(|s| !s.is_empty()) {
        at = format!("{at}/{seg}");
        out.push(Crumb {
            label: seg.into(),
            path: at.clone(),
        });
    }
    out
}

/// One part inside the place capsule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapPart {
    /// What it is: a whole tile (its name and kind), or one folder. Its
    /// `terms` are the bytes it stands for; `remove` takes it out.
    pub spot: Spot,
    /// A folder's steps, for the crumbs; empty for a tile.
    pub crumbs: Vec<Crumb>,
    /// Drawn as its own text, to be edited: the caret is in it, or it is an
    /// `under:` with nothing after it yet.
    pub raw: bool,
    /// The caret is in one of its terms.
    pub caret: bool,
    /// Drawn; one not drawn is counted in [`PlaceCap::hidden`].
    pub shown: bool,
}

/// A run of positive `under:` terms and the parts it is drawn as.
///
/// Every byte of `start..=end` stays addressable: [`PlaceCap::part_at`]
/// says which part an offset is drawn at. A raw part takes the caret at its
/// own letters; a collapsed one at its trailing edge (its leading edge for
/// `start`), and a hidden one at the "+N". A caret moved into a member's
/// letters makes it raw on the next draw, so editing always shows the text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceCap {
    pub start: usize,
    pub end: usize,
    /// In the order of their first term in the text.
    pub parts: Vec<CapPart>,
    /// How many parts the "+N" after the last shown part stands for.
    pub hidden: usize,
}

impl PlaceCap {
    /// The part an offset in `start..=end` is drawn at: the one whose term
    /// it is in or just after, else the one before it (a `|`), else the first.
    pub fn part_at(&self, offset: usize) -> Option<usize> {
        if offset < self.start || offset > self.end {
            return None;
        }
        let of = |k: usize| self.parts[k].spot.terms.iter().map(move |t| (k, t));
        let all: Vec<_> = (0..self.parts.len()).flat_map(of).collect();
        all.iter()
            .find(|(_, t)| t.start < offset && offset <= t.end)
            .or_else(|| {
                all.iter()
                    .filter(|(_, t)| t.end <= offset)
                    .max_by_key(|(_, t)| t.end)
            })
            .or_else(|| all.iter().min_by_key(|(_, t)| t.start))
            .map(|(k, _)| *k)
    }

    /// Show no more than `max` parts, at least one, the caret's always among
    /// them; the rest are behind the "+N". A face measures, then folds.
    pub fn fold(&mut self, max: usize) {
        let max = max.max(1);
        let n = self.parts.len();
        let caret = self.parts.iter().position(|p| p.caret);
        for (k, p) in self.parts.iter_mut().enumerate() {
            p.shown = n <= max || k < max;
        }
        if let Some(c) = caret.filter(|&c| c >= max && n > max) {
            self.parts[max - 1].shown = false;
            self.parts[c].shown = true;
        }
        self.hidden = self.parts.iter().filter(|p| !p.shown).count();
    }
}

/// Every run of `text` made only of `under:` terms not written with `!`, as
/// one capsule each, every part shown. A run with any other term in it —
/// `under:/a|ext:pdf`, `!under:/a|under:/b` — is none: its terms are drawn
/// as they were. `caret` is the text's caret, if it has one.
pub fn place_caps(text: &str, home: &str, tiles: &[Tile], caret: Option<usize>) -> Vec<PlaceCap> {
    runs(text)
        .into_iter()
        .filter(|r| r.iter().all(is_place))
        .map(|run| {
            let members: Vec<Held> = run
                .iter()
                .map(|t| Held {
                    path: norm(home, &t.value),
                    term: t.clone(),
                })
                .collect();
            let inside = |t: &super::Term| caret.is_some_and(|c| t.start < c && c <= t.end);
            let lone = run.iter().find(|t| inside(t));
            let parts = gather(&members, home, tiles, lone)
                .into_iter()
                .map(|spot| part(spot, home, &inside))
                .collect();
            PlaceCap {
                start: run[0].start,
                end: run[run.len() - 1].end,
                parts,
                hidden: 0,
            }
        })
        .collect()
}

fn part(spot: Spot, home: &str, inside: &dyn Fn(&super::Term) -> bool) -> CapPart {
    let caret = spot.terms.iter().any(inside);
    let empty = spot.tile.is_none() && spot.terms[0].value.is_empty();
    CapPart {
        crumbs: match spot.tile {
            None if !empty => crumbs(&spot.paths[0], home),
            _ => Vec::new(),
        },
        raw: caret || empty,
        caret,
        shown: true,
        spot,
    }
}

#[cfg(test)]
#[path = "capsule_tests.rs"]
mod tests;
