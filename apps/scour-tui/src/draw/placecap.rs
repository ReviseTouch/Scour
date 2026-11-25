//! A run of `under:` terms in the query line drawn as one unit, `⛁ CACHY ·
//! Depo`, from `scour_ui::lens::place_caps`, while the caret is outside it.
//! The text stays the query; this only says which bytes are drawn as what.

use scour_ui::lens;

use crate::app::App;

/// A run of the query drawn as its label in place of its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folded {
    pub start: usize,
    pub end: usize,
    /// The disk kind of its first part, when that part is a whole disk.
    pub kind: Option<&'static str>,
    /// What follows the `⛁ `: the parts, and "+N" for those not shown.
    pub label: String,
}

impl Folded {
    /// The characters it is drawn in.
    pub fn width(&self) -> usize {
        2 + self.label.chars().count()
    }
}

/// The query's place runs drawn collapsed: not one the caret is in, nor
/// one with an `under:` still empty. `room` is the field's width.
pub fn folded(app: &App, room: usize) -> Vec<Folded> {
    let home = std::env::var("HOME").unwrap_or_default();
    let caret = app.caret.min(app.query.len());
    let caps = lens::place_caps(&app.query, &home, &app.place_tiles(), Some(caret));
    let raw_rest: usize = app.query.chars().count();
    caps.into_iter()
        .filter(|c| !(c.start < caret && caret <= c.end))
        .filter(|c| !c.parts.iter().any(|p| p.raw))
        .map(|mut cap| {
            let own = app.query[cap.start..cap.end].chars().count();
            // The run's own room: the field less the rest of the query.
            let budget = room.saturating_sub(raw_rest - own).max(12);
            let mut max = cap.parts.len();
            loop {
                cap.fold(max);
                if max == 1 || 2 + label(&cap).chars().count() <= budget {
                    break;
                }
                max -= 1;
            }
            Folded {
                start: cap.start,
                end: cap.end,
                kind: cap.parts.iter().find(|p| p.shown).and_then(|p| p.spot.kind),
                label: label(&cap),
            }
        })
        .collect()
}

fn label(cap: &lens::PlaceCap) -> String {
    let shown: Vec<&str> = cap
        .parts
        .iter()
        .filter(|p| p.shown)
        .map(|p| p.spot.name.as_str())
        .collect();
    let mut out = shown.join(" · ");
    if cap.hidden > 0 {
        out.push_str(&format!(" +{}", cap.hidden));
    }
    out
}

/// The column the caret is drawn at, counted from the query's first
/// character: a run folded before it takes its label's width, not its own.
pub fn caret_column(query: &str, caret: usize, folds: &[Folded]) -> usize {
    let mut col = query[..caret].chars().count();
    for f in folds.iter().filter(|f| f.end <= caret) {
        col = col + f.width() - query[f.start..f.end].chars().count();
    }
    col
}
