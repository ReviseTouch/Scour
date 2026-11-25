//! Where the query looks, said alike by every face: the entries of the place
//! lens's "Searching in" row and the scope chip's line and dots, from the
//! query's place terms and the disk tiles. A disk held whole is one entry.

use super::Term;
use super::group::{Held, Pick, Scope, remove_term, scope, tile_toggle};
use crate::disks::{Shown, Tile, lies_under};
use crate::path::{home_form, leaf};

/// One place the query looks in, or does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spot {
    /// The tile it is, by its index in the tiles given: every root held.
    pub tile: Option<usize>,
    /// The tile's name, or the folder in `~` form.
    pub name: String,
    /// What the chip says: the tile's name, or the folder's last step.
    pub short: String,
    /// The tile's kind; none for a folder.
    pub kind: Option<&'static str>,
    /// Written `!under:`: where the query does not look.
    pub not: bool,
    /// The full paths it stands for: a tile's roots, or the one folder.
    pub paths: Vec<String>,
    /// The terms it stands for, in the order of the text.
    pub terms: Vec<Term>,
}

impl Spot {
    /// The query without it, and the caret where it was: a tile goes as
    /// [`tile_toggle`]`(…, Pick::Also)`, a folder or an exclusion as its term.
    pub fn remove(&self, text: &str, home: &str) -> (String, usize) {
        match self.tile {
            Some(_) => tile_toggle(text, home, &self.paths, Pick::Also),
            None => remove_term(text, &self.terms[0]),
        }
    }

    fn folder(h: &Held, home: &str, not: bool) -> Spot {
        let shown = home_form(home, &h.path);
        Spot {
            tile: None,
            short: leaf(&shown).to_string(),
            name: shown,
            kind: None,
            not,
            paths: vec![h.path.clone()],
            terms: vec![h.term.clone()],
        }
    }
}

/// The query's place group holds the whole tile.
pub fn tile_held(t: &Tile, scope: &Scope) -> bool {
    t.choosable() && !t.roots.is_empty() && t.roots.iter().all(|r| scope.holds(r))
}

/// The query searches somewhere on the tile's disk: a place on it or above
/// it is held, or none is. Never a disk on which nothing is indexed.
pub fn tile_lit(t: &Tile, scope: &Scope) -> bool {
    let near = |r: &String| {
        scope
            .within
            .iter()
            .any(|h| lies_under(&h.path, r) || lies_under(r, &h.path))
    };
    t.choosable() && (scope.within.is_empty() || t.roots.iter().any(near))
}

/// Where `text` looks, in the order of its terms — a tile held whole as one
/// entry — and then where it does not.
pub fn spots(text: &str, home: &str, tiles: &[Tile]) -> Vec<Spot> {
    let s = scope(text, home);
    let mut out = gather(&s.within, home, tiles, None);
    out.extend(s.outside.iter().map(|h| Spot::folder(h, home, true)));
    out
}

/// `members` as entries: each tile whose roots they all hold is one, at its
/// first member, `lone` left out of it; every other member is its own.
pub(super) fn gather(
    members: &[Held],
    home: &str,
    tiles: &[Tile],
    lone: Option<&Term>,
) -> Vec<Spot> {
    let paths: Vec<&str> = members.iter().map(|h| h.path.as_str()).collect();
    let mut taken = vec![false; members.len()];
    let mut out = Vec::new();
    for (k, t) in tiles.iter().enumerate() {
        let whole = t.choosable() && !t.roots.is_empty();
        if !whole || !t.roots.iter().all(|r| paths.contains(&r.as_str())) {
            continue;
        }
        let mut terms = Vec::new();
        for (i, h) in members.iter().enumerate() {
            if !taken[i] && t.roots.contains(&h.path) && Some(&h.term) != lone {
                taken[i] = true;
                terms.push(h.term.clone());
            }
        }
        if terms.is_empty() {
            continue;
        }
        out.push(Spot {
            tile: Some(k),
            name: t.name.clone(),
            short: t.name.clone(),
            kind: Some(t.kind),
            not: false,
            paths: t.roots.clone(),
            terms,
        });
    }
    for (i, h) in members.iter().enumerate() {
        if !taken[i] {
            out.push(Spot::folder(h, home, false));
        }
    }
    out.sort_by_key(|s| s.terms[0].start);
    out
}

/// The chip's line: `everywhere` when nothing is held, else the places'
/// short names joined by " + ", the first `max` of them and "+k" for the rest.
pub fn chip_label(spots: &[Spot], max: usize, everywhere: &str) -> String {
    let names: Vec<&str> = spots
        .iter()
        .filter(|s| !s.not)
        .map(|s| s.short.as_str())
        .collect();
    let max = max.max(1);
    match names.len() {
        0 => everywhere.to_string(),
        n if n <= max => names.join(" + "),
        n => format!("{} +{}", names[..max].join(" + "), n - max),
    }
}

/// A dot of the chip: a disk the search can reach, or one asking to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dot {
    pub kind: &'static str,
    /// Waiting for an answer: drawn ringed.
    pub asking: bool,
    /// [`tile_lit`]: drawn full, else dimmed.
    pub lit: bool,
}

/// The chip's dots, in the tiles' order: every disk something is indexed on,
/// and every one waiting for an answer.
pub fn chip_dots(tiles: &[Tile], scope: &Scope) -> Vec<Dot> {
    tiles
        .iter()
        .filter(|t| t.choosable() || t.shown == Shown::Asking)
        .map(|t| Dot {
            kind: t.kind,
            asking: t.shown == Shown::Asking,
            lit: tile_lit(t, scope),
        })
        .collect()
}

#[cfg(test)]
#[path = "spots_tests.rs"]
mod tests;
