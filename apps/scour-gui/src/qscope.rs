//! The scope chip at the left of the query bar: a dot for every disk the
//! search can reach, and where it looks. Drawn from the last `volumes` and
//! `sources` answers and the query text; it keeps nothing of its own.

use scour_core::Catalog;
use scour_i18n::Catalogue;
use scour_ui::disks::{Shown, Tile};
use scour_ui::lens::{self, Scope};
use scour_ui::path::{home_form, leaf};
use slint::{ModelRc, VecModel};

use crate::qwire::{QueryBar, home};
use crate::ui::ScopeDot;
use crate::{MainWindow, placewire, qplace};

/// What the chip shows: a dot per disk, kind and ringed and lit, and its words.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Chip {
    pub dots: Vec<(&'static str, bool, bool)>,
    pub label: String,
}

/// The chip for these disks and this query. `everywhere` is said when the
/// query's place group holds nothing.
pub fn chip(tiles: &[Tile], scope: &Scope, hm: &str, everywhere: &str) -> Chip {
    let dots = tiles
        .iter()
        .filter(|t| t.choosable() || t.shown == Shown::Asking)
        .map(|t| {
            let asking = t.shown == Shown::Asking;
            (t.kind, asking, asking || placewire::lit(t, scope))
        })
        .collect();
    if scope.within.is_empty() {
        return Chip {
            dots,
            label: everywhere.to_string(),
        };
    }
    let mut names = Vec::new();
    let mut covered: Vec<&str> = Vec::new();
    for t in tiles.iter().filter(|t| placewire::selected(t, scope)) {
        names.push(t.name.clone());
        covered.extend(t.roots.iter().map(String::as_str));
    }
    for h in &scope.within {
        if !covered.contains(&h.path.as_str()) {
            names.push(leaf(&home_form(hm, &h.path)).to_string());
        }
    }
    Chip {
        dots,
        label: names.join(" + "),
    }
}

/// Draw the chip, when it changed.
pub fn draw(w: &MainWindow, qb: &mut QueryBar, cat: &Catalogue, text: &str) {
    let hm = home();
    let tiles = qplace::tiles(&qb.disks, &qb.sources);
    let next = chip(
        &tiles,
        &lens::scope(text, &hm),
        &hm,
        cat.get("Everywhere").as_ref(),
    );
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
