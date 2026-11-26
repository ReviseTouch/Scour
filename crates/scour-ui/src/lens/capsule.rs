//! A run of `under:` terms drawn as one capsule: `under:/etc|under:~|…` is
//! the system disk, not five capsules. The text stays the only model; this
//! says which bytes each part of the capsule stands for, never how wide.

use super::Term;
use super::caret::{Caret, is_edited};
use super::group::{Held, is_place, norm, runs};
use super::negate::exclusion_blocks;
use super::spots::{Spot, gather};
use crate::disks::{Tile, lies_under};
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

/// The steps of `path` as a capsule shows them, and the disk it lies on by
/// its index in `tiles`: from `~` inside `home`, from the disk's name below a
/// disk's root ("Depo › PROJELER"), else from `/` as [`crumbs`]. `/` itself
/// stays `/`: every disk lies under it.
pub fn crumbs_on(path: &str, home: &str, tiles: &[Tile]) -> (Vec<Crumb>, Option<usize>) {
    let disk = tile_of(path, tiles);
    let in_home = !home.is_empty() && home_form(home, path).starts_with('~');
    let Some(k) = disk.filter(|_| !in_home && !path.trim_end_matches('/').is_empty()) else {
        return (crumbs(path, home), disk);
    };
    let root = tiles[k].root.trim_end_matches('/');
    let mut out = vec![Crumb {
        label: tiles[k].name.clone(),
        path: if root.is_empty() { "/" } else { root }.to_string(),
    }];
    let mut at = root.to_string();
    for seg in path[root.len()..].split('/').filter(|s| !s.is_empty()) {
        at = format!("{at}/{seg}");
        out.push(Crumb {
            label: seg.into(),
            path: at.clone(),
        });
    }
    (out, disk)
}

/// The tile `path` lies on, by its index: the one with the longest root above it.
pub fn tile_of(path: &str, tiles: &[Tile]) -> Option<usize> {
    tiles
        .iter()
        .enumerate()
        .filter(|(_, t)| !t.root.is_empty() && lies_under(path, &t.root))
        .max_by_key(|(_, t)| t.root.trim_end_matches('/').len())
        .map(|(k, _)| k)
}

/// One part inside the place capsule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapPart {
    /// What it is: a whole tile (its name and kind), or one folder. Its
    /// `terms` are the bytes it stands for; `remove` takes it out.
    pub spot: Spot,
    /// A folder's steps, for the crumbs; empty for a tile.
    pub crumbs: Vec<Crumb>,
    /// Drawn as its own text: a folder being edited (see [`Caret`]), or an
    /// `under:` with nothing after it yet. A tile part never is.
    pub raw: bool,
    /// The caret is in or at the end of one of its terms: drawn at its
    /// trailing edge when the part is not raw.
    pub caret: bool,
    /// Drawn; one not drawn is counted in [`PlaceCap::hidden`].
    pub shown: bool,
    /// The disk it lies on, by its index in the tiles given: a tile's own,
    /// or the one with the longest root above the folder. None for an empty
    /// member, or a folder on no disk known.
    pub disk: Option<usize>,
}

/// A run of `under:` terms, or of `!under:` ones, and the parts it is drawn as.
///
/// Every byte of `start..=end` stays addressable: [`PlaceCap::part_at`]
/// says which part an offset is drawn at. A raw part takes the caret at its
/// own letters; a collapsed one at its trailing edge (its leading edge for
/// `start`), and a hidden one at its block's "+N". The arrow keys go by
/// [`PlaceCap::step`]; a caret strictly inside a member makes it raw.
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
    /// The one disk its parts lie on, by its index in the tiles: what the
    /// header shows. None when they lie on several, or on none known; a part
    /// on no disk (an empty member) does not count.
    pub fn disk(&self) -> Option<usize> {
        let mut on = self.parts.iter().filter_map(|p| p.disk);
        let first = on.next()?;
        on.all(|d| d == first).then_some(first)
    }

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

/// [`place_caps_at`] with the caret resting: superseded by it, which says
/// whether the person is editing.
pub fn place_caps(text: &str, home: &str, tiles: &[Tile], caret: Option<usize>) -> Vec<PlaceCap> {
    place_caps_at(text, home, tiles, caret.map(Caret::resting))
}

/// Every run of `text` made only of `under:` terms not written with `!`, as
/// one capsule each, every part shown. A run with any other term in it —
/// `under:/a|ext:pdf`, `!under:/a|under:/b` — is none: its terms are drawn
/// as they were. A member is raw only while edited (see [`Caret`]); a tile
/// collapses over consecutive members none of which is, so a caret resting
/// on a tile's edge after a press leaves it whole.
pub fn place_caps_at(
    text: &str,
    home: &str,
    tiles: &[Tile],
    caret: Option<Caret>,
) -> Vec<PlaceCap> {
    runs(text)
        .into_iter()
        .filter(|r| r.iter().all(is_place))
        .map(|run| cap(&run, home, tiles, caret, false))
        .collect()
}

/// Every block of standalone `!under:` terms side by side ("in none of
/// them"), as one negated capsule each, drawn as [`place_caps_at`] draws a
/// group: a tile all of whose roots are left out is one part. Its parts'
/// `spot.not` is true.
pub fn exclusion_caps(
    text: &str,
    home: &str,
    tiles: &[Tile],
    caret: Option<Caret>,
) -> Vec<PlaceCap> {
    exclusion_blocks(text)
        .iter()
        .map(|block| cap(block, home, tiles, caret, true))
        .collect()
}

fn cap(members: &[Term], home: &str, tiles: &[Tile], caret: Option<Caret>, not: bool) -> PlaceCap {
    let held: Vec<Held> = members
        .iter()
        .map(|t| Held {
            path: norm(home, &t.value),
            term: t.clone(),
        })
        .collect();
    let raw = |t: &Term| is_edited(t, caret);
    let parts = gather(&held, home, tiles, not, Some(&raw))
        .into_iter()
        .map(|spot| part(spot, home, tiles, caret))
        .collect();
    PlaceCap {
        start: members[0].start,
        end: members[members.len() - 1].end,
        parts,
        hidden: 0,
    }
}

fn part(spot: Spot, home: &str, tiles: &[Tile], caret: Option<Caret>) -> CapPart {
    let on = |t: &Term| caret.is_some_and(|c| t.start < c.at && c.at <= t.end);
    let raw = spot.tile.is_none() && is_edited(&spot.terms[0], caret);
    let empty = spot.tile.is_none() && spot.terms[0].value.is_empty();
    let (crumbs, disk) = match spot.tile {
        Some(k) => (Vec::new(), Some(k)),
        None if empty => (Vec::new(), None),
        None => crumbs_on(&spot.paths[0], home, tiles),
    };
    CapPart {
        crumbs,
        raw,
        caret: spot.terms.iter().any(on),
        shown: true,
        disk,
        spot,
    }
}

#[cfg(test)]
#[path = "capsule_tests.rs"]
mod tests;
