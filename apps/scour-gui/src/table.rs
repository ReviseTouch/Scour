//! The list's columns: which, how wide, laid out again when the window moves.

use super::*;
use scour_i18n::Catalogue;
use slint::{Model, ModelRc, VecModel};

/// The column headings and their widths, from `scour-ui`; painting is not shared.
pub(super) fn columns(window: &MainWindow, cat: &Catalogue) {
    // The report has a size heading of its own, which is not the table's.
    let head = |id: &str| {
        scour_ui::column(id)
            .map(|c| t(cat, c.msgid))
            .unwrap_or_default()
    };
    window.set_head_size(head("size"));
}

/// The columns this window shows when nobody has said otherwise: `scour-ui`'s.
pub(super) const COLUMN_DEFAULT: &[&str] = scour_ui::DEFAULT_COLUMNS;

/// Which columns are shown, how wide, and what a person dragged them to: one
/// place, because widths are shared out among the columns showing and a drag has
/// to survive a column being hidden and shown again.
pub(super) struct Table {
    pub(super) shown: Vec<&'static scour_ui::Column>,
    /// What each column was dragged to, for **every** column and not only the
    /// showing ones: `Change::widths` replaces the map rather than merging.
    pub(super) widths: std::collections::BTreeMap<String, u32>,
    /// The widest text each column holds in the answer on screen, in pixels:
    /// what `scour_ui::fit_to` hands room by. Measured when a question's first
    /// page lands and not as the list scrolls, so the columns do not shift
    /// under a reading eye.
    pub(super) need: std::collections::BTreeMap<String, u32>,
}

impl Table {
    /// What was saved, or the default: an unknown id is dropped, and an empty list
    /// is the one answer that cannot be right.
    pub(super) fn read(kept: &scour_settings::Settings) -> Table {
        let mut shown: Vec<&'static scour_ui::Column> = kept
            .columns
            .iter()
            .filter_map(|id| scour_ui::column(id))
            .collect();
        if shown.is_empty() {
            shown = COLUMN_DEFAULT
                .iter()
                .filter_map(|id| scour_ui::column(id))
                .collect();
        }
        Table {
            shown,
            widths: kept.widths.clone(),
            need: Default::default(),
        }
    }

    /// Measure the stretching columns against these rows. `name` and `cell`
    /// are one character of the mono face at the name's size and the other
    /// cells', as the window measured them.
    pub(super) fn measure(&mut self, page: &[Row], name: f32, cell: f32) {
        self.need.clear();
        // What is on screen and a little past it: one deep path further down
        // should not decide the columns for the rows a person is reading.
        let seen = &page[..page.len().min(60)];
        for (ci, c) in self.shown.iter().enumerate() {
            if c.near == 0 {
                continue;
            }
            let widest = seen
                .iter()
                .filter_map(|r| r.cells.row_data(ci))
                // The name is held in three parts around the match.
                .map(|x| {
                    [&x.text, &x.pre, &x.hit, &x.post]
                        .iter()
                        .map(|t| t.chars().count())
                        .sum::<usize>()
                })
                .max();
            let Some(chars) = widest else { continue };
            // The name's icon, its gap, and a few pixels so the last letter is
            // not against the next column.
            let px = if c.id == "name" {
                23.0 + chars as f32 * name + 6.0
            } else {
                chars as f32 * cell + 6.0
            };
            self.need.insert(c.id.to_owned(), px.ceil() as u32);
        }
    }

    pub(super) fn ids(&self) -> Vec<String> {
        self.shown.iter().map(|c| c.id.to_owned()).collect()
    }

    pub(super) fn showing(&self, id: &str) -> bool {
        self.shown.iter().any(|c| c.id == id)
    }

    /// Switch one column on or off, put back in front of the first shown column
    /// that outranks it in `scour_ui::COLUMNS`.
    pub(super) fn toggle(&mut self, id: &str) {
        if self.shown.len() == 1 && self.showing(id) {
            return;
        }
        if let Some(at) = self.shown.iter().position(|c| c.id == id) {
            self.shown.remove(at);
            return;
        }
        let Some(col) = scour_ui::column(id) else {
            return;
        };
        let rank = |x: &str| scour_ui::COLUMNS.iter().position(|c| c.id == x);
        let at = self
            .shown
            .iter()
            .position(|c| rank(c.id) > rank(col.id))
            .unwrap_or(self.shown.len());
        self.shown.insert(at, col);
    }
}

/// Divide the row up among the columns again: a resize, the preview panel, a
/// dragged edge. `scour_ui::lay_out` promises the widths add up to the room.
pub(super) fn relayout(window: &MainWindow, table: &Table) {
    let ids = table.ids();
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    // The kind column holds a word from a short list, so it is as wide as
    // the widest word plus its gap and does not breathe with the answer: the
    // room moves between the name and the location only.
    let kind = (window.get_kind_width() > 0.0).then(|| window.get_kind_width().ceil() as u32 + 14);
    let widths = scour_ui::lay_out(
        &ids,
        // Zero is "nobody has touched it", the same as in the settings file.
        |id| {
            table
                .widths
                .get(id)
                .copied()
                .filter(|v| *v > 0)
                .or(if id == "kind" { kind } else { None })
        },
        window.get_lane().max(0.0) as u32,
    );
    if widths.len() != ids.len() {
        return;
    }
    let mut widths = widths;
    scour_ui::fit_to(
        &ids,
        |id| table.widths.get(id).copied().filter(|v| *v > 0),
        |id| table.need.get(id).copied(),
        &mut widths,
    );
    let px: Vec<f32> = widths.iter().map(|w| *w as f32).collect();
    // Where each column starts inside the row: the row's 12px padding, then
    // every earlier column and the 8px gap after it. Handed over so a cell can
    // tell whether the row's pointer is on it without reading its own
    // geometry — which, in a repeater's `changed` binding, is a cycle.
    let mut at = 12.0;
    let starts: Vec<f32> = px
        .iter()
        .map(|w| {
            let here = at;
            at += w + 8.0;
            here
        })
        .collect();
    window.set_cx(ModelRc::new(VecModel::from(starts)));
    window.set_cw(ModelRc::new(VecModel::from(px)));
}

/// The headings, in the order they are shown; redone when either changes.
pub(super) fn set_heads(window: &MainWindow, table: &Table, cat: &Catalogue) {
    window.set_heads(ModelRc::new(VecModel::from(heads_of(table, cat))));
}

/// The same, without a window — which is what makes the wiring testable.
pub(super) fn heads_of(table: &Table, cat: &Catalogue) -> Vec<HeadInfo> {
    table
        .shown
        .iter()
        .map(|c| HeadInfo {
            id: c.id.into(),
            label: t(cat, c.msgid),
            sort: c.sort.into(),
            right: c.align == scour_ui::Align::End,
        })
        .collect()
}
