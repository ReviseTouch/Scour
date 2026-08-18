//! What every Scour window agrees on.
//!
//! **Data, and nothing else.** The palette and the metrics live here once; a
//! window reads them and turns them into whatever its own toolkit wants — CSS
//! custom properties in the browser, globals in Slint, escape codes in a
//! terminal. This crate has no dependencies and knows about none of them.
//!
//! ## Why it exists
//!
//! There were two copies. `page.html` declared twenty-six CSS variables and
//! `theme.slint` declared thirty properties, and they held **the same hex
//! codes** — `#0d1117`, `#7fa9e0`, `4px`, `30px` — because somebody kept them
//! that way by hand. That works right up until it doesn't, and it already
//! hadn't: the focus colour had drifted apart in the light theme, one side
//! saying `#4a9eff` and the other `#2f6ba3`, and nothing anywhere could tell.
//!
//! ## What is deliberately *not* here
//!
//! Layout. Where a button sits, how a panel opens, what a list does when it is
//! scrolled — those are a toolkit's business and copying them between two
//! toolkits produces something that fits neither. What is shared is the
//! vocabulary: this colour means "a field name", that one means "changed this
//! week", a row is thirty pixels tall.

/// A colour, and the one representation both sides can be built from.
///
/// Alpha is carried because the palette needs it: the match wash and the
/// selection tint are translucent on purpose, so that a row which is both
/// matched *and* selected stays legible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn hex(v: u32) -> Rgba {
        Rgba {
            r: (v >> 16) as u8,
            g: (v >> 8) as u8,
            b: v as u8,
            a: 255,
        }
    }

    /// A translucent colour, alpha as hundredths — `17` is CSS's `.17`.
    pub const fn wash(v: u32, percent: u8) -> Rgba {
        Rgba {
            r: (v >> 16) as u8,
            g: (v >> 8) as u8,
            b: v as u8,
            // Rounded the way CSS does it, at compile time.
            a: ((percent as u32 * 255 + 50) / 100) as u8,
        }
    }

    /// How the browser wants it: `#rrggbb`, or `rgba(…)` when translucent.
    ///
    /// Two spellings rather than always `#rrggbbaa`, because the page these
    /// go into is read by people and `rgba(74, 158, 255, .15)` is what was
    /// written there before this crate existed.
    pub fn css(&self) -> String {
        if self.a == 255 {
            format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            let alpha = (self.a as f32 / 255.0 * 100.0).round() / 100.0;
            format!(
                "rgba({}, {}, {}, {})",
                self.r,
                self.g,
                self.b,
                format!("{alpha:.2}").trim_start_matches('0')
            )
        }
    }

    /// How Slint wants it, through `slint::Color::from_argb_u8`.
    pub const fn argb(&self) -> (u8, u8, u8, u8) {
        (self.a, self.r, self.g, self.b)
    }
}

/// Every colour a window needs, in one of the two schemes.
///
/// The field names are the vocabulary. A window may not invent a colour that
/// is not here; if it needs one, it belongs here and in both windows at once.
#[derive(Debug, Clone, Copy)]
pub struct Palette {
    /// Behind the window itself.
    pub ground: Rgba,
    /// The window's own surface.
    pub panel: Rgba,
    /// One step up from it: headers, wells, the inside of a control.
    pub panel_2: Rgba,
    /// Borders that separate.
    pub line: Rgba,
    /// Borders that only hint.
    pub line_soft: Rgba,
    /// Text a person reads.
    pub ink: Rgba,
    /// Text beside it — a path, a unit, a count.
    pub ink_2: Rgba,
    /// Text that is barely there: labels, disabled things.
    pub ink_3: Rgba,
    /// The wash over a matched piece of a name. Translucent.
    pub mark: Rgba,
    /// Text drawn *on* `mark`, where the toolkit cannot blend.
    pub mark_ink: Rgba,
    /// The tint over a selected row. Translucent, and blue where the match
    /// wash is yellow — a row can be both at once.
    pub pick: Rgba,
    /// The ring around whatever has the keyboard.
    pub focus: Rgba,
    /// The row under the pointer.
    pub hover: Rgba,
    /// The time spectrum, newest to oldest. Six bands, and the engine already
    /// stores rows in date order, so an unbroken spectrum runs the length of
    /// the list.
    pub t: [Rgba; 6],
    /// The query line's own palette, **deliberately separate** from the time
    /// spectrum: the spectrum uses all of the saturation there is, and reusing
    /// a band here would give "changed yesterday" and "field value" the same
    /// colour.
    ///
    /// Field name and comparison — structure.
    pub q_key: Rgba,
    /// Value, and text inside quotes — content.
    pub q_val: Rgba,
    /// Wildcard — a pattern rather than a word.
    pub q_glob: Rgba,
    /// Exclusion — deliberate, not a mistake.
    pub q_not: Rgba,
    /// Looks like a field, was searched for as text.
    pub q_bad: Rgba,
}

