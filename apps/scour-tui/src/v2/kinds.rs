//! The type panel in text: the kinds with their counts and a share bar,
//! then the extensions within the kinds chosen. Space or Enter puts a value
//! in, `!` leaves it out (its own `!kind:` term), Tab goes between the two
//! lists. The values are toggled by `scour_ui::lens::value_toggle`.

use ratatui::crossterm::event::KeyCode;
use scour_core::{FacetBy, FacetResponse};
use scour_ui::lens::{self, EXT_TOP, KIND_LABELS};

use super::filters::{Asked, Slot};
use crate::app::{App, Want};
use crate::draw::PanelLine;

pub const KIND_ROWS: usize = KIND_LABELS.len();

/// Cells in a share bar.
const SHARE: usize = 10;

/// Eighths of a cell, for the share bar's last one.
const EIGHTHS: [char; 8] = [' ', '▏', '▎', '▍', '▌', '▋', '▊', '▉'];

impl App {
    /// Ask what the tiles and the extensions count over, each only when its
    /// query is new: a kind toggled leaves the kinds' query as it was.
    pub(super) fn ask_type(&mut self) {
        let ask = lens::type_ask(&self.query);
        let before = self.v2.filters.asked_type.replace(ask.clone());
        let exact = || FacetBy::Age { edges: vec![0] };
        let ext = || FacetBy::Ext { top: EXT_TOP };
        if ask.one_walk() {
            if before.as_ref() != Some(&ask) {
                let by = vec![FacetBy::Kind, ext(), exact()];
                let query = ask.kinds;
                self.lens_ask(Slot::Type, Asked::Facets { query, by });
            }
            return;
        }
        if before.as_ref().map(|b| &b.kinds) != Some(&ask.kinds) {
            let by = vec![FacetBy::Kind, exact()];
            let query = ask.kinds.clone();
            self.lens_ask(Slot::Kinds, Asked::Facets { query, by });
        }
        if before.as_ref().map(|b| &b.exts) != Some(&ask.exts) {
            let by = vec![ext(), exact()];
            let query = ask.exts;
            self.lens_ask(Slot::Exts, Asked::Facets { query, by });
        }
    }

    pub(super) fn type_landed(&mut self, slot: Slot, reply: &FacetResponse) {
        let group = |want: fn(&FacetBy) -> bool| {
            reply
                .groups
                .iter()
                .find(|g| want(&g.by))
                .map(|g| g.facets.iter().map(|x| (x.key.clone(), x.count)).collect())
        };
        let f = &mut self.v2.filters;
        if matches!(slot, Slot::Kinds | Slot::Type) {
            f.kinds = group(|b| matches!(b, FacetBy::Kind));
            f.kinds_total = reply.total;
        }
        if matches!(slot, Slot::Exts | Slot::Type) {
            f.exts = group(|b| matches!(b, FacetBy::Ext { .. }));
            f.exts_total = reply.total;
            if f.ext_focus {
                f.ext_focus = false;
                self.panel_at = KIND_ROWS;
            }
        }
    }

    /// The extensions listed: the facet's, then any the query holds that it
    /// did not return, so each can be taken out again.
    fn ext_rows(&self) -> Vec<(String, Option<u64>)> {
        let mut out: Vec<(String, Option<u64>)> = self
            .v2
            .filters
            .exts
            .iter()
            .flatten()
            .map(|(k, n)| (k.clone(), Some(*n)))
            .collect();
        let held = lens::picked(&self.query, "ext");
        for k in held.on.iter().chain(&held.off) {
            if !out.iter().any(|(e, _)| e == k) {
                out.push((k.clone(), None));
            }
        }
        out
    }

    pub(super) fn type_len(&self) -> usize {
        KIND_ROWS + self.ext_rows().len()
    }

