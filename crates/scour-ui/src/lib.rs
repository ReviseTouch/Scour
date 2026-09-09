//! What every Scour window agrees on: data, and nothing else. Each face turns
//! it into what its toolkit wants — CSS variables, Slint globals, escape codes
//! — and this crate has no dependencies. What is shared is the vocabulary,
//! never the layout: which colour means "a field name", how tall a row is.

pub mod faces;
pub mod format;
pub mod path;
pub mod preview;

/// What every face says while the index is being walked. A number, not a word:
/// a still indicator cannot be told from a stuck one. The msgid rather than the
/// sentence, because only the caller can punctuate a number.
pub const SCANNING: &str = "scanning {n}";

/// The index has grown an unsorted tail, and searching has slowed for it: a
/// week of ordinary use took ordering by path from 1.9 ms to 21.5, and one
/// rebuild put it back. Names the command, not only the condition.
pub const REBUILD_ADVISED: &str = "`scour maintain rebuild` would speed searches up";
pub mod menu;
pub mod query;

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
    /// colour: it marks the wastebasket item before a hand gets there.
    pub danger: Rgba,
    /// The time spectrum, newest to oldest. Six bands, and rows are stored in
    /// date order, so the spectrum runs unbroken down the list.
    pub t: [Rgba; 6],
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
    q_term: Rgba::hex(0x59a6ff),
    q_key: Rgba::hex(0x7fa9e0),
    q_val: Rgba::hex(0x6fc2a0),
    q_glob: Rgba::hex(0xb48ce0),
    q_not: Rgba::hex(0xff6b6b),
    q_bad: Rgba::hex(0xe8a33d),
};

/// The light scheme. `focus` stays `#4a9eff` here rather than darkening, which
/// is the ring people have actually been looking at.
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
    q_term: Rgba::hex(0x1c5fa8),
    q_key: Rgba::hex(0x2f6ba3),
    q_val: Rgba::hex(0x1a7a58),
    q_glob: Rgba::hex(0x6b3fa0),
    q_not: Rgba::hex(0xc0392b),
    q_bad: Rgba::hex(0x9a6a10),
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
    row: 30.0,
    radius: 6.0,
    size: 13.0,
};

/// Words people write as operators that this language does not read as ones:
/// `a or b` looks for three words, and nothing in the colouring says so.
pub const MISTAKEN: [&str; 6] = ["or", "and", "not", "ve", "veya", "değil"];

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
    #[test]
    fn a_wash_keeps_its_alpha() {
        assert_eq!(DARK.mark.css(), "rgba(255, 210, 74, .17)");
        assert_eq!(DARK.pick.css(), "rgba(74, 158, 255, .26)");
        assert_eq!(LIGHT.mark.css(), "rgba(226, 160, 0, .18)");
        assert_eq!(LIGHT.pick.css(), "rgba(0, 98, 204, .20)");
    }

    /// Opaque colours are written the short way, as the page writes them.
    #[test]
    fn an_opaque_colour_is_six_digits() {
        assert_eq!(DARK.ground.css(), "#0d1117");
        assert_eq!(DARK.q_key.css(), "#7fa9e0");
        assert_eq!(LIGHT.panel.css(), "#fffefb");
    }

    /// Slint takes alpha first, the browser last: reversed, `#ffffff` still
    /// looks right and nothing else does.
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
    /// Numbers, right-aligned so the digits line up down the column.
    End,
}

/// One column a window can show: the definition, not the drawing. Which columns
/// exist, their names, sort keys and starting widths are the same in every face.
#[derive(Debug, Clone, Copy)]
pub struct Column {
    /// What the settings file calls it. Stable; the sort key is not.
    pub id: &'static str,
    /// The English source string, and therefore the catalogue key.
    pub msgid: &'static str,
    /// What `sort:` this column asks for; empty means it cannot be sorted by.
    pub sort: &'static str,
    /// What it asks for in pixels before anything is shared out or taken back.
    /// A drag overrides it, kept per column id in `scour_settings::widths`.
    pub width: u32,
    /// Never squeezed narrower than this: the floor is what keeps the date and
    /// the size on screen. A column on its floor stops paying, so the two text
    /// columns carry the rest — a path that elides is still a path.
    pub min: u32,
    /// Its share of the stretching room in a **narrow** window, as a percentage;
    /// zero never stretches and keeps [`width`](Column::width). The share slides
    /// to [`far`](Column::far) between [`NARROW`] and [`WIDE`], continuously, so
    /// the name leads in a narrow window and the location in a wide one.
    pub near: u8,
    /// Its share in a **wide** window, as a percentage. See [`near`](Column::near).
    pub far: u8,
    /// As wide as growth may take it; zero is no ceiling. Only the name has
    /// one: past it, what it declines goes round again to the location.
    pub max: u32,
    pub align: Align,
}

