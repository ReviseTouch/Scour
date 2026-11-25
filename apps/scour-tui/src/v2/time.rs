//! The time panel in text: the five presets with their counts, a scale line
//! and a mode line, the bars as a row of blocks between the axis ends, and
//! the choice in words with its count. One `age` facet per opening and per
//! scale change answers the presets and the bars together.

use scour_core::{FacetBy, FacetResponse};
use scour_ui::calendar::MONTHS;
use scour_ui::format::DAY;
use scour_ui::lens::{
    self, Date, DateForm, MODES, Mode, PRESET_EDGES, SCALES, Selected, TIME_PRESETS, Term, TimeSaid,
};

use super::filters::{Asked, Slot, bare};
use crate::app::{App, Want};
use crate::draw::PanelLine;

pub const SCALE_LINE: usize = 5;
pub const MODE_LINE: usize = 6;
pub const BARS_LINE: usize = 7;
pub const SAID_LINE: usize = 8;
pub const TIME_LINES: usize = SAID_LINE + 1;

const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

impl App {
    /// The scale, mode and bars a `dm:` term already says.
    pub(super) fn time_from(&mut self, t: &Term) {
        let Some(pick) = lens::time_pick(&t.value) else {
            return;
        };
        if let Some(s) = pick.selected {
            let f = &mut self.v2.filters;
            f.scale = s.scale;
            f.mode = s.mode;
            f.sel = Some((s.first.min(s.last), s.first.max(s.last)));
        }
    }

    /// The presets and the current scale's bars, in one walk.
    pub(super) fn ask_time(&mut self) {
        let query = lens::time_ask(&self.query);
        let by = vec![
            FacetBy::Age {
                edges: PRESET_EDGES.to_vec(),
            },
            FacetBy::Age {
                edges: self.v2.filters.scale.edges(),
            },
        ];
        self.lens_ask(Slot::Time, Asked::Facets { query, by });
    }

    pub(super) fn time_landed(&mut self, reply: &FacetResponse) {
        let f = &mut self.v2.filters;
        let pairs = |i: usize| {
            reply
                .groups
                .get(i)
                .into_iter()
                .flat_map(|g| g.facets.iter().map(|x| (x.key.as_str(), x.count)))
        };
        f.presets = Some(lens::preset_counts(pairs(0)));
        let edges = f.scale.edges();
        let ours = reply
            .groups
            .get(1)
            .is_some_and(|g| g.by == FacetBy::Age { edges });
        if ours {
            f.hist = Some(lens::histogram(f.scale, pairs(1), f.now));
        }
    }

    /// The selection as the model takes it.
    fn selected(&self) -> Option<Selected> {
        let f = &self.v2.filters;
        f.sel.map(|(first, last)| Selected {
            scale: f.scale,
            mode: f.mode,
            first,
            last,
        })
    }

    /// How many rows the selection holds, from the bars.
    fn selected_count(&self) -> Option<u64> {
        let f = &self.v2.filters;
        let (h, (a, b)) = (f.hist.as_ref()?, f.sel?);
        let n = h.bars.len();
        let sum = |r: std::ops::Range<usize>| h.bars[r].iter().map(|x| x.count).sum::<u64>();
        Some(match f.mode {
            Mode::Between => sum(a..b + 1),
            Mode::Before => sum(0..b + 1) + h.older,
            Mode::After => sum(a..n),
        })
    }

    /// A `dm:` value in words, as the other faces say it.
    pub fn time_words(&self, value: &str, now: i64, offset: i64) -> Option<String> {
        Some(match lens::time_said(value, now, offset)? {
            TimeSaid::Label(m) => self.say(m).into_owned(),
            TimeSaid::Count(m, n) => self.say(m).replace("{n}", &n.to_string()),
            TimeSaid::Date(d) => self.date_text(d),
            TimeSaid::Bound(m, d) => self.say(m).replace("{date}", &self.date_text(d)),
            TimeSaid::Range(a, b) => format!("{} – {}", self.date_text(a), self.date_text(b)),
        })
    }

    /// The axis ends and the bars, and where the bars start in the text.
    pub(super) fn bars_text(&self) -> (String, usize) {
        let f = &self.v2.filters;
        let n = f.scale.bars();
        let unit = i64::from(f.scale.days()) * DAY;
        let (from, to) = match &f.hist {
            Some(h) => (h.from, h.to),
            None => (f.now - n as i64 * unit, f.now),
        };
        let end = |s: i64| {
            self.date_text(Date {
                day: (s + f.offset).div_euclid(DAY),
                form: f.scale.ends(),
            })
        };
        let left = end(from);
        let bars: String = match &f.hist {
            Some(h) => h.bars.iter().map(|b| block(b.count, h.peak)).collect(),
            None => "·".repeat(n),
        };
        let lead = left.chars().count() + 1;
        (format!("{left} {bars} {}", end(to)), lead)
    }

