//! The shared palette, in the terminal's terms.
//!
//! **Ours, imposed.** The rail's kind bars and the age stripe carry meaning in
//! their colour — "this is a document", "this has not been touched in a year"
//! — and a terminal's own sixteen cannot say either. So the same hex codes the
//! window and the page use are written out as true colour here.
//!
//! Two ways out, both deliberate rather than a fallback nobody chose:
//! `SCOUR_TUI_COLORS=terminal` hands the whole thing back to the terminal's
//! palette, and a terminal that cannot do true colour is detected and dropped
//! to its nearest.

use ratatui::style::Color;
use scour_ui::{DARK, LIGHT, Palette, Rgba};

/// Which palette to paint with, and whether the terminal can take it.
pub struct Theme {
    pub palette: &'static Palette,
    /// False when `COLORTERM` says nothing and the palette has to be
    /// approximated.
    pub truecolor: bool,
    /// True when somebody asked for the terminal's own colours instead.
    pub theirs: bool,
}

impl Theme {
    /// Read the environment: the scheme, then what the terminal can do.
    pub fn read(dark: bool) -> Theme {
        let theirs = std::env::var("SCOUR_TUI_COLORS").is_ok_and(|v| v == "terminal");
        let truecolor = std::env::var("COLORTERM")
            .is_ok_and(|v| v.contains("truecolor") || v.contains("24bit"));
        Theme {
            palette: if dark { &DARK } else { &LIGHT },
            truecolor,
            theirs,
        }
    }

    fn of(&self, c: Rgba) -> Color {
        if self.theirs {
            return Color::Reset;
        }
        let (_, r, g, b) = c.argb();
        if self.truecolor {
            Color::Rgb(r, g, b)
        } else {
            // The 6×6×6 cube of the 256-colour palette, which every terminal
            // written this century has. Nearest, not dithered: this is a
            // fallback, and a wrong shade is better than a wrong hue.
            let step = |v: u8| u16::from(v).saturating_mul(5).div_euclid(255) as u8;
            Color::Indexed(16 + 36 * step(r) + 6 * step(g) + step(b))
        }
    }

    pub fn ink(&self) -> Color {
        self.of(self.palette.ink)
    }
    pub fn ink_2(&self) -> Color {
        self.of(self.palette.ink_2)
    }
    pub fn ink_3(&self) -> Color {
        self.of(self.palette.ink_3)
    }
    pub fn back(&self) -> Color {
        self.of(self.palette.ground)
    }
    /// The panel colour, which the query line is drawn on so that it reads as
    /// a field rather than as one more row of text.
    pub fn panel(&self) -> Color {
        self.of(self.palette.panel)
    }
    pub fn line(&self) -> Color {
        self.of(self.palette.line)
    }
    pub fn key(&self) -> Color {
        self.of(self.palette.q_key)
    }
    pub fn bad(&self) -> Color {
        self.of(self.palette.q_bad)
    }
    pub fn pick(&self) -> Color {
        self.of(self.palette.pick)
    }

    /// The colour a kind is drawn in, or the quiet ink for one with no colour
    /// of its own — which is what "nothing in particular" looks like.
    pub fn kind(&self, token: &str) -> Color {
        match scour_ui::kind_colour(token) {
            Some(c) => self.of(c),
            None => self.ink_3(),
        }
    }

    /// The six age bands, oldest last. The stripe down the left of a row.
    pub fn band(&self, band: usize) -> Color {
        self.of(self.palette.t[band.min(5)])
    }
}