/// Every column, in default order. The first five are the default set; the rest
/// are there to be turned on.
pub const COLUMNS: &[Column] = &[
    Column {
        id: "name",
        msgid: "Name",
        sort: "name",
        width: 240,
        min: 110,
        near: 52,
        far: 30,
        max: 520,
        align: Align::Start,
    },
    Column {
        id: "kind",
        msgid: "Kind",
        sort: "kind",
        width: 110,
        min: 68,
        near: 0,
        far: 0,
        max: 0,
        align: Align::Start,
    },
    Column {
        id: "path",
        msgid: "Location",
        sort: "path",
        width: 320,
        min: 120,
        near: 48,
        far: 70,
        max: 0,
        align: Align::Start,
    },
    Column {
        id: "mtime",
        msgid: "Modified",
        sort: "modified",
        width: 120,
        min: 100,
        near: 0,
        far: 0,
        max: 0,
        align: Align::Start,
    },
    Column {
        id: "size",
        msgid: "Size",
        sort: "size",
        width: 90,
        min: 68,
        near: 0,
        far: 0,
        max: 0,
        align: Align::End,
    },
    Column {
        id: "ext",
        msgid: "Extension",
        sort: "ext",
        width: 80,
        min: 56,
        near: 0,
        far: 0,
        max: 0,
        align: Align::Start,
    },
    Column {
        id: "ctime",
        msgid: "Created",
        sort: "created",
        width: 120,
        min: 100,
        near: 0,
        far: 0,
        max: 0,
        align: Align::Start,
    },
    Column {
        id: "atime",
        msgid: "Accessed",
        sort: "accessed",
        width: 120,
        min: 100,
        near: 0,
        far: 0,
        max: 0,
        align: Align::Start,
    },
    Column {
        id: "perm",
        msgid: "Mode",
        sort: "mode",
        width: 108,
        min: 92,
        near: 0,
        far: 0,
        max: 0,
        align: Align::Start,
    },
    Column {
        id: "user",
        msgid: "Owner",
        sort: "uid",
        width: 100,
        min: 72,
        near: 0,
        far: 0,
        max: 0,
        align: Align::Start,
    },
    Column {
        id: "group",
        msgid: "Group",
        sort: "gid",
        width: 100,
        min: 72,
        near: 0,
        far: 0,
        max: 0,
        align: Align::Start,
    },
    Column {
        id: "disk",
        msgid: "On disk",
        sort: "disk",
        width: 90,
        min: 68,
        near: 0,
        far: 0,
        max: 0,
        align: Align::End,
    },
];

/// What a window shows before anybody has chosen, in the reading order of the
/// question: what, what kind, where, when, how big.
pub const DEFAULT_COLUMNS: &[&str] = &["name", "kind", "path", "mtime", "size"];

/// Look one up by the id the settings file uses.
pub fn column(id: &str) -> Option<&'static Column> {
    COLUMNS.iter().find(|c| c.id == id)
}

/// The window width at which a stretching column asks for its [`Column::near`]
/// share, and below which it asks for nothing more: under 700px the floors are
/// already doing the deciding.
pub const NARROW: u32 = 700;
/// And where it asks for its [`Column::far`] share, and stops changing. A
/// laptop leaves near a thousand pixels, so the slide covers the working range.
pub const WIDE: u32 = 1900;

