//! A capsule of the query bar dressed for drawing: its header as a picture
//! and a name, its value in words or in a well, and how wide each word is
//! drawn — asked of the window and guessed at until it answers.

use scour_core::Catalog;
use scour_i18n::Catalogue;
use scour_ui::lens;

use crate::Cap;
use crate::qbar;
use crate::qgroup::{self, Drawn};
use crate::qwire::QueryBar;

/// What a header drawn as a picture stands for: the filter's name in the list
/// `:` opens, and what it does.
fn field_tip(cat: &Catalogue, field: &str) -> String {
    lens::filter_of(field).map_or_else(
        || format!("{field}:"),
        |f| format!("{} — {}", cat.get(f.label), cat.get(f.about)),
    )
}

/// The name a header drawn as a picture carries beside it: the filter's, as
/// the list `:` opens says it. An ≈ says itself.
fn head_label(cat: &Catalogue, field: &str) -> String {
    if field == "fuzzy" {
        return String::new();
    }
    lens::filter_of(field).map_or_else(String::new, |f| cat.get(f.label).into_owned())
}

/// What an empty filter asks for, in the place its value will be.
fn head_hint(cat: &Catalogue, field: &str) -> String {
    let id = match field {
        "dm" => "when?",
        "size" => "how big?",
        "kind" => "which type?",
        "ext" => "pdf, rs…",
        "under" | "parent" => "which folder?",
        "content" => "text inside the files",
        _ => "a word",
    };
    cat.get(id).into_owned()
}

/// What a filter holds, in words, where its panel has words for it: `7d`
/// is "last 7 days", `2026-11-12..2026-11-25` "12–25 November",
/// `100kb..10mb` "100 KB to 10 MB", `doc` "Document".
fn friendly(cat: &Catalogue, field: &str, raw: &str) -> Option<String> {
    match field {
        "dm" => crate::qtime::words(raw, cat),
        "size" | "empty" => {
            let text = format!("{field}:{raw}");
            let t = lens::terms(&text).into_iter().next()?;
            Some(cat.get(lens::size_said(&text, &t)?).into_owned())
        }
        "kind" if lens::KINDS.contains(&raw) => scour_core::Kind::from_name(raw)
            .and_then(|k| k.first())
            .map(|k| cat.get(k.msgid()).into_owned()),
        _ => None,
    }
}

/// How wide the window draws `text` in a face of `qgroup`'s. Not measured
/// yet, it is asked for and guessed at meanwhile.
pub(crate) fn width_of(qb: &mut QueryBar, text: &str, face: i32) -> f32 {
    let key = (text.to_string(), face);
    if let Some(w) = qb.widths.get(&key) {
        return *w;
    }
    if !qb.asked.contains(&key) {
        qb.asked.push(key);
    }
    text.chars().count() as f32 * if face == qgroup::HEAD { 6.9 } else { 8.4 }
}

/// What the bar needs to know to dress a capsule.
pub(crate) struct Dress<'a> {
    pub(crate) cat: &'a Catalogue,
    pub(crate) words: bool,
    pub(crate) adv: f32,
    /// The caret as a char offset, and as a byte offset while it is a caret
    /// in the focused field.
    pub(crate) cur: usize,
    pub(crate) caret: Option<usize>,
}

/// A filter's header the caret is not in is drawn as a picture and a name,
/// when the settings ask for pictures. The filter the caret is in is being
/// written: its value as typed, in a well to type in. Any other says what
/// it holds in words, where its lens has words for it.
pub(crate) fn dress(qb: &mut QueryBar, d: &Dress, c: &mut Cap) {
    let start = qb.field.lines[c.line as usize].start + c.at as usize;
    c.icon = !d.words
        && qbar::has_icon(&c.field)
        && c.head > 0
        && !(d.cur > start && d.cur < start + c.head as usize);
    c.edit = c.head > 0
        && d.caret
            .is_some_and(|k| k > c.from as usize && k <= c.to as usize);
    if c.head > 0 {
        c.tail = qbar::VALUE_TAIL;
    }
    if !c.icon {
        return;
    }
    let field = c.field.to_string();
    c.tip = field_tip(d.cat, &field).into();
    let label = head_label(d.cat, &field);
    let name_w = if label.is_empty() {
        0.0
    } else {
        qbar::HEAD_JOIN + width_of(qb, &label, qgroup::HEAD)
    };
    c.label = label.into();
    c.head_w = qbar::HEAD_LEAD
        + qbar::HEAD_PIC
        + name_w
        + qbar::HEAD_TRAIL
        + qbar::HEAD_ROOM
        + if c.tone == 2 { qbar::NOT_W } else { 0.0 };
    let held = (c.len - c.head).max(0);
    if c.tone == 3 && c.whole && held == 0 {
        let hint = head_hint(d.cat, &field);
        c.tail = width_of(qb, &hint, qgroup::VALUE) + qbar::VALUE_TAIL + 10.0;
        c.hint = hint.into();
    } else if c.edit {
        c.tail = (qbar::WELL_MIN - held as f32 * d.adv).max(qbar::VALUE_TAIL);
    } else if c.whole
        && held as usize == c.raw.chars().count()
        && let Some(nice) = friendly(d.cat, &field, c.raw.as_str())
    {
        c.nice_w = width_of(qb, &nice, qgroup::VALUE);
        c.nice = nice.into();
    }
}

/// The capsules of the field's lines, dressed, the members of `groups` left out.
pub(crate) fn dressed(
    qb: &mut QueryBar,
    d: &Dress,
    text: &str,
    spans: &[scour_core::Span],
    groups: &[Drawn],
) -> Vec<Cap> {
    let mut caps = qb.field.caps(text, spans);
    caps.retain(|c| {
        !groups
            .iter()
            .any(|g| g.start <= c.from as usize && c.to as usize <= g.end)
    });
    for c in &mut caps {
        dress(qb, d, c);
    }
    caps
}

/// A chip capsule's header: as wide as its filter's capsule's, and where
/// each of its `chars` letters is, drawn small when the settings ask for
/// words. Excluded, it holds the `!` too.
pub(crate) fn head(
    qb: &mut QueryBar,
    d: &Dress,
    field: &str,
    chars: usize,
    not: bool,
) -> (f32, f32) {
    if d.words {
        let each = d.adv * qbar::HEAD_SCALE - qbar::HEAD_TIGHT;
        return (chars as f32 * each + qbar::HEAD_GAP, each);
    }
    let mut c = Cap {
        head: 1,
        field: field.into(),
        tone: if not { 2 } else { 0 },
        ..Cap::default()
    };
    dress(
        qb,
        &Dress {
            cur: usize::MAX,
            ..*d
        },
        &mut c,
    );
    (c.head_w, 0.0)
}
