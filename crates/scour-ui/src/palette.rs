//! Colours, sizes and fonts, and how a page writes them as CSS.

/// A colour, and the one representation both sides can be built from. Alpha is
/// carried so a row that is both matched and selected stays legible: at 15% the
/// selection was indistinguishable from hover, so it is 26% dark, 20% light.
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

    /// How the browser wants it: `#rrggbb`, or `rgba(…)` when translucent —
    /// two spellings rather than `#rrggbbaa`, because people read the page.
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

/// Every colour a window needs, in one of the two schemes. The field names are
/// the vocabulary: a window that needs a new colour adds it here, for both.
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
    /// The tint over a selected row: translucent, and blue where the match wash
    /// is yellow, since a row can be both at once.
    pub pick: Rgba,
    /// The ring around whatever has the keyboard.
    pub focus: Rgba,
    /// The row under the pointer.
    pub hover: Rgba,
    /// What a menu draws on the one item that changes something. Not an error
    /// colour: it marks the trash item before a hand gets there.
    pub danger: Rgba,
    /// The time spectrum, newest to oldest. Six bands, and rows are stored in
    /// date order, so the spectrum runs unbroken down the list.
    pub t: [Rgba; 6],
    /// The report's ring and its folder bar: one hue in six steps, darkest
    /// first. One hue on purpose — saturation belongs to the time spectrum, and
    /// a second set of hues beside it would read as age.
    pub k: [Rgba; 6],
    /// The slice for what was folded away: "the other five kinds".
    pub kx: Rgba,
    /// What is being looked for: a bare word. Blue against the red of `q_not`.
    /// The query palette is separate from the time spectrum, or "changed
    /// yesterday" and "field value" would share a colour.
    pub q_term: Rgba,
    /// Field name and comparison — structure.
    pub q_key: Rgba,
    /// Value, and text inside quotes — content.
    pub q_val: Rgba,
    /// Wildcard — a pattern rather than a word.
    pub q_glob: Rgba,
    /// Exclusion. Red, the opposite of `q_term`: `!` means not this.
    pub q_not: Rgba,
    /// Looks like a field, was searched for as text. Amber: a warning that the
    /// query does not mean what it looks like, not an error.
    pub q_bad: Rgba,
    /// A filter in the query, drawn as a capsule: its tint and its ring.
    pub cap: Rgba,
    pub cap_ring: Rgba,
    /// A `content:` capsule — files will be read.
    pub cap_read: Rgba,
    pub cap_read_ring: Rgba,
    /// An excluded term's capsule.
    pub cap_neg: Rgba,
    pub cap_neg_ring: Rgba,
}

/// The dark scheme: the desktop's own greys, a dark list under lighter chrome,
/// white ink, and a blue selection. A greyed-out dark read as faded and dead
/// (17 November); this is what GNOME's dark applications look like.
pub const DARK: Palette = Palette {
    ground: Rgba::hex(0x2a2a2c),
    panel: Rgba::hex(0x1e1e20),
    panel_2: Rgba::hex(0x323235),
    line: Rgba::hex(0x3d3d41),
    line_soft: Rgba::hex(0x2c2c2f),
    ink: Rgba::hex(0xffffff),
    ink_2: Rgba::hex(0xc8c8cc),
    ink_3: Rgba::hex(0x9a9aa0),
    mark: Rgba::wash(0xffd24a, 17),
    mark_ink: Rgba::hex(0x1a1200),
    // Neutral: the match wash is the only colour a row carries.
    pick: Rgba::wash(0x3584e4, 34),
    focus: Rgba::hex(0x78aeed),
    hover: Rgba::hex(0x28282b),
    // Legible on the dark panel without shouting.
    danger: Rgba::hex(0xd07070),
    t: [
        Rgba::hex(0xffb020),
        Rgba::hex(0xff7a45),
        Rgba::hex(0xc2688f),
        Rgba::hex(0x7a6fc4),
        Rgba::hex(0x4a7fb5),
        Rgba::hex(0x3b4a58),
    ],
    k: [
        Rgba::hex(0xa9c6e6),
        Rgba::hex(0x7fa9d6),
        Rgba::hex(0x5d8bbd),
        Rgba::hex(0x4a7fb5),
        Rgba::hex(0x35618d),
        Rgba::hex(0x264a6d),
    ],
    kx: Rgba::hex(0x3d3d41),
    q_term: Rgba::hex(0x59a6ff),
    q_key: Rgba::hex(0x7fa9e0),
    q_val: Rgba::hex(0x6fc2a0),
    q_glob: Rgba::hex(0xb48ce0),
    q_not: Rgba::hex(0xff6b6b),
    q_bad: Rgba::hex(0xe8a33d),
    cap: Rgba::hex(0x2b3444),
    cap_ring: Rgba::hex(0x3e4c63),
    cap_read: Rgba::hex(0x3d3218),
    cap_read_ring: Rgba::hex(0x6b5523),
    cap_neg: Rgba::hex(0x45262a),
    cap_neg_ring: Rgba::hex(0x6e3a3f),
};