    /// The value line `at` stands for, and its field.
    fn type_value(&self, at: usize) -> Option<(&'static str, String)> {
        match at.checked_sub(KIND_ROWS) {
            None => Some(("kind", KIND_LABELS.get(at)?.0.to_string())),
            Some(i) => Some(("ext", self.ext_rows().get(i)?.0.clone())),
        }
    }

    pub(super) fn type_lines(&self, wide: usize) -> Vec<PanelLine> {
        let f = &self.v2.filters;
        let kinds = lens::picked(&self.query, "kind");
        let exts = lens::picked(&self.query, "ext");
        let name_w = KIND_LABELS
            .iter()
            .map(|(_, l)| self.say(l).chars().count())
            .max()
            .unwrap_or(0)
            .min(wide.saturating_sub(SHARE + 16));
        let row = |at: usize,
                   label: String,
                   n: Option<u64>,
                   total: u64,
                   held: &lens::Picked,
                   key: &str| {
            let on = held.on.iter().any(|v| v == key);
            let off = held.off.iter().any(|v| v == key);
            let tick = if off {
                "✗"
            } else if on {
                "✓"
            } else {
                " "
            };
            let label: String = label.chars().take(name_w).collect();
            let bar = n.map(|n| share(n, total)).unwrap_or_default();
            // The key that leaves a value out, beside the line it would.
            let leave = if at == self.panel_at { "  !" } else { "   " };
            PanelLine {
                text: format!("{tick} {label:<name_w$}  {bar}"),
                key: format!("{}{leave}", self.count_text(n)),
                dimmed: n == Some(0),
                careful: off,
                ..Default::default()
            }
        };
        let counted = |k: &str| {
            let all = f.kinds.as_ref()?;
            Some(all.iter().find(|(t, _)| t == k).map_or(0, |(_, n)| *n))
        };
        let mut out: Vec<PanelLine> = KIND_LABELS
            .iter()
            .enumerate()
            .map(|(at, (token, label))| {
                row(
                    at,
                    self.say(label).into(),
                    counted(token),
                    f.kinds_total,
                    &kinds,
                    token,
                )
            })
            .collect();
        for (i, (ext, n)) in self.ext_rows().into_iter().enumerate() {
            let mut line = row(
                KIND_ROWS + i,
                format!(".{ext}"),
                n,
                f.exts_total,
                &exts,
                &ext,
            );
            if i == 0 {
                line.rule = true;
                line.head = self.say("Extension").into_owned();
            }
            out.push(line);
        }
        out
    }

    /// Put the value under the cursor in, or with `exclude` leave it out.
    pub(super) fn type_toggle(&mut self, exclude: bool) -> Want {
        let Some((field, value)) = self.type_value(self.panel_at) else {
            return Want::Nothing;
        };
        let (q, caret) = lens::value_toggle(&self.query, field, &value, exclude);
        self.query = q;
        self.caret = caret;
        self.v2.editing = false;
        self.ask_type();
        self.typed()
    }

    pub(super) fn type_key(&mut self, code: KeyCode) -> Option<Want> {
        match code {
            KeyCode::Char('!') => Some(self.type_toggle(true)),
            KeyCode::Tab | KeyCode::BackTab => {
                self.panel_at = match self.panel_at < KIND_ROWS && self.type_len() > KIND_ROWS {
                    true => KIND_ROWS,
                    false => 0,
                };
                self.dirty = true;
                Some(Want::Nothing)
            }
            _ => None,
        }
    }
}

/// `n` of `total` as a bar of [`SHARE`] cells, to the eighth.
fn share(n: u64, total: u64) -> String {
    let eighths = match total {
        0 => 0,
        t => ((n as u128 * (SHARE as u128 * 8)).div_ceil(t as u128)) as usize,
    }
    .min(SHARE * 8);
    let mut out = "█".repeat(eighths / 8);
    if eighths % 8 > 0 {
        out.push(EIGHTHS[eighths % 8]);
    }
    format!("{out:<SHARE$}")
}

#[cfg(test)]
#[path = "kinds_tests.rs"]
mod tests;
