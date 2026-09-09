//! The shared palette, in the terminal's terms.
//!
//! True colour, because the kind bars and the age stripe carry meaning in it.
//! `SCOUR_TUI_COLORS=terminal` hands the palette back; a terminal without true
//! colour is dropped to the nearest 256-colour cell.

use ratatui::style::Color;
use scour_ui::{DARK, LIGHT, Palette, Rgba};

/// Which palette to paint with, and whether the terminal can take it.
pub struct Theme {
    pub palette: &'static Palette,
    /// False when `COLORTERM` says nothing: the palette is approximated.
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
            // Nearest cell of the 256-colour palette's 6×6×6 cube, not dithered.
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
    /// The panel colour; the query line sits on it so it reads as a field.
    pub fn panel(&self) -> Color {
        self.of(self.palette.panel)
    }
    /// The one item in a menu that changes something. Not an error colour.
    pub fn danger(&self) -> Color {
        self.of(self.palette.danger)
    }
    pub fn line(&self) -> Color {
        self.of(self.palette.line)
    }
    pub fn key(&self) -> Color {
        self.of(self.palette.q_key)
    }
    /// What is being looked for: a bare word. Blue, against the red of `not`.
    pub fn term(&self) -> Color {
        self.of(self.palette.q_term)
    }
    /// The value after a field's colon; `glob`, `not` and `bad` follow it.
    pub fn val(&self) -> Color {
        self.of(self.palette.q_val)
    }
    pub fn glob(&self) -> Color {
        self.of(self.palette.q_glob)
    }
    pub fn not(&self) -> Color {
        self.of(self.palette.q_not)
    }
    pub fn bad(&self) -> Color {
        self.of(self.palette.q_bad)
    }
    /// What a row goes under the pointer: a shade of the panel, not a colour.
    pub fn hover(&self) -> Color {
        self.of(self.palette.hover)
    }
    pub fn pick(&self) -> Color {
        self.of(self.palette.pick)
    }

    /// The colour a kind is drawn in, or quiet ink for one with none.
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