/// The light scheme: a white list, grey chrome, no tint. The warm paper tried
/// first read as yellowed (17 November). `ink_3` keeps 4.5:1 on white.
pub const LIGHT: Palette = Palette {
    ground: Rgba::hex(0xf0f0f2),
    panel: Rgba::hex(0xffffff),
    panel_2: Rgba::hex(0xe8e8eb),
    line: Rgba::hex(0xd6d6da),
    line_soft: Rgba::hex(0xe6e6e9),
    ink: Rgba::hex(0x1a1a1c),
    ink_2: Rgba::hex(0x4a4a50),
    ink_3: Rgba::hex(0x707078),
    mark: Rgba::wash(0xe2a000, 18),
    mark_ink: Rgba::hex(0x2a1f00),
    pick: Rgba::wash(0x3584e4, 18),
    focus: Rgba::hex(0x3584e4),
    hover: Rgba::hex(0xf3f3f5),
    // Darker on paper, for the same weight against a light ground.
    danger: Rgba::hex(0xa8342c),
    t: [
        Rgba::hex(0xd98600),
        Rgba::hex(0xd4532a),
        Rgba::hex(0xa8447a),
        Rgba::hex(0x5f52ad),
        Rgba::hex(0x2f6ba3),
        Rgba::hex(0x97a1ac),
    ],
    k: [
        Rgba::hex(0x24507a),
        Rgba::hex(0x2f6ba3),
        Rgba::hex(0x5a8bbf),
        Rgba::hex(0x86abd3),
        Rgba::hex(0xb3c9e2),
        Rgba::hex(0xd7e2ee),
    ],
    kx: Rgba::hex(0xd6d6da),
    q_term: Rgba::hex(0x1c5fa8),
    q_key: Rgba::hex(0x2f6ba3),
    q_val: Rgba::hex(0x1a7a58),
    q_glob: Rgba::hex(0x6b3fa0),
    q_not: Rgba::hex(0xc0392b),
    q_bad: Rgba::hex(0x9a6a10),
    cap: Rgba::hex(0xe8eef7),
    cap_ring: Rgba::hex(0xc3d1e6),
    cap_read: Rgba::hex(0xfbf0d6),
    cap_read_ring: Rgba::hex(0xe5c886),
    cap_neg: Rgba::hex(0xfbe7e5),
    cap_neg_ring: Rgba::hex(0xeab4ad),
};

/// The numbers that are not colours: pixels at the same nominal scale in both
/// windows, with the platform handling the rest.
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
    row: 32.0,
    radius: 6.0,
    size: 14.0,
};

