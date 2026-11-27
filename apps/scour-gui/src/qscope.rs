//! The scope chip at the left of the query bar: a dot for every disk the
//! search can reach, and where it looks. Drawn from the last `volumes` and
//! `sources` answers and the query text; it keeps nothing of its own.

use scour_core::Catalog;
use scour_i18n::Catalogue;
use scour_ui::disks::Tile;
use scour_ui::lens;
use slint::{ModelRc, VecModel};

use crate::qwire::{QueryBar, home};
use crate::ui::ScopeDot;
use crate::{MainWindow, qplace};

/// What the chip shows: a dot per disk, kind and ringed and lit, and its words.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Chip {
    pub dots: Vec<(&'static str, bool, bool)>,
    pub label: String,
}

/// Names the chip's line holds before it says `+k`; one when two would not
/// fit its widest, which elides at about ten letters.
const NAMES: usize = 2;
const ROOM: usize = 10;

/// The chip for these disks and this query, as the model says it.
pub fn chip(tiles: &[Tile], text: &str, hm: &str, everywhere: &str) -> Chip {
    let dots = lens::chip_dots(tiles, &lens::scope(text, hm))
        .into_iter()
        .map(|d| (d.kind, d.asking, d.lit))
        .collect();
    let spots = lens::spots(text, hm, tiles);
    let mut label = lens::chip_label(&spots, NAMES, everywhere);
    if label.chars().count() > ROOM {
        label = lens::chip_label(&spots, 1, everywhere);
    }
    Chip { dots, label }
}

/// Draw the chip, when it changed.
pub fn draw(w: &MainWindow, qb: &mut QueryBar, cat: &Catalogue, text: &str) {
    let hm = home();
    let tiles = qplace::tiles(&qb.disks, &qb.sources);
    let next = chip(&tiles, text, &hm, cat.get("Everywhere").as_ref());
    if next == qb.chip {
        return;
    }
    let dots: Vec<ScopeDot> = next
        .dots
        .iter()
        .map(|&(kind, ring, lit)| ScopeDot {
            kind: kind.into(),
            ring,
            lit,
        })
        .collect();
    w.set_where_dots(ModelRc::new(VecModel::from(dots)));
    w.set_where_label(next.label.as_str().into());
    qb.chip = next;
}

#[cfg(test)]
#[path = "qscope_tests.rs"]
mod tests;