/// The dark scheme. The neutrals lean blue; chosen, not inherited.
pub const DARK: Palette = Palette {
    ground: Rgba::hex(0x0d1117),
    panel: Rgba::hex(0x131920),
    panel_2: Rgba::hex(0x171e26),
    line: Rgba::hex(0x212a35),
    line_soft: Rgba::hex(0x1a222c),
    ink: Rgba::hex(0xe7eaee),
    ink_2: Rgba::hex(0x9aa5b1),
    ink_3: Rgba::hex(0x63707e),
    mark: Rgba::wash(0xffd24a, 17),
    mark_ink: Rgba::hex(0x1a1200),
    pick: Rgba::wash(0x4a9eff, 15),
    focus: Rgba::hex(0x4a9eff),
    hover: Rgba::hex(0x171f29),
    t: [
        Rgba::hex(0xffb020),
        Rgba::hex(0xff7a45),
        Rgba::hex(0xc2688f),
        Rgba::hex(0x7a6fc4),
        Rgba::hex(0x4a7fb5),
        Rgba::hex(0x3b4a58),
    ],
    q_key: Rgba::hex(0x7fa9e0),
    q_val: Rgba::hex(0x6fc2a0),
    q_glob: Rgba::hex(0xb48ce0),
    q_not: Rgba::hex(0xe8825a),
    q_bad: Rgba::hex(0xff5f56),
};

/// The light scheme.
///
/// **`focus` is the browser's `#4a9eff`, not the window's `#2f6ba3`.** The two
/// had drifted: the page never redeclared `--focus` in its light block, so the
/// dark value carried over, while `theme.slint` had chosen a darker blue. The
/// browser's actual behaviour wins here because it is the one people have been
/// looking at. If the darker ring is the better answer it is now one edit, in
/// one place, for both windows.
pub const LIGHT: Palette = Palette {
    ground: Rgba::hex(0xf7f5f0),
    panel: Rgba::hex(0xfffefb),
    panel_2: Rgba::hex(0xf1efe9),
    line: Rgba::hex(0xddd8cd),
    line_soft: Rgba::hex(0xe8e4db),
    ink: Rgba::hex(0x1c1e21),
    ink_2: Rgba::hex(0x5c636c),
    ink_3: Rgba::hex(0x8a9198),
    mark: Rgba::wash(0xe2a000, 18),
    mark_ink: Rgba::hex(0x2a1f00),
    pick: Rgba::wash(0x0062cc, 13),
    focus: Rgba::hex(0x4a9eff),
    hover: Rgba::hex(0xefece4),
    t: [
        Rgba::hex(0xd98600),
        Rgba::hex(0xd4532a),
        Rgba::hex(0xa8447a),
        Rgba::hex(0x5f52ad),
        Rgba::hex(0x2f6ba3),
        Rgba::hex(0x97a1ac),
    ],
    q_key: Rgba::hex(0x2f6ba3),
    q_val: Rgba::hex(0x1a7a58),
    q_glob: Rgba::hex(0x6b3fa0),
    q_not: Rgba::hex(0xb8531f),
    q_bad: Rgba::hex(0xc0392b),
};

/// The numbers that are not colours.
///
/// Pixels, and they mean the same thing in both windows because both draw at
/// the same nominal scale and let the platform handle the rest.
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    /// The spacing unit everything else is a multiple of.
    pub unit: f32,
    /// A row in the detail list. The pitch the virtual list is built on.
    pub row: f32,
    /// Corner radius for a control.
    pub radius: f32,
    /// Body text.
    pub size: f32,
}

pub const METRICS: Metrics = Metrics {
    unit: 4.0,
    row: 30.0,
    radius: 6.0,
    size: 13.0,
};