    pub(super) fn time_lines(&self) -> Vec<PanelLine> {
        let f = &self.v2.filters;
        let held = self.field_term("dm");
        let preset = held
            .as_ref()
            .and_then(|t| lens::time_pick(&t.value))
            .and_then(|p| p.preset);
        let mut out: Vec<PanelLine> = TIME_PRESETS
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let n = f.presets.map(|c| c[i]);
                let tick = if preset == Some(i) { "✓" } else { " " };
                PanelLine {
                    text: format!("{tick} {}", self.say(p.label)),
                    key: self.count_text(n),
                    dimmed: n == Some(0),
                    ..Default::default()
                }
            })
            .collect();
        let scales: Vec<String> = SCALES.iter().map(|s| self.say(s.label()).into()).collect();
        let at = SCALES.iter().position(|s| *s == f.scale).unwrap_or(0);
        let (text, spans) = switch(&scales);
        out.push(PanelLine {
            text,
            key: "← →".into(),
            rule: true,
            lit: Some(spans[at]),
            ..Default::default()
        });
        let modes: Vec<String> = MODES.iter().map(|m| self.say(m.label()).into()).collect();
        let at = MODES.iter().position(|m| *m == f.mode).unwrap_or(0);
        let (text, spans) = switch(&modes);
        out.push(PanelLine {
            text,
            key: "← →".into(),
            lit: Some(spans[at]),
            ..Default::default()
        });
        let (text, lead) = self.bars_text();
        out.push(PanelLine {
            text,
            key: "← → ⇧".into(),
            lit: f.sel.map(|(a, b)| (lead + a, lead + b + 1)),
            ..Default::default()
        });
        let (said, n) = self.said_now(held.as_ref(), preset);
        out.push(PanelLine {
            dimmed: said == "·",
            text: said,
            key: self.count_text(n),
            rule: true,
            ..Default::default()
        });
        out
    }

    /// The choice in words and its count: the bars chosen, else the term
    /// the query holds.
    fn said_now(&self, held: Option<&Term>, preset: Option<usize>) -> (String, Option<u64>) {
        let f = &self.v2.filters;
        let value = match self.selected() {
            Some(s) => {
                let term = lens::time_term(&bare("dm"), s);
                term.strip_prefix("dm:").unwrap_or_default().to_string()
            }
            None => held.map(|t| t.value.clone()).unwrap_or_default(),
        };
        let words = self.time_words(&value, f.now, f.offset);
        let n = match (f.sel, preset) {
            (Some(_), _) => self.selected_count(),
            (None, Some(p)) => f.presets.map(|c| c[p]),
            _ => None,
        };
        match words {
            Some(w) => (w, n),
            None if f.sel.is_some() => ("…".into(), n),
            None => ("·".into(), None),
        }
    }

    pub(super) fn time_press(&mut self) -> Want {
        let t = self.field_term("dm");
        if self.panel_at < TIME_PRESETS.len() {
            let new = lens::time_preset_term(t.as_ref().unwrap_or(&bare("dm")), self.panel_at);
            return self.write_term(t.as_ref(), &new);
        }
        match self.selected() {
            Some(s) => {
                let new = lens::time_term(t.as_ref().unwrap_or(&bare("dm")), s);
                self.write_term(t.as_ref(), &new)
            }
            None => Want::Nothing,
        }
    }
}

/// A bar's block, its height a share of the tallest; `·` for none.
fn block(n: u64, peak: u64) -> char {
    if n == 0 || peak == 0 {
        return '·';
    }
    BLOCKS[(n * 8).div_ceil(peak).clamp(1, 8) as usize - 1]
}

/// Words side by side, two spaces apart, and where each one is.
pub(super) fn switch(words: &[String]) -> (String, Vec<(usize, usize)>) {
    let mut text = String::new();
    let mut spans = Vec::new();
    for w in words {
        if !text.is_empty() {
            text.push_str("  ");
        }
        let a = text.chars().count();
        text.push_str(w);
        spans.push((a, a + w.chars().count()));
    }
    (text, spans)
}

impl App {
    /// A day in the person's words, as much of it as its form says: "26
    /// Oct", "26 Oct 2025", "Oct 2025", "2025", "26"; the month the first
    /// three letters of its name, which is how both languages shorten it.
    pub fn date_text(&self, d: Date) -> String {
        let (y, m, day) = d.ymd();
        let mon: String = self.say(MONTHS[m as usize - 1]).chars().take(3).collect();
        match d.form {
            DateForm::Day => format!("{day}"),
            DateForm::DayMonth => format!("{day} {mon}"),
            DateForm::DayMonthYear => format!("{day} {mon} {y}"),
            DateForm::MonthYear => format!("{mon} {y}"),
            DateForm::Year => format!("{y}"),
        }
    }
}

#[cfg(test)]
#[path = "time_tests.rs"]
mod tests;