/// How wide each of the columns on screen is, given the room there is. The
/// widths add up to `avail` exactly at every size, which is the property the
/// whole thing exists for; only a room narrower than one pixel a column fails.
///
/// Four moves: what each column asks for (a drag, else its own width, else a
/// share of what is left that slides from [`Column::near`] to [`Column::far`]);
/// share out the remainder, a column at its [`Column::max`] dropping out; take
/// back the overflow in proportion to what each has above its floor; and below
/// [`floor_width`] let the floors give way together rather than draw a column
/// off the right-hand edge.
///
/// `chosen` is the width a person dragged that column to, or `None`. Zero
/// counts as `None`, as the settings file means it.
pub fn lay_out(ids: &[&str], chosen: impl Fn(&str) -> Option<u32>, avail: u32) -> Vec<u32> {
    let cols: Vec<&Column> = ids.iter().filter_map(|id| column(id)).collect();
    if cols.is_empty() {
        return Vec::new();
    }
    let set: Vec<Option<u32>> = cols
        .iter()
        .map(|c| chosen(c.id).filter(|w| *w > 0))
        .collect();

    // The room, less the columns that never stretch. A dragged one stays in
    // the budget — see `DRAG_CAP` — so narrowing the window is shared.
    let taken: u32 = cols
        .iter()
        .zip(&set)
        .filter(|(c, _)| c.near == 0)
        .map(|(c, s)| s.unwrap_or(c.width).max(c.min))
        .sum();
    let budget = avail.saturating_sub(taken);
    let stretchy: Vec<usize> = (0..cols.len()).filter(|&i| cols[i].near > 0).collect();

    let mut w: Vec<u32> = cols
        .iter()
        .zip(&set)
        .map(|(c, s)| s.unwrap_or(c.width).max(c.min))
        .collect();
    // A dragged width is a wish, not a lock: honoured while there is room, and
    // capped at this share of the stretching budget when there is not, so a
    // narrowing window shrinks both text columns instead of one. 45 rather
    // than 60, at which the location fell from 942px to 212 before the dragged
    // column moved. A floor under the column's own sliding share, not a ceiling.
    const DRAG_CAP: u32 = 45;
    let mut handed = 0;
    for (n, &i) in stretchy.iter().enumerate() {
        let want = match set[i] {
            // Dragged: what was asked for while it fits a fair share, and
            // never less than the column would have had untouched.
            Some(chosen) => {
                let fair = share_at(cols[i], avail).max(DRAG_CAP * 10);
                chosen.min(budget * fair / 1000)
            }
            // Not dragged and last: whatever the others left, so the shares
            // add up to the budget exactly however they rounded.
            None if n + 1 == stretchy.len() => budget.saturating_sub(handed),
            None => budget * share_at(cols[i], avail) / 1000,
        };
        handed += want;
        w[i] = want.max(cols[i].min);
        if cols[i].max > 0 {
            w[i] = w[i].min(cols[i].max).max(cols[i].min);
        }
    }

    let sum: u32 = w.iter().sum();
    if sum < avail {
        share_out(&cols, &set, &mut w, avail - sum);
    } else if sum > avail {
        take_back(&cols, &mut w, sum - avail);
        squash(&mut w, avail);
    }
    w
}

/// What share of the stretching room this column asks for at this width, in
/// parts per thousand. Straight-line and integer the whole way, because the
/// browser page has to reach the same number.
fn share_at(c: &Column, avail: u32) -> u32 {
    let t = if avail <= NARROW {
        0
    } else if avail >= WIDE {
        1000
    } else {
        (avail - NARROW) * 1000 / (WIDE - NARROW)
    };
    (u32::from(c.near) * 10 * (1000 - t) + u32::from(c.far) * 10 * t) / 1000
}

/// The narrowest these columns can be drawn with every floor still honoured.
/// Below it, [`lay_out`] still fits them in, but under the widths they need.
pub fn floor_width(ids: &[&str]) -> u32 {
    ids.iter().filter_map(|id| column(id)).map(|c| c.min).sum()
}

