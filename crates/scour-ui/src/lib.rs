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

pub mod faces;
pub mod format;
pub mod path;
pub mod preview;

/// What every face says while the index is being walked.
///
/// **A number, not a word.** A face that says `scanning` and nothing else
/// cannot be told from one that is stuck, and the moment the question is
/// asked — somebody switched a skip rule off and is watching the counts for
/// proof — a still indicator is the same as no indicator. `scanned` climbs
/// several times a second during a walk, so it is the proof.
///
/// The msgid rather than the sentence, because the number has to be
/// punctuated in the reader's language and only the caller has the catalogue.
/// Empty when nothing is being walked, which is what a face draws nothing for.
pub const SCANNING: &str = "scanning {n}";
pub mod query;

/// A colour, and the one representation both sides can be built from.
///
/// Alpha is carried because the palette needs it: the match wash and the
/// selection tint are translucent on purpose, so that a row which is both
/// matched *and* selected stays legible.
///
/// **The selection was 15% and could not be seen.** At that alpha it lands two
/// or three values away from the row under the pointer, so "which row am I on"
/// and "which row is selected" were the same faint blue — and on a list of
/// hover, match and selection stacked, none of the three said which it was.
/// 26% dark and 20% light is still translucent enough that a matched row shows
/// its wash through, and is a colour rather than a suggestion of one.
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
    /// **What is being looked for.** A bare word — the thing the query is
    /// actually about.
    ///
    /// It used to be the ordinary ink, on the reasoning that the plain part of
    /// a query is plain. But a query line is read to answer two questions —
    /// what am I looking for, and what am I leaving out — and the answer to
    /// the first was the same colour as the punctuation around it. Blue for
    /// what is wanted, red for what is not, is the pair somebody can read
    /// without being taught.
    pub q_term: Rgba,
    /// Field name and comparison — structure.
    pub q_key: Rgba,
    /// Value, and text inside quotes — content.
    pub q_val: Rgba,
    /// Wildcard — a pattern rather than a word.
    pub q_glob: Rgba,
    /// **Exclusion.** Red, and the opposite of `q_term` on purpose.
    ///
    /// This was orange, on the reasoning that an exclusion is deliberate and
    /// red is for mistakes. True, and it lost the argument to the thing a
    /// person actually does with this line: `!` means *not this*, and the
    /// colour of not-this is red in every interface anybody has used.
    pub q_not: Rgba,
    /// Looks like a field, was searched for as text.
    ///
    /// Amber now that red belongs to exclusion — which is the better fit
    /// anyway: this is not an error, it is a warning that the query does not
    /// mean what it looks like it means.
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
    pick: Rgba::wash(0x4a9eff, 26),
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
    q_term: Rgba::hex(0x59a6ff),
    q_key: Rgba::hex(0x7fa9e0),
    q_val: Rgba::hex(0x6fc2a0),
    q_glob: Rgba::hex(0xb48ce0),
    q_not: Rgba::hex(0xff6b6b),
    q_bad: Rgba::hex(0xe8a33d),
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
    pick: Rgba::wash(0x0062cc, 20),
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
    q_term: Rgba::hex(0x1c5fa8),
    q_key: Rgba::hex(0x2f6ba3),
    q_val: Rgba::hex(0x1a7a58),
    q_glob: Rgba::hex(0x6b3fa0),
    q_not: Rgba::hex(0xc0392b),
    q_bad: Rgba::hex(0x9a6a10),
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

/// Words people write as operators that this language does not read as ones.
///
/// Whitespace is AND, `|` is OR, `!` is NOT — so `a or b` looks for three
/// words, one of which is "or", and nothing anywhere is marked wrong. It is
/// the one case where a query means something else entirely and the colouring
/// has nothing to say about it, so the reading is shown for it whether or not
/// anything else in the query earns a line.
pub const MISTAKEN: [&str; 6] = ["or", "and", "not", "ve", "veya", "değil"];

/// The font stacks, as the browser wants them written.
///
/// A native window cannot use a list like this — it asks the platform for one
/// family, and the generic at the end of this stack is *not* one: a toolkit
/// looks for a family literally called "monospace", finds none, and serves the
/// interface sans instead. So the window resolves the generic the way the rest
/// of the desktop does, through fontconfig, and lands on the same face this
/// list ends at. See `scour-gui`'s `mono_family`.
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
    put("q-term", &p.q_term);
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
        assert_eq!(DARK.pick.css(), "rgba(74, 158, 255, .26)");
        assert_eq!(LIGHT.mark.css(), "rgba(226, 160, 0, .18)");
        assert_eq!(LIGHT.pick.css(), "rgba(0, 98, 204, .20)");
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