/// The font stacks, as the browser wants them written.
///
/// A native window cannot use a list like this — it asks the platform for one
/// family — so the window takes the first name it can resolve and the list is
/// here so that both are choosing from the same set rather than from two.
pub const MONO: &str =
    r#"ui-monospace, "SF Mono", "JetBrains Mono", "Cascadia Mono", Menlo, Consolas, monospace"#;
pub const SANS: &str = r#"system-ui, -apple-system, "Segoe UI", Inter, Roboto, sans-serif"#;

/// The palette as CSS custom properties, without the surrounding braces.
///
/// Written here rather than in the bridge because the *names* are part of the
/// shared vocabulary: `--q-key` is the same idea as `Theme.q-key`, and a page
/// that renamed it would be a page this crate no longer describes.
pub fn css_vars(p: &Palette) -> String {
    let mut s = String::with_capacity(700);
    let mut put = |name: &str, c: &Rgba| {
        s.push_str("    --");
        s.push_str(name);
        s.push_str(": ");
        s.push_str(&c.css());
        s.push_str(";\n");
    };
    put("ground", &p.ground);
    put("panel", &p.panel);
    put("panel-2", &p.panel_2);
    put("line", &p.line);
    put("line-soft", &p.line_soft);
    put("ink", &p.ink);
    put("ink-2", &p.ink_2);
    put("ink-3", &p.ink_3);
    put("mark", &p.mark);
    put("mark-ink", &p.mark_ink);
    put("pick", &p.pick);
    put("focus", &p.focus);
    put("hover", &p.hover);
    for (i, c) in p.t.iter().enumerate() {
        put(&format!("t{i}"), c);
    }
    put("q-key", &p.q_key);
    put("q-val", &p.q_val);
    put("q-glob", &p.q_glob);
    put("q-not", &p.q_not);
    put("q-bad", &p.q_bad);
    s
}

/// The metrics and font stacks, as CSS custom properties.
pub fn css_metrics() -> String {
    format!(
        "    --mono: {MONO};\n    --sans: {SANS};\n    --u: {}px;\n    --row: {}px;\n    --radius: {}px;\n",
        METRICS.unit, METRICS.row, METRICS.radius
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two schemes describe the same set of colours.
    ///
    /// A scheme that forgot a field is how the drift started: the page's light
    /// block never redeclared `--focus`, so it silently kept a colour meant
    /// for a dark background. Here that cannot happen — the struct has the
    /// field or it does not compile — and this checks the weaker thing the
    /// type system cannot: that neither scheme left one at the other's value
    /// by accident.
    #[test]
    fn the_two_schemes_are_actually_different() {
        assert_ne!(DARK.ground, LIGHT.ground);
        assert_ne!(DARK.ink, LIGHT.ink);
        assert_ne!(DARK.q_key, LIGHT.q_key);
        for i in 0..6 {
            assert_ne!(DARK.t[i], LIGHT.t[i], "time band {i} is the same in both");
        }
    }

    /// The translucent colours stay translucent, and round the way CSS reads.
    #[test]
    fn a_wash_keeps_its_alpha() {
        assert_eq!(DARK.mark.css(), "rgba(255, 210, 74, .17)");
        assert_eq!(DARK.pick.css(), "rgba(74, 158, 255, .15)");
        assert_eq!(LIGHT.mark.css(), "rgba(226, 160, 0, .18)");
        assert_eq!(LIGHT.pick.css(), "rgba(0, 98, 204, .13)");
    }

    /// Opaque colours are written the short way, because that is what the page
    /// said before and a diff of the served page should be empty.
    #[test]
    fn an_opaque_colour_is_six_digits() {
        assert_eq!(DARK.ground.css(), "#0d1117");
        assert_eq!(DARK.q_key.css(), "#7fa9e0");
        assert_eq!(LIGHT.panel.css(), "#fffefb");
    }

    /// Slint takes alpha first; the browser takes it last. Getting this the
    /// wrong way round is invisible for `#ffffff` and wrong for everything.
    #[test]
    fn argb_is_alpha_first() {
        assert_eq!(DARK.focus.argb(), (255, 0x4a, 0x9e, 0xff));
        assert_eq!(DARK.mark.argb().0, 43);
    }
}