/// Move 2: hand out what is still spare, and let a column that fills up pass.
fn share_out(cols: &[&Column], set: &[Option<u32>], w: &mut [u32], spare: u32) {
    let mut left = spare;
    loop {
        // Recomputed each round, because a column that reached its ceiling is
        // no longer open and its share has to go somewhere.
        let open: Vec<usize> = (0..cols.len())
            .filter(|&i| {
                set[i].is_none() && cols[i].near > 0 && (cols[i].max == 0 || w[i] < cols[i].max)
            })
            .collect();
        let weight: u32 = open.iter().map(|&i| u32::from(cols[i].far).max(1)).sum();
        if left == 0 || weight == 0 {
            return;
        }
        let mut spent = 0;
        for (n, &i) in open.iter().enumerate() {
            // The last one takes the rounding too, or a hairline of panel
            // shows past the last column.
            let share = if n + 1 == open.len() {
                left - spent
            } else {
                left * u32::from(cols[i].far).max(1) / weight
            };
            let before = w[i];
            let want = before + share;
            w[i] = if cols[i].max > 0 {
                want.min(cols[i].max)
            } else {
                want
            };
            spent += w[i] - before;
        }
        // Every open column filled up and none could take the remainder;
        // another round would find the same thing.
        if spent == 0 {
            return;
        }
        left -= spent;
    }
}

/// Move 4, and only when move 3 ran out of room: everything in proportion. The
/// floors say which column gives way first, not that the window can keep them —
/// 400px for five floors adding to 466 has no ordering that works.
fn squash(w: &mut [u32], avail: u32) {
    let sum: u32 = w.iter().sum();
    // Not a window: below a pixel a column there is nothing to say.
    if sum <= avail || sum == 0 || avail < w.len() as u32 {
        return;
    }
    // The widest takes the rounding, because a pixel matters least to it.
    let widest = (0..w.len()).max_by_key(|&i| w[i]).unwrap_or(0);
    let mut spent = 0;
    for (i, x) in w.iter_mut().enumerate() {
        if i == widest {
            continue;
        }
        *x = (u64::from(*x) * u64::from(avail) / u64::from(sum)).max(1) as u32;
        spent += *x;
    }
    w[widest] = avail.saturating_sub(spent).max(1);
}

/// Move 3: take `over` back, in proportion to what each column has to spare.
fn take_back(cols: &[&Column], w: &mut [u32], over: u32) {
    let mut left = over;
    loop {
        let open: Vec<usize> = (0..cols.len()).filter(|&i| w[i] > cols[i].min).collect();
        let room: u32 = open.iter().map(|&i| w[i] - cols[i].min).sum();
        // Every column on its floor and the window still too narrow: nothing
        // here can fix that, and `squash` takes over.
        if left == 0 || room == 0 {
            return;
        }
        let take = left.min(room);
        let mut taken = 0;
        for (n, &i) in open.iter().enumerate() {
            let want = if n + 1 == open.len() {
                take - taken
            } else {
                take * (w[i] - cols[i].min) / room
            };
            let share = want.min(w[i] - cols[i].min);
            w[i] -= share;
            taken += share;
        }
        if taken == 0 {
            return;
        }
        left -= taken;
    }
}

#[cfg(test)]
mod column_tests {
    use super::*;

    /// Every default is a column that exists: a typo starts a window with four
    /// columns and no complaint.
    #[test]
    fn the_defaults_all_name_real_columns() {
        for id in DEFAULT_COLUMNS {
            assert!(column(id).is_some(), "`{id}` is not a column");
        }
    }

    /// Every column can be drawn: a floor above the width it asks for would
    /// mean a column that starts out already squeezed.
    #[test]
    fn no_floor_is_above_the_width_it_guards() {
        for c in COLUMNS {
            assert!(
                c.min > 0,
                "`{}` has no floor and can be squeezed away",
                c.id
            );
            assert!(
                c.min <= c.width,
                "`{}`: floor {} over width {}",
                c.id,
                c.min,
                c.width
            );
            if c.max > 0 {
                assert!(c.max >= c.width, "`{}`: ceiling below its own width", c.id);
                assert!(c.near > 0, "`{}` has a ceiling it can never reach", c.id);
            }
            // A share at both ends or neither: one of the two left at zero
            // collapses the column to its floor at that end of the range.
            assert_eq!(
                c.near == 0,
                c.far == 0,
                "`{}` stretches at one end of the range only",
                c.id
            );
        }
        // Percentages of one row, and they must read as such at both ends.
        for word in ["near", "far"] {
            let total: u32 = DEFAULT_COLUMNS
                .iter()
                .filter_map(|id| column(id))
                .map(|c| u32::from(if word == "near" { c.near } else { c.far }))
                .sum();
            assert_eq!(total, 100, "the {word} shares add up to {total}%, not 100");
        }
        // Something has to be able to absorb, or no window ever fits exactly.
        assert!(
            COLUMNS.iter().any(|c| c.near > 0 && c.max == 0),
            "nothing can take up the slack"
        );
    }

