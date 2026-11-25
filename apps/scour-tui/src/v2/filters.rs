//! What the time, type and size panels share: which one is open, what it
//! asks on the lens lane, and the answers. Every ask carries the opening
//! and a tag; an answer whose tag is no longer awaited is dropped, so a
//! panel opened again, or asked again, never shows an older question's
//! numbers.

use ratatui::crossterm::event::{KeyCode, KeyModifiers};
use scour_core::{FacetBy, FacetResponse};
use scour_ui::lens::{self, Lens, Mode, Scale, Term, TypeAsk};

use crate::app::{App, Panel, Want};
use crate::draw::PanelLine;

/// Which of the three panels is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Time,
    Type,
    Size,
}

/// What an answer fills.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    /// The presets and the bars, from one walk.
    Time,
    Kinds,
    Exts,
    /// The kinds and the extensions, from one walk.
    Type,
    Size(usize),
}

impl Slot {
    /// Whether an answer for `self` stands in for one of `other`.
    fn covers(self, other: Slot) -> bool {
        self == other
            || matches!(
                (self, other),
                (Slot::Type, Slot::Kinds | Slot::Exts) | (Slot::Kinds | Slot::Exts, Slot::Type)
            )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Asked {
    Facets { query: String, by: Vec<FacetBy> },
    Count { query: String },
}

/// One question for the lens lane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LensAsk {
    pub opening: u64,
    pub tag: u64,
    pub slot: Slot,
    pub asked: Asked,
}

impl LensAsk {
    /// Not worth asking once `other` is queued: a later opening, or a later
    /// question for what this one fills.
    pub fn stale_beside(&self, other: &LensAsk) -> bool {
        other.opening > self.opening || (other.slot.covers(self.slot) && other.tag > self.tag)
    }
}

/// The open panel's state and numbers.
#[derive(Debug, Clone)]
pub struct Filters {
    pub view: Option<View>,
    pub opening: u64,
    tag: u64,
    /// Tags asked and not answered, and what each fills.
    pub waiting: Vec<(u64, Slot)>,
    /// Asked, not yet sent: the loop sends them after drawing.
    pub asks: Vec<LensAsk>,
    /// When this opening was asked, and the zone then: the bars and the
    /// words are counted back from it.
    pub now: i64,
    pub offset: i64,
    pub scale: Scale,
    pub mode: Mode,
    /// The bars chosen, by drawn index, the older first.
    pub sel: Option<(usize, usize)>,
    /// A bar clicked once: the next click ends the run there.
    pub anchor: Option<usize>,
    pub presets: Option<[u64; 5]>,
    pub hist: Option<lens::Histogram>,
    pub kinds: Option<Vec<(String, u64)>>,
    pub kinds_total: u64,
    pub exts: Option<Vec<(String, u64)>>,
    pub exts_total: u64,
    pub asked_type: Option<TypeAsk>,
    /// Opened on `ext:`: the cursor goes to the extensions once they come.
    pub ext_focus: bool,
    pub sizes: [Option<u64>; 5],
}

impl Default for Filters {
    fn default() -> Self {
        Filters {
            view: None,
            opening: 0,
            tag: 0,
            waiting: Vec::new(),
            asks: Vec::new(),
            now: 0,
            offset: 0,
            scale: Scale::Day,
            mode: Mode::Between,
            sel: None,
            anchor: None,
            presets: None,
            hist: None,
            kinds: None,
            kinds_total: 0,
            exts: None,
            exts_total: 0,
            asked_type: None,
            ext_focus: false,
            sizes: [None; 5],
        }
    }
}

/// Seconds since the epoch, now.
pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl App {
    /// The panel the term at the caret opens, and that term.
    fn filter_view_at(&self) -> Option<(View, Term)> {
        let t = lens::term_at(&self.query, self.caret.min(self.query.len()))?;
        if lens::is_empty_term(&self.query, &t) {
            return Some((View::Size, t));
        }
        let view = match lens::lens_of(&self.query, &t)? {
            Lens::Time => View::Time,
            Lens::Size => View::Size,
            Lens::Kind | Lens::Ext => View::Type,
            _ => return None,
        };
        Some((view, t))
    }

    /// The lens rebuilt off the place mode: true when one of the three
    /// panels shows, opened anew unless it was already up.
    pub(super) fn filters_rebuild(&mut self) -> bool {
        let Some((view, t)) = self.filter_view_at() else {
            self.v2.filters.view = None;
            return false;
        };
        if self.v2.filters.view != Some(view) || self.panel != Panel::Lens {
            self.filters_open(view, &t);
        }
        true
    }

    fn filters_open(&mut self, view: View, t: &Term) {
        let now = unix_now();
        let f = &mut self.v2.filters;
        *f = Filters {
            view: Some(view),
            opening: f.opening + 1,
            tag: f.tag,
            now,
            offset: scour_ui::format::local_offset(now),
            ext_focus: t.field == Some("ext"),
            ..Filters::default()
        };
        self.panel_at = 0;
        match view {
            View::Time => {
                self.time_from(t);
                self.ask_time();
            }
            View::Type => {
                if self.v2.filters.ext_focus {
                    self.panel_at = self.type_len().min(super::kinds::KIND_ROWS);
                }
                self.ask_type();
            }
            View::Size => self.ask_sizes(),
        }
    }

    /// Queue a question, forgetting the ones it replaces.
    pub(super) fn lens_ask(&mut self, slot: Slot, asked: Asked) {
        let f = &mut self.v2.filters;
        f.tag += 1;
        f.waiting.retain(|(_, s)| !slot.covers(*s));
        f.waiting.push((f.tag, slot));
        f.asks.push(LensAsk {
            opening: f.opening,
            tag: f.tag,
            slot,
            asked,
        });
    }

    /// What is to be sent on the lens lane, taken.
    pub fn take_lens_asks(&mut self) -> Vec<LensAsk> {
        std::mem::take(&mut self.v2.filters.asks)
    }

    /// What a tag was asked for, when it is still awaited.
    fn lens_landed(&mut self, tag: u64) -> Option<Slot> {
        let f = &mut self.v2.filters;
        let at = f.waiting.iter().position(|(t, _)| *t == tag)?;
        self.dirty = true;
        Some(f.waiting.remove(at).1)
    }

    pub fn lens_faceted(&mut self, tag: u64, reply: &FacetResponse) {
        match self.lens_landed(tag) {
            Some(Slot::Time) => self.time_landed(reply),
            Some(slot @ (Slot::Kinds | Slot::Exts | Slot::Type)) => self.type_landed(slot, reply),
            _ => {}
        }
    }

    pub fn lens_counted(&mut self, tag: u64, total: u64) {
        if let Some(Slot::Size(i)) = self.lens_landed(tag) {
            self.v2.filters.sizes[i] = Some(total);
        }
    }

    /// The open panel, when it is one of the three.
    fn filter_view(&self) -> Option<View> {
        self.v2.filters.view.filter(|_| !self.v2.place_mode)
    }

    pub(super) fn filter_len(&self) -> Option<usize> {
        Some(match self.filter_view()? {
            View::Time => super::time::TIME_LINES,
            View::Type => self.type_len(),
            View::Size => lens::SIZE_PRESETS.len(),
        })
    }

    pub(super) fn filter_lines(&self, wide: usize) -> Option<(String, Vec<PanelLine>)> {
        Some(match self.filter_view()? {
            View::Time => (
                self.say("When it last changed").into_owned(),
                self.time_lines(),
            ),
            View::Type => (self.say("Type").into_owned(), self.type_lines(wide)),
            View::Size => (self.say("Size").into_owned(), self.size_lines()),
        })
    }

    pub(super) fn filter_press(&mut self) -> Option<Want> {
        Some(match self.filter_view()? {
            View::Time => self.time_press(),
            View::Type => self.type_toggle(false),
            View::Size => self.size_press(),
        })
    }

    /// A key the open panel takes before the query does.
    pub fn filter_key(&mut self, code: KeyCode, mods: KeyModifiers) -> Option<Want> {
        if mods.contains(KeyModifiers::CONTROL) {
            return None;
        }
        match self.filter_view()? {
            View::Time => self.time_key(code, mods.contains(KeyModifiers::SHIFT)),
            View::Type => self.type_key(code),
            View::Size => None,
        }
    }

    /// A press at character `ch` of line `at`; `None` presses the line.
    pub fn filter_click(&mut self, at: usize, ch: usize, shift: bool) -> Option<Want> {
        match self.filter_view()? {
            View::Time => self.time_click(at, ch, shift),
            _ => None,
        }
    }

    /// The term of `field` a choice replaces: the one at the caret, else the
    /// first.
    pub(super) fn field_term(&self, field: &str) -> Option<Term> {
        let at = lens::term_at(&self.query, self.caret.min(self.query.len()))
            .filter(|t| lens::of_field(&self.query, t, field));
        at.or_else(|| lens::field_terms(&self.query, field).into_iter().next())
    }

    /// `new` in place of `t`, or after the query; the panel closes on it.
    pub(super) fn write_term(&mut self, t: Option<&Term>, new: &str) -> Want {
        let (q, caret) = match t {
            Some(t) => lens::replace(&self.query, t, new),
            None => {
                let q = lens::with_term(&self.query, new);
                let end = q.len();
                (q, end)
            }
        };
        self.query = q;
        self.caret = caret;
        self.panel = Panel::None;
        self.typed()
    }

    /// A count as the panels print it; nothing while it is on its way.
    pub(super) fn count_text(&self, n: Option<u64>) -> String {
        n.map(|n| scour_ui::format::grouped(n, self.mark().0))
            .unwrap_or_default()
    }
}

/// A term standing for "none written yet": no `!`, nothing in it.
pub(super) fn bare(field: &'static str) -> Term {
    Term {
        start: 0,
        end: 0,
        not: false,
        field: Some(field),
        value: String::new(),
    }
}

#[cfg(test)]
#[path = "filters_tests.rs"]
pub(crate) mod tests;