/// The font stacks, as the browser wants them written. A native window asks the
/// platform for one family and the trailing generic is not one, so it resolves
/// `monospace` through fontconfig — see `scour-gui`'s `mono_family`.
pub const MONO: &str =
    r#"ui-monospace, "SF Mono", "JetBrains Mono", "Cascadia Mono", Menlo, Consolas, monospace"#;

pub const SANS: &str = r#"system-ui, -apple-system, "Segoe UI", Inter, Roboto, sans-serif"#;

/// The palette as CSS custom properties, without the surrounding braces. The
/// names are part of the vocabulary: `--q-key` is `Theme.q-key`.
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
    put("danger", &p.danger);
    for (i, c) in p.t.iter().enumerate() {
        put(&format!("t{i}"), c);
    }
    for (i, c) in p.k.iter().enumerate() {
        put(&format!("k{i}"), c);
    }
    put("kx", &p.kx);
    put("q-term", &p.q_term);
    put("q-key", &p.q_key);
    put("q-val", &p.q_val);
    put("q-glob", &p.q_glob);
    put("q-not", &p.q_not);
    put("q-bad", &p.q_bad);
    put("cap", &p.cap);
    put("cap-ring", &p.cap_ring);
    put("cap-read", &p.cap_read);
    put("cap-read-ring", &p.cap_read_ring);
    put("cap-neg", &p.cap_neg);
    put("cap-neg-ring", &p.cap_neg_ring);
    s
}

/// The metrics and font stacks, as CSS custom properties.
pub fn css_metrics() -> String {
    format!(
        "    --mono: {MONO};\n    --sans: {SANS};\n    --u: {}px;\n    --row: {}px;\n    --radius: {}px;\n",
        METRICS.unit, METRICS.row, METRICS.radius
    )
}

/// The colour a kind's icon is drawn in: one hue a kind, the same in every
/// face, so the eye learns the list without reading it. `file` has no colour —
/// a plain file takes the window's own quiet ink.
pub fn kind_colour(token: &str) -> Option<Rgba> {
    Some(match token {
        "folder" => Rgba::hex(0x7d9bc4),
        "doc" => Rgba::hex(0x9aa5b1),
        "code" => Rgba::hex(0x7fbfa8),
        "image" => Rgba::hex(0xb98fc4),
        "video" | "media" => Rgba::hex(0xc48f9b),
        "audio" => Rgba::hex(0x8f9dc4),
        "archive" => Rgba::hex(0xc4a87f),
        "exec" => Rgba::hex(0xc49a7f),
        "data" => Rgba::hex(0x7fb3c4),
        "config" => Rgba::hex(0xa3a97f),
        "font" => Rgba::hex(0xc4b87f),
        "build" => Rgba::hex(0x8a8f99),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The struct makes the two schemes describe the same set of colours; this
    /// checks the weaker thing, that neither left a field at the other's value.
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
    /// Literal colours, not the palette's: these test the writing, and the
    /// palette is a choice that changes.
    #[test]
    fn a_wash_keeps_its_alpha() {
        assert_eq!(Rgba::wash(0xffd24a, 17).css(), "rgba(255, 210, 74, .17)");
        assert_eq!(Rgba::wash(0x0062cc, 20).css(), "rgba(0, 98, 204, .20)");
        assert_eq!(DARK.mark.css(), "rgba(255, 210, 74, .17)");
    }

    /// Opaque colours are written the short way, as the page writes them.
    #[test]
    fn an_opaque_colour_is_six_digits() {
        assert_eq!(Rgba::hex(0x0d1117).css(), "#0d1117");
        assert_eq!(Rgba::hex(0x7fa9e0).css(), "#7fa9e0");
    }

    /// Slint takes alpha first, the browser last: reversed, `#ffffff` still
    /// looks right and nothing else does.
    #[test]
    fn argb_is_alpha_first() {
        assert_eq!(Rgba::hex(0x4a9eff).argb(), (255, 0x4a, 0x9e, 0xff));
        assert_eq!(DARK.mark.argb().0, 43);
    }
}
