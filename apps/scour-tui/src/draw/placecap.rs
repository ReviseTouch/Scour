//! A run of `under:` terms in the query line drawn as one unit, `⛁ CACHY ·
//! Depo`, from `scour_ui::lens::place_caps_at`, while none of it is edited;
//! a block of `!under:` terms as one negated unit, `⛁ !Depo`.
//! The text stays the query; this only says which bytes are drawn as what.
//! The other filters' words are `valuecap`'s; both are [`Folded`].

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
    /// A place run, drawn after the disk glyph; any other is its words.
    pub place: bool,
    /// Written with `!`: the words are drawn as left out.
    pub not: bool,
}

impl Folded {
    /// The characters it is drawn in.
    pub fn width(&self) -> usize {
        usize::from(self.place) * 2 + self.label.chars().count()
    }
}

/// The query's place runs, and its blocks of `!under:` terms, drawn
/// collapsed: not one with a member being edited (`lens::Caret`), nor one
/// with an `under:` still empty. `room` is the field's width.
pub fn folded(app: &App, room: usize) -> Vec<Folded> {
    let home = std::env::var("HOME").unwrap_or_default();
    let caret = Some(app.caret_at());
    let tiles = app.place_tiles();
    let within = lens::place_caps_at(&app.query, &home, &tiles, caret);
    let outside = lens::exclusion_caps(&app.query, &home, &tiles, caret);
    let raw_rest: usize = app.query.chars().count();
    let caps = within.into_iter().map(|c| (c, false));
    caps.chain(outside.into_iter().map(|c| (c, true)))
        .filter(|(c, _)| !c.parts.iter().any(|p| p.raw))
        .map(|(mut cap, not)| {
            let own = app.query[cap.start..cap.end].chars().count();
            // The run's own room: the field less the rest of the query.
            let budget = room.saturating_sub(raw_rest - own).max(12);
            let mut max = cap.parts.len();
            loop {
                cap.fold(max);
                if max == 1 || 2 + label(&cap, not).chars().count() <= budget {
                    break;
                }
                max -= 1;
            }
            Folded {
                start: cap.start,
                end: cap.end,
                kind: cap.parts.iter().find(|p| p.shown).and_then(|p| p.spot.kind),
                label: label(&cap, not),
                place: true,
                not,
            }
        })
        .collect()
}

fn label(cap: &lens::PlaceCap, not: bool) -> String {
    let shown: Vec<&str> = cap
        .parts
        .iter()
        .filter(|p| p.shown)
        .map(|p| p.spot.name.as_str())
        .collect();
    let mut out = format!("{}{}", if not { "!" } else { "" }, shown.join(" · "));
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
