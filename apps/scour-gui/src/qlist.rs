//! A `kind:` or `ext:` list drawn as one capsule of inner chips, as the
//! place group is: each value its own chip, a quiet `·` between, `+N` for
//! what does not fit. With the caret in the term it is drawn as typed.

use scour_core::Catalog;
use scour_i18n::Catalogue;
use scour_ui::lens::{self, Term};

use crate::qgroup::{Aim, Bit, Drawn, PART_BOLD, value_of};

/// Inside a chip, each side; between two, the `·` in its middle; after
/// the last, to the ring.
const PAD: f32 = 7.0;
const SEP: f32 = 13.0;
const END: f32 = 5.0;

/// The list terms drawn as chips: two values or more, the caret not in them.
pub fn lists(text: &str, caret: Option<usize>) -> Vec<Term> {
    lens::terms(text)
        .into_iter()
        .filter(|t| matches!(t.field, Some("kind" | "ext")))
        .filter(|t| !caret.is_some_and(|c| c > t.start && c <= t.end))
        .filter(|t| values(text, t).len() > 1)
        .collect()
}

/// Where each value of a list term is in the text, in order.
pub fn values(text: &str, t: &Term) -> Vec<(usize, usize)> {
    let v = value_of(text, t);
    let mut out = Vec::new();
    let mut from = v;
    let push = |a: usize, b: usize, out: &mut Vec<(usize, usize)>| {
        let s = &text[a..b];
        let lead = s.len() - s.trim_start().len();
        let trail = s.len() - s.trim_end().len();
        if b - a > lead + trail {
            out.push((a + lead, b - trail));
        }
    };
    for (i, c) in text[v..t.end].char_indices() {
        if c == ';' {
            push(from, v + i, &mut out);
            from = v + i + 1;
        }
    }
    push(from, t.end, &mut out);
    out
}

/// Each value as its chip says it: a kind by its name, an extension with
/// its dot.
pub fn labels(text: &str, t: &Term, cat: &Catalogue) -> Vec<String> {
    let field = t.field.unwrap_or("kind");
    values(text, t)
        .into_iter()
        .map(|(a, b)| {
            let v = &text[a..b];
            let key = lens::value_key(field, v);
            match field {
                "ext" => format!(".{key}"),
                _ => lens::kind_label(&key)
                    .map_or_else(|| v.to_string(), |m| cat.get(m).into_owned()),
            }
        })
        .collect()
}

/// The widest fold that fits `room`; none when one chip alone would not.
pub fn fit(
    text: &str,
    t: &Term,
    labels: &[String],
    room: f32,
    head_w: f32,
    m: &mut dyn FnMut(&str, i32) -> f32,
) -> Option<Drawn> {
    (1..=labels.len()).rev().find_map(|max| {
        let d = lay(text, t, labels, max, head_w, m);
        (d.width <= room).then_some(d)
    })
}

fn bit(x: f32, w: f32, kind: i32) -> Bit {
    Bit {
        x,
        w,
        kind,
        text: String::new(),
        bold: false,
        dim: 0,
        disk: "",
    }
}

/// A chip of words at `x`: its ground and its words; how wide.
fn chip(
    bits: &mut Vec<Bit>,
    x: f32,
    label: &str,
    dim: i32,
    m: &mut dyn FnMut(&str, i32) -> f32,
) -> f32 {
    let lw = m(label, PART_BOLD);
    bits.push(bit(x, lw + 2.0 * PAD, 0));
    bits.push(Bit {
        text: label.to_string(),
        bold: true,
        dim,
        ..bit(x + PAD, lw, 1)
    });
    lw + 2.0 * PAD
}

/// The list with its first `max` values shown, from `head_w`.
pub fn lay(
    text: &str,
    t: &Term,
    labels: &[String],
    max: usize,
    head_w: f32,
    m: &mut dyn FnMut(&str, i32) -> f32,
) -> Drawn {
    let vals = values(text, t);
    let n = vals.len().min(labels.len());
    let shown = n.min(max.max(1));
    let mut bits = Vec::new();
    let mut slots: Vec<(f32, f32)> = Vec::new();
    let mut x = head_w;
    for (k, label) in labels.iter().take(shown).enumerate() {
        if k > 0 {
            bits.push(bit(x + SEP / 2.0 - 1.5, 3.0, 3));
            x += SEP;
        }
        let w = chip(&mut bits, x, label, 0, m);
        slots.push((x, x + w));
        x += w;
    }
    if n > shown {
        bits.push(bit(x + SEP / 2.0 - 1.5, 3.0, 3));
        x += SEP;
        let w = chip(&mut bits, x, &format!("+{}", n - shown), 1, m);
        slots.push((x, x + w));
        x += w;
    }
    let width = x + END;
    let first = vals.first().map_or(t.end, |v| v.0);
    let dx = text[t.start..t.end]
        .char_indices()
        .map(|(i, _)| t.start + i)
        .chain(std::iter::once(t.end))
        .map(|at| {
            if at <= first {
                return 0.0;
            }
            let k = vals.iter().rposition(|v| v.0 < at).unwrap_or(0);
            slots.get(k.min(shown)).map_or(width, |s| s.1)
        })
        .collect();
    let zones = slots
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let x0 = if i == 0 { 0.0 } else { s.0 - SEP / 2.0 };
            let x1 = if i + 1 == slots.len() {
                width
            } else {
                s.1 + SEP / 2.0
            };
            (x0, x1, Aim::At(t.end))
        })
        .collect();
    Drawn {
        start: t.start,
        end: t.end,
        head: t.clone(),
        field: t.field.unwrap_or("kind"),
        cap: None,
        head_w,
        width,
        dx,
        bits,
        zones,
        letters: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qgroup::hidden;

    fn t(q: &str) -> Term {
        lens::terms(q).remove(0)
    }

    #[test]
    fn a_list_s_values_are_where_they_are_written_spaces_and_all() {
        let q = "ext:pdf ; odt;;md";
        let at: Vec<&str> = values(q, &t(q)).iter().map(|&(a, b)| &q[a..b]).collect();
        assert_eq!(at, ["pdf", "odt", "md"]);
        assert_eq!(lists("rapor kind:doc;code ext:pdf", None).len(), 1);
        assert!(lists("kind:doc;code", Some(13)).is_empty(), "being written");
    }

    #[test]
    fn what_does_not_fit_folds_into_a_plus_and_every_letter_has_a_place() {
        let q = "kind:doc;code;image";
        let term = t(q);
        let labels: Vec<String> = ["Document", "Code", "Image"].map(String::from).to_vec();
        let mut m = |s: &str, _: i32| s.chars().count() as f32 * 8.0;
        let all = lay(q, &term, &labels, 3, 40.0, &mut m);
        assert_eq!((hidden(&all), all.dx.len()), (0, q.len() + 1));
        let narrow = fit(q, &term, &labels, all.width - 1.0, 40.0, &mut m).unwrap();
        assert_eq!(hidden(&narrow), 1);
        // The header's letters are at its start; the last letter at the end.
        assert_eq!(all.dx[3], 0.0);
        assert_eq!(*all.dx.last().unwrap(), all.width - END);
    }
}
