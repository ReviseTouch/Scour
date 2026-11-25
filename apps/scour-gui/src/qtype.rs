//! The type panel as this window keeps it: the kinds as tiles with their
//! share of the query, and under them the extensions within the kinds
//! chosen (the query's own top ones when none is). `kind:` opens it on the
//! tiles, `ext:` on the extensions. Counts come from `lens::type_ask`'s two
//! facets, made exact by an age group; one walk when both ask the same.

use std::collections::HashMap;

use scour_core::FacetBy;
use scour_proto::Response;
use scour_ui::lens::{self, EXT_TOP, KIND_LABELS};

use crate::qlens::{Env, Item, Open};
use crate::qpanel::{self, Want};

pub const KINDS: u32 = 0;
pub const EXTS: u32 = 1;

/// One kind as its tile shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Tile {
    pub token: &'static str,
    /// Its msgid.
    pub label: &'static str,
    pub n: Option<u64>,
    /// Its count against everything the query finds.
    pub share: f32,
    pub on: bool,
    pub off: bool,
}

/// One extension as its chip shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Chip {
    pub value: String,
    pub n: Option<u64>,
    pub on: bool,
    pub off: bool,
}

/// The age group that lifts the facet's sampling cap: its counts are exact.
fn exact() -> FacetBy {
    FacetBy::Age { edges: vec![0] }
}

pub fn fill(o: &mut Open, seen: &HashMap<String, Response>, q: &str) -> Vec<Want> {
    let mut out = Vec::new();
    // An empty filter is one not written yet, which the search leaves out:
    // left in, it would split one walk into two.
    let q = if o.term.value.trim().is_empty() {
        lens::without(q, &o.term)
    } else {
        q.to_string()
    };
    let ask = lens::type_ask(&q);
    if lens::reads(&ask.kinds) || lens::reads(&ask.exts) {
        return out;
    }
    let ext = FacetBy::Ext { top: EXT_TOP };
    if ask.one_walk() {
        let by = vec![FacetBy::Kind, ext, exact()];
        let key = format!("type {}", ask.kinds);
        let f = qpanel::ask_facets(seen, KINDS, key, &ask.kinds, by, &mut out);
        o.got.kinds = f.clone();
        o.got.exts = f;
    } else {
        let key = format!("kinds {}", ask.kinds);
        let by = vec![FacetBy::Kind, exact()];
        o.got.kinds = qpanel::ask_facets(seen, KINDS, key, &ask.kinds, by, &mut out);
        let key = format!("exts {}", ask.exts);
        let by = vec![ext, exact()];
        o.got.exts = qpanel::ask_facets(seen, EXTS, key, &ask.exts, by, &mut out);
    }
    out
}

/// The thirteen kinds, in the model's order.
pub fn tiles(o: &Open, q: &str) -> Vec<Tile> {
    let picked = lens::picked(q, "kind");
    let f = o.got.kinds.as_ref();
    let counts = f
        .map(|f| qpanel::group(f, |b| *b == FacetBy::Kind))
        .unwrap_or_default();
    let total = f.map_or(0, |f| f.total).max(1);
    KIND_LABELS
        .iter()
        .map(|&(token, label)| {
            let n = f.map(|_| {
                counts
                    .iter()
                    .find(|(k, _)| *k == token)
                    .map_or(0, |(_, n)| *n)
            });
            Tile {
                token,
                label,
                n,
                share: n.map_or(0.0, |n| n as f32 / total as f32),
                on: picked.on.iter().any(|v| v == token),
                off: picked.off.iter().any(|v| v == token),
            }
        })
        .collect()
}

/// The extensions: the facet's top ones by count, then any the query holds
/// that are not among them, so a chosen one can always be taken out.
pub fn chips(o: &Open, q: &str) -> Vec<Chip> {
    let picked = lens::picked(q, "ext");
    let counts = o
        .got
        .exts
        .as_ref()
        .map(|f| qpanel::group(f, |b| matches!(b, FacetBy::Ext { .. })))
        .unwrap_or_default();
    let mut out: Vec<Chip> = counts
        .iter()
        .map(|(k, n)| Chip {
            value: (*k).to_string(),
            n: Some(*n),
            on: false,
            off: false,
        })
        .collect();
    for v in picked.on.iter().chain(&picked.off) {
        if !out.iter().any(|c| c.value == *v) {
            out.push(Chip {
                value: v.clone(),
                n: None,
                on: false,
                off: false,
            });
        }
    }
    for c in &mut out {
        c.on = picked.on.contains(&c.value);
        c.off = picked.off.contains(&c.value);
    }
    out
}

/// What the keys move over: the tiles, then the extensions.
pub fn items(o: &mut Open, q: &str, env: &Env) -> Vec<Item> {
    let tr = env.tr;
    o.title = tr("Type");
    o.foot = tr("click: add or take away · Alt+click: leave out");
    let mut out: Vec<Item> = tiles(o, q)
        .into_iter()
        .map(|t| Item {
            label: tr(t.label),
            value: Some(("kind", t.token.to_string())),
            stays: true,
            ..Default::default()
        })
        .collect();
    out.extend(chips(o, q).into_iter().map(|c| Item {
        label: format!(".{}", c.value),
        value: Some(("ext", c.value)),
        stays: true,
        ..Default::default()
    }));
    out
}

/// The query with one value toggled, and the caret on a term the panel
/// still opens on: its own term `at` where the edit left it in place, so
/// the panel does not move; else the term the value went into or the next
/// of the panel's fields; else an empty one left where the last went, so
/// the panel stays open until the field is left.
pub fn toggle(
    q: &str,
    field: &str,
    value: &str,
    exclude: bool,
    at: (usize, usize),
) -> (String, usize) {
    let (text, caret) = lens::value_toggle(q, field, value, exclude);
    let on_panel = |t: &lens::Term| matches!(t.field, Some("kind" | "ext"));
    let (a, b) = at;
    let moved = text.len() as isize - q.len() as isize;
    let kept = if text.get(..b) == q.get(..b) {
        Some(b)
    } else if q.get(a..) == text.get((a as isize + moved).max(0) as usize..) {
        Some((b as isize + moved) as usize)
    } else {
        None
    };
    let held = |c: usize| lens::term_at(&text, c).filter(|t| on_panel(t) && t.end == c);
    if let Some(t) = kept.and_then(held) {
        return (text, t.end);
    }
    if let Some(t) = lens::term_at(&text, caret).filter(on_panel) {
        return (text, t.end);
    }
    if let Some(t) = lens::terms(&text).into_iter().find(on_panel) {
        return (text.clone(), t.end);
    }
    let at = caret.min(text.len());
    let before = if at > 0 && !text[..at].ends_with(' ') {
        " "
    } else {
        ""
    };
    let after = if at < text.len() && !text[at..].starts_with(' ') {
        " "
    } else {
        ""
    };
    let empty = format!("{before}{field}:");
    let out = format!("{}{empty}{after}{}", &text[..at], &text[at..]);
    (out, at + empty.len())
}
