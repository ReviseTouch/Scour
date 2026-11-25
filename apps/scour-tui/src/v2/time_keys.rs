//! The time panel's keys and clicks: ←/→ walk the scale and mode lines and
//! move the newer end of the bars, Shift the older; a click picks a word of
//! a switch, or a bar and then the bar that ends the run.

use ratatui::crossterm::event::KeyCode;
use scour_ui::lens::{MODES, SCALES};

use super::time::{BARS_LINE, MODE_LINE, SCALE_LINE, switch};
use crate::app::{App, Want};

impl App {
    pub(super) fn time_key(&mut self, code: KeyCode, shift: bool) -> Option<Want> {
        let by = match code {
            KeyCode::Left => -1,
            KeyCode::Right => 1,
            _ => return None,
        };
        match self.panel_at {
            SCALE_LINE => {
                let at = SCALES.iter().position(|s| *s == self.v2.filters.scale)?;
                self.set_scale(at.saturating_add_signed(by).min(SCALES.len() - 1));
            }
            MODE_LINE => {
                let at = MODES.iter().position(|m| *m == self.v2.filters.mode)?;
                self.v2.filters.mode = MODES[at.saturating_add_signed(by).min(MODES.len() - 1)];
            }
            BARS_LINE => self.move_end(by, shift),
            _ => return None,
        }
        self.dirty = true;
        Some(Want::Nothing)
    }

    fn set_scale(&mut self, at: usize) {
        let f = &mut self.v2.filters;
        if f.scale == SCALES[at] {
            return;
        }
        f.scale = SCALES[at];
        f.sel = None;
        f.anchor = None;
        f.hist = None;
        self.ask_time();
    }

    /// ←/→ move the newer end, Shift the older; the other end follows
    /// rather than cross. The first press only chooses the newest bar.
    fn move_end(&mut self, by: isize, older: bool) {
        let f = &mut self.v2.filters;
        let last_bar = f.scale.bars() - 1;
        let Some((a, b)) = f.sel else {
            f.sel = Some((last_bar, last_bar));
            return;
        };
        let step = |x: usize| x.saturating_add_signed(by).min(last_bar);
        f.sel = Some(match older {
            true => {
                let a = step(a);
                (a, b.max(a))
            }
            false => {
                let b = step(b);
                (a.min(b), b)
            }
        });
        f.anchor = None;
    }

    pub(super) fn time_click(&mut self, at: usize, ch: usize, shift: bool) -> Option<Want> {
        match at {
            SCALE_LINE => {
                let names: Vec<String> =
                    SCALES.iter().map(|s| self.say(s.label()).into()).collect();
                if let Some(i) = hit(&switch(&names).1, ch) {
                    self.set_scale(i);
                }
            }
            MODE_LINE => {
                let names: Vec<String> = MODES.iter().map(|m| self.say(m.label()).into()).collect();
                if let Some(i) = hit(&switch(&names).1, ch) {
                    self.v2.filters.mode = MODES[i];
                }
            }
            BARS_LINE => {
                let lead = self.bars_text().1;
                let n = self.v2.filters.scale.bars();
                if let Some(k) = ch.checked_sub(lead).filter(|k| *k < n) {
                    self.click_bar(k, shift);
                }
            }
            _ => return None,
        }
        self.panel_at = at;
        self.dirty = true;
        Some(Want::Nothing)
    }

    /// A click picks one bar; the next ends the run there, as does Shift.
    fn click_bar(&mut self, k: usize, shift: bool) {
        let f = &mut self.v2.filters;
        let from = match (shift, f.anchor, f.sel) {
            (true, _, Some((a, b))) => Some(if k < a { b } else { a }),
            (false, Some(a), _) => Some(a),
            _ => None,
        };
        match from {
            Some(a) => {
                f.sel = Some((a.min(k), a.max(k)));
                f.anchor = None;
            }
            None => {
                f.sel = Some((k, k));
                f.anchor = Some(k);
            }
        }
    }
}

/// Which span holds character `ch`.
fn hit(spans: &[(usize, usize)], ch: usize) -> Option<usize> {
    spans.iter().position(|(a, b)| (*a..*b).contains(&ch))
}