/// Which way a column's text sits in its cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Start,
    /// Numbers, right-aligned so the digits line up down the column — the
    /// whole reason a size column is readable at a glance.
    End,
}

/// One column a window can show.
///
/// **The definition, not the drawing.** How a cell is painted is each window's
/// own business — one writes a `<td>`, the other a `Text` — but *which*
/// columns exist, what they are called, what sorting them asks the service
/// for, and how wide they start out are the same in both or the two windows
/// are two programs.
#[derive(Debug, Clone, Copy)]
pub struct Column {
    /// What the settings file calls it. Stable; the sort key is not.
    pub id: &'static str,
    /// The English source string, and therefore the catalogue key.
    pub msgid: &'static str,
    /// What `sort:` this column asks the service for. Empty means the column
    /// cannot be sorted by.
    pub sort: &'static str,
    /// Starting width in pixels. A person dragging the edge overrides it, and
    /// what they chose is kept per column id — see `scour_settings::widths`.
    pub width: u32,
    pub align: Align,
}

/// Every column, in the order a window shows them when nobody has said
/// otherwise.
///
/// The first five are the default set. The rest are there to be turned on.
pub const COLUMNS: &[Column] = &[
    Column {
        id: "name",
        msgid: "Name",
        sort: "name",
        width: 240,
        align: Align::Start,
    },
    Column {
        id: "kind",
        msgid: "Kind",
        sort: "kind",
        width: 110,
        align: Align::Start,
    },
    Column {
        id: "path",
        msgid: "Location",
        sort: "path",
        width: 320,
        align: Align::Start,
    },
    Column {
        id: "mtime",
        msgid: "Modified",
        sort: "modified",
        width: 120,
        align: Align::Start,
    },
    Column {
        id: "size",
        msgid: "Size",
        sort: "size",
        width: 90,
        align: Align::End,
    },
    Column {
        id: "ext",
        msgid: "Extension",
        sort: "ext",
        width: 80,
        align: Align::Start,
    },
    Column {
        id: "ctime",
        msgid: "Created",
        sort: "created",
        width: 120,
        align: Align::Start,
    },
    Column {
        id: "atime",
        msgid: "Accessed",
        sort: "accessed",
        width: 120,
        align: Align::Start,
    },
    Column {
        id: "perm",
        msgid: "Mode",
        sort: "mode",
        width: 108,
        align: Align::Start,
    },
    Column {
        id: "user",
        msgid: "Owner",
        sort: "uid",
        width: 100,
        align: Align::Start,
    },
    Column {
        id: "group",
        msgid: "Group",
        sort: "gid",
        width: 100,
        align: Align::Start,
    },
    Column {
        id: "disk",
        msgid: "On disk",
        sort: "disk",
        width: 90,
        align: Align::End,
    },
];

/// What a window shows before anybody has chosen.
///
/// Five, and the order is the reading order of the question people actually
/// ask: what is it called, what kind of thing is it, where does it live, when
/// did it change, how big is it.
pub const DEFAULT_COLUMNS: &[&str] = &["name", "kind", "path", "mtime", "size"];

/// Look one up by the id the settings file uses.
pub fn column(id: &str) -> Option<&'static Column> {
    COLUMNS.iter().find(|c| c.id == id)
}

#[cfg(test)]
mod column_tests {
    use super::*;

    /// Every default is a column that exists.
    ///
    /// A typo here is a window that starts with four columns and no complaint.
    #[test]
    fn the_defaults_all_name_real_columns() {
        for id in DEFAULT_COLUMNS {
            assert!(column(id).is_some(), "`{id}` is not a column");
        }
    }

    /// Ids are unique, because the settings file keys widths by them.
    #[test]
    fn no_id_is_used_twice() {
        let mut seen: Vec<&str> = Vec::new();
        for c in COLUMNS {
            assert!(!seen.contains(&c.id), "`{}` appears twice", c.id);
            seen.push(c.id);
        }
    }

