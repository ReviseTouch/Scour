//! The place lens's first line, "Searching in:": where the query looks, from
//! `scour_ui::lens::spots`, each place with the digit that takes it out.

use scour_ui::lens;

use super::home;
use super::place::head_of;
use crate::app::{App, Want};
use crate::draw::PanelLine;

/// How many of the spots a digit reaches.
const KEYED: usize = 9;

impl App {
    /// `written` as the query, the caret where the model rests it while a
    /// place is held. Without one the caret stays where the writer left it:
    /// this lens hangs from no term, so an empty `under:` would only be noise.
    pub(super) fn place_wrote(&mut self, (q, caret): (String, usize)) -> Want {
        let home = home();
        let held = !lens::scope(&q, &home).within.is_empty();
        let not = self.v2.place_not && !lens::scope(&q, &home).outside.is_empty();
        let (q, caret) = match held || not {
            true => lens::place_settle(&q, caret, not),
            false => (q, caret),
        };
        self.v2.editing = false;
        self.v2.place_left = Some((q.clone(), caret));
        self.v2.place_part = None;
        self.query = q;
        self.caret = caret;
        if self.v2.place_mode {
            self.rebuild_lens();
        }
        self.typed()
    }

    /// The k-th spot of "Searching in:" taken out, counted from one.
    pub(super) fn spot_out(&mut self, k: usize) -> Option<Want> {
        let home = home();
        let spots = lens::spots(&self.query, &home, &self.place_tiles());
        let written = spots.get(k.checked_sub(1)?)?.remove(&self.query, &home);
        Some(self.place_wrote(written))
    }

    /// "Searching in:" and each spot with the digit that takes it out: a
    /// disk held whole by its name, a folder in `~` form, one left out
    /// after a `!`. Everywhere, quietly, when nothing is held.
    pub(super) fn spots_line(&self, spots: &[lens::Spot], wide: usize) -> PanelLine {
        let head = self.say("Searching in:");
        let text = match spots.is_empty() {
            true => format!("{head} {}", self.say("Everywhere")),
            false => {
                let said: Vec<String> = spots
                    .iter()
                    .enumerate()
                    .map(|(k, s)| {
                        let not = if s.not { "!" } else { "" };
                        match k < KEYED {
                            true => format!("{} {not}{}", k + 1, s.name),
                            false => format!("{not}{}", s.name),
                        }
                    })
                    .collect();
                format!("{head} {}", said.join("  ·  "))
            }
        };
        PanelLine {
            text: head_of(&text, wide),
            key: String::new(),
            dimmed: spots.is_empty(),
            careful: false,
            rule: false,
            head: String::new(),
            mark: None,
            lit: None,
        }
    }
}

#[cfg(test)]
#[path = "spots_tests.rs"]
mod tests;