    /// The invariant: the columns add up to the room, whatever the window does.
    #[test]
    fn the_widths_add_up_to_the_room_at_every_size() {
        for avail in (200..3600).step_by(7) {
            let w = lay_out(DEFAULT_COLUMNS, |_| None, avail);
            assert_eq!(
                w.iter().sum::<u32>(),
                avail,
                "{avail}px: {w:?} does not add up"
            );
        }
    }

    /// A name dragged wide must not push the date and the size off the edge.
    #[test]
    fn a_name_dragged_far_too_wide_does_not_push_the_date_and_size_off() {
        let avail = 900;
        let w = lay_out(DEFAULT_COLUMNS, |id| (id == "name").then_some(4000), avail);
        assert_eq!(w.iter().sum::<u32>(), avail);
        for (c, got) in COLUMNS
            .iter()
            .filter(|c| DEFAULT_COLUMNS.contains(&c.id))
            .zip(&w)
        {
            assert!(*got >= c.min, "`{}` fell to {got}, under its floor", c.id);
        }
        // And the two on the right are still readable rather than a sliver.
        let mtime = w[DEFAULT_COLUMNS.iter().position(|i| *i == "mtime").unwrap()];
        let size = w[DEFAULT_COLUMNS.iter().position(|i| *i == "size").unwrap()];
        assert!(mtime >= column("mtime").unwrap().min);
        assert!(size >= column("size").unwrap().min);
    }

    /// A wide screen is the location's, not the name's.
    #[test]
    fn the_name_stops_at_its_ceiling_and_the_location_takes_the_rest() {
        let w = lay_out(DEFAULT_COLUMNS, |_| None, 2400);
        let at = |id: &str| w[DEFAULT_COLUMNS.iter().position(|i| *i == id).unwrap()];
        assert_eq!(at("name"), column("name").unwrap().max, "the name ran on");
        assert!(
            at("path") > at("name"),
            "a path elides worse than a name and got less room: {} vs {}",
            at("path"),
            at("name")
        );
        // The columns that never grow are exactly the width they asked for.
        for id in ["kind", "mtime", "size"] {
            assert_eq!(at(id), column(id).unwrap().width, "`{id}` grew");
        }
    }

    /// A dragged width is a wish, not a lock: honoured while there is room,
    /// following its own share down when there is not.
    #[test]
    fn a_dragged_column_gives_way_too_once_there_is_no_room() {
        let pinned = |id: &str| (id == "name").then_some(359);
        let at = |w: &[u32], id: &str| w[DEFAULT_COLUMNS.iter().position(|i| *i == id).unwrap()];

        // Wide: exactly what was asked for.
        let wide = lay_out(DEFAULT_COLUMNS, pinned, 1621);
        assert_eq!(at(&wide, "name"), 359, "the drag was not honoured");

        // Narrow: it moved, and it is not the location paying alone any more.
        let tight = lay_out(DEFAULT_COLUMNS, pinned, 850);
        assert!(at(&tight, "name") < 300, "the dragged column barely moved");

        for avail in (400..1700).step_by(11) {
            let w = lay_out(DEFAULT_COLUMNS, pinned, avail);
            let free = lay_out(DEFAULT_COLUMNS, |_| None, avail);
            assert_eq!(w.iter().sum::<u32>(), avail, "at {avail}px: {w:?}");
            // Never wider than asked for…
            assert!(at(&w, "name") <= 359, "a drag grew at {avail}px");
            // …and never narrower than it would have been untouched, which
            // would turn asking for a width into a penalty for having asked.
            assert!(
                at(&w, "name") >= at(&free, "name").min(359),
                "at {avail}px the drag cost it: {} against {}",
                at(&w, "name"),
                at(&free, "name")
            );
        }
    }