    /// Numbers are the ones that right-align, and only those.
    ///
    /// Stated as a test because it is the rule a new column will be added
    /// against, and "size-ish" is not something the type can check.
    #[test]
    fn only_the_sizes_are_right_aligned() {
        for c in COLUMNS {
            let numeric = c.id == "size" || c.id == "disk";
            assert_eq!(
                c.align == Align::End,
                numeric,
                "`{}` is aligned the wrong way",
                c.id
            );
        }
    }
}

/// How many bars the time ribbon has.
pub const BAR_COUNT: usize = 24;

/// How far back the ribbon reaches, in days.
pub const BAR_SPAN_DAYS: f64 = 730.0;

/// The upper edge of each bar, in days, newest first.
///
/// **Logarithmic, so that the last day, the last week and the last year all
/// have room on one screen.** A linear scale spends twenty-three of its
/// twenty-four bars on "older than a month", which is the part nobody is
/// looking for.
///
/// These are what the service is asked for — `FacetBy::Age { edges }` — so the
/// two windows asking for different edges would be two windows drawing
/// different histograms of the same index.
pub fn bar_edges() -> Vec<u32> {
    let mut v: Vec<u32> = (0..BAR_COUNT)
        .map(|i| {
            let t = 1.0 - (i as f64) / (BAR_COUNT as f64);
            (((10f64.powf(t) - 1.0) / 9.0) * BAR_SPAN_DAYS)
                .round()
                .max(1.0) as u32
        })
        .collect();
    v.reverse();
    v
}

/// Which of the six time bands an age in days falls in, oldest last.
///
/// The same six the rows use for the date's colour, so a bar and the rows it
/// stands for are the same colour.
pub fn band_of(days: f64) -> usize {
    match days {
        d if d < 1.0 => 0,
        d if d < 7.0 => 1,
        d if d < 30.0 => 2,
        d if d < 365.0 => 3,
        d if d < 730.0 => 4,
        _ => 5,
    }
}

#[cfg(test)]
mod ribbon_tests {
    use super::*;

    /// The edges are what the browser page has always computed.
    ///
    /// Written out rather than recomputed, because the point is that this
    /// function replaced a line of JavaScript and has to produce the same
    /// twenty-four numbers — a ribbon whose bars mean something slightly
    /// different in each window is worse than two ribbons.
    #[test]
    fn the_edges_match_the_page() {
        let want: [u32; BAR_COUNT] = [
            8, 17, 27, 38, 50, 63, 78, 94, 111, 131, 152, 175, 201, 230, 261, 295, 333, 375, 421,
            471, 527, 588, 656, 730,
        ];
        assert_eq!(bar_edges(), want, "the ribbon's bars moved");
    }

    /// The ribbon reads left to right as time does.
    ///
    /// **`bar_edges()` is newest first and a ribbon is not.** The edges are
    /// upper bounds, so the smallest one is the newest bar; the ribbon's axis
    /// says "2 years ago" at its left end. A window drawing them in the order
    /// they arrive gets bars running backwards under an axis that does not —
    /// which is worse than no ribbon, because it is a ribbon that is
    /// confidently wrong. Both windows reverse; this says why in one place.
    #[test]
    fn the_edges_are_newest_first_and_the_ribbon_is_not() {
        let e = bar_edges();
        assert!(
            e[0] < e[e.len() - 1],
            "the edges stopped being newest first"
        );
        let drawn: Vec<u32> = e.iter().rev().copied().collect();
        assert_eq!(drawn[0], 730, "the leftmost bar is not the oldest");
        assert!(
            drawn[drawn.len() - 1] < 10,
            "the rightmost bar is not the newest"
        );
    }

    /// A bar's colour is the colour the rows in it get.
    #[test]
    fn a_band_is_one_of_six() {
        assert_eq!(band_of(0.5), 0);
        assert_eq!(band_of(3.0), 1);
        assert_eq!(band_of(10.0), 2);
        assert_eq!(band_of(100.0), 3);
        assert_eq!(band_of(400.0), 4);
        assert_eq!(band_of(1000.0), 5);
    }
}

/// The colour a kind's icon is drawn in.
///
/// **One hue a kind, and the same one in both windows.** These are what makes
/// a list of two hundred rows readable at a glance without reading a word of
/// it: the eye learns "blue-grey is a folder, green is code" in about a
/// screenful. The browser page paints an SVG mask with them; the native window
/// tints the same SVG. `file` has no colour — a plain file is drawn in the
/// window's own quiet ink, because "nothing in particular" is not a category.
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
