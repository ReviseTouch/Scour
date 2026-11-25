//! The size panel in text: the five presets, each with its own count (there
//! is no size facet), the one the query holds ticked.

use scour_ui::lens::{self, SIZE_PRESETS};

use super::filters::{Asked, Slot};
use crate::app::{App, Want};
use crate::draw::PanelLine;

impl App {
    pub(super) fn ask_sizes(&mut self) {
        for (i, query) in lens::size_counts(&self.query).into_iter().enumerate() {
            self.lens_ask(Slot::Size(i), Asked::Count { query });
        }
    }

    pub(super) fn size_lines(&self) -> Vec<PanelLine> {
        let held = self.field_term("size");
        let chosen = held
            .as_ref()
            .and_then(|t| lens::size_preset_of(&self.query, t));
        let not = held.as_ref().is_some_and(|t| t.not);
        SIZE_PRESETS
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let n = self.v2.filters.sizes[i];
                let tick = match (chosen == Some(i), not) {
                    (true, true) => "✗",
                    (true, false) => "✓",
                    _ => " ",
                };
                PanelLine {
                    text: format!("{tick} {}", self.say(p.label)),
                    key: self.count_text(n),
                    dimmed: n == Some(0),
                    careful: chosen == Some(i) && not,
                    ..Default::default()
                }
            })
            .collect()
    }

    pub(super) fn size_press(&mut self) -> Want {
        let Some(i) = Some(self.panel_at).filter(|i| *i < SIZE_PRESETS.len()) else {
            return Want::Nothing;
        };
        let t = self.field_term("size");
        let new = match &t {
            Some(t) => lens::size_preset_term(t, i),
            None => SIZE_PRESETS[i].term.to_string(),
        };
        self.write_term(t.as_ref(), &new)
    }
}

#[cfg(test)]
#[path = "sizes_tests.rs"]
mod tests;