    /// What the sliding shares are for: the two text columns trade places as
    /// the window grows, and they do it without a step.
    #[test]
    fn the_name_leads_in_a_narrow_window_and_the_location_in_a_wide_one() {
        let at = |w: &[u32], id: &str| w[DEFAULT_COLUMNS.iter().position(|i| *i == id).unwrap()];

        let tight = lay_out(DEFAULT_COLUMNS, |_| None, NARROW);
        assert!(
            at(&tight, "name") >= at(&tight, "path"),
            "narrow: name {} is not leading the location {}",
            at(&tight, "name"),
            at(&tight, "path")
        );
        let roomy = lay_out(DEFAULT_COLUMNS, |_| None, 2400);
        assert!(
            at(&roomy, "path") > at(&roomy, "name") * 2,
            "wide: the location {} has not overtaken the name {}",
            at(&roomy, "path"),
            at(&roomy, "name")
        );

        // No step anywhere between: one pixel of window is a pixel or two of
        // column. The 4 is integer rounding on a share carried in parts per
        // thousand, not a threshold.
        let mut last = lay_out(DEFAULT_COLUMNS, |_| None, 500);
        for avail in 501..3000 {
            let now = lay_out(DEFAULT_COLUMNS, |_| None, avail);
            for (i, id) in DEFAULT_COLUMNS.iter().enumerate() {
                let step = now[i].abs_diff(last[i]);
                assert!(step <= 4, "`{id}` jumped {step}px at {avail}px");
            }
            last = now;
        }
    }

    /// What a person dragged is kept, and never quietly grown back.
    #[test]
    fn a_dragged_width_is_not_a_hole_to_pour_space_into() {
        let w = lay_out(DEFAULT_COLUMNS, |id| (id == "name").then_some(160), 2400);
        assert_eq!(w[0], 160, "the name was grown past what was chosen");
        assert_eq!(w.iter().sum::<u32>(), 2400);
        // A remembered width from a narrower window is pulled up to the floor.
        let w = lay_out(DEFAULT_COLUMNS, |id| (id == "size").then_some(3), 1400);
        let size = w[DEFAULT_COLUMNS.iter().position(|i| *i == "size").unwrap()];
        assert_eq!(size, column("size").unwrap().min);
    }

    /// Below every floor at once, they all give way together.
    #[test]
    fn too_narrow_for_the_floors_cramps_them_rather_than_losing_any() {
        let floor = floor_width(DEFAULT_COLUMNS);
        for avail in 10..floor {
            let w = lay_out(DEFAULT_COLUMNS, |_| None, avail);
            assert_eq!(w.iter().sum::<u32>(), avail, "at {avail}px: {w:?}");
            assert!(w.iter().all(|x| *x > 0), "a column vanished at {avail}px");
            // Still in the same proportions: the location is the widest.
            assert_eq!(
                w.iter().position(|x| *x == *w.iter().max().unwrap()),
                Some(2),
                "at {avail}px the location stopped being the widest: {w:?}"
            );
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

    /// Numbers are the ones that right-align, and only those — a rule the type
    /// cannot check.
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

/// The upper edge of each bar, in days, newest first. Logarithmic, so the last
/// day, week and year all have room: a linear scale spends 23 of 24 bars on
/// "older than a month". The service is asked for exactly these edges.
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

/// Which of the six time bands an age in days falls in, oldest last — the same
/// six the rows use, so a bar and the rows it stands for match.
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

    /// The edges, written out rather than recomputed: every face must draw the
    /// same twenty-four numbers.
    #[test]
    fn the_edges_match_the_page() {
        let want: [u32; BAR_COUNT] = [
            8, 17, 27, 38, 50, 63, 78, 94, 111, 131, 152, 175, 201, 230, 261, 295, 333, 375, 421,
            471, 527, 588, 656, 730,
        ];
        assert_eq!(bar_edges(), want, "the ribbon's bars moved");
    }

    /// The ribbon reads left to right as time does, and `bar_edges()` is newest
    /// first: the edges are upper bounds, so every face reverses them before
    /// drawing or the bars run backwards under the axis.
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
