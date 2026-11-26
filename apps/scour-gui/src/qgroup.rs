//! The place group drawn as one Location capsule: `lens::place_caps` says
//! which parts a run of `under:` terms is, this says where each is drawn — a
//! disk as a dot and its name, a folder as crumbs, the member being edited as
//! its letters, `+N` for what does not fit — and where every byte offset of
//! the run is, so the caret, a press and the keys keep working on the text.

use scour_ui::frame;
use scour_ui::lens::{CapPart, PlaceCap, Term};

/// Faces the window measures a capsule's words in (`QMeasure.face`).
pub const HEAD: i32 = 0;
pub const VALUE: i32 = 1;
pub const PART: i32 = 2;
pub const PART_BOLD: i32 = 3;

/// Inside an inner chip, each side; a disk's dot and the room after it.
const PAD: f32 = 7.0;
const DOT: f32 = 7.0;
const DOT_GAP: f32 = 5.0;
/// Either side of a crumb's `›`.
const CRUMB_GAP: f32 = 3.0;
/// Between two inner chips, the quiet `·` in its middle.
const SEP: f32 = 13.0;
/// The member being edited: its letters in a well this much inside, at least this wide.
const WELL_PAD: f32 = 5.0;
const WELL_MIN: f32 = 36.0;
/// After the last chip, to the ring.
const END: f32 = 5.0;

/// What is drawn inside the capsule, from its left edge.
#[derive(Debug, Clone, PartialEq)]
pub struct Bit {
    pub x: f32,
    pub w: f32,
    /// 0 a chip's ground · 1 words · 2 a disk's dot · 3 the `·` · 4 a well
    pub kind: i32,
    pub text: String,
    pub bold: bool,
    /// Words: 0 ink · 1 quieter · 2 quietest.
    pub dim: i32,
    pub disk: &'static str,
}

/// Where a press over part of the capsule puts the caret, as byte offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Aim {
    /// At the end of that member: it is opened, its lens with it.
    At(usize),
    /// Among these letters, where the press is.
    Letters(usize, usize),
}

/// A capsule drawn as inner chips — a place group, or a `kind:`/`ext:`
/// list — laid out.
#[derive(Debug, Clone)]
pub struct Drawn {
    /// The bytes it stands for.
    pub start: usize,
    pub end: usize,
    /// The term whose header is the capsule's, and its field.
    pub head: Term,
    pub field: &'static str,
    /// The place group as folded, for the arrow keys; none for a list.
    pub cap: Option<PlaceCap>,
    /// Its header's width, and the whole capsule's.
    pub head_w: f32,
    pub width: f32,
    /// From the capsule's left: every char boundary of `start..=end`, in order.
    pub dx: Vec<f32>,
    pub bits: Vec<Bit>,
    pub zones: Vec<(f32, f32, Aim)>,
    /// Byte ranges drawn as letters: the value of a member being edited.
    pub letters: Vec<(usize, usize)>,
    /// The one disk its parts lie on, as the header shows it: its kind and
    /// its tile's state (`Shown::as_str`). Empty for several, or none.
    pub disk: &'static str,
    pub shown: &'static str,
}

/// How many parts the "+N" stands for, as drawn; 0 without one.
#[cfg(test)]
pub fn hidden(d: &Drawn) -> usize {
    d.bits
        .iter()
        .find_map(|b| b.text.strip_prefix('+')?.parse().ok())
        .unwrap_or(0)
}

/// Where a member's value begins, past `under:` and an opening quote.
pub fn value_of(text: &str, t: &Term) -> usize {
    frame::head_of(text, t, |_| true).map_or(t.start, |h| frame::value_start(text, h, t.end))
}

/// The run as the widest fold that fits `room`, or none when one part alone
/// would not: then it is drawn as its terms, as before.
pub fn fit(
    text: &str,
    cap: &PlaceCap,
    room: f32,
    head_w: f32,
    adv: f32,
    m: &mut dyn FnMut(&str, i32) -> f32,
) -> Option<Drawn> {
    (1..=cap.parts.len().max(1)).rev().find_map(|max| {
        let mut c = cap.clone();
        c.fold(max);
        let d = lay(text, c, head_w, adv, m);
        (d.width <= room).then_some(d)
    })
}

fn part_w(text: &str, p: &CapPart, adv: f32, m: &mut dyn FnMut(&str, i32) -> f32) -> f32 {
    if p.raw {
        let t = &p.spot.terms[0];
        let n = text[value_of(text, t)..t.end].chars().count();
        return (n as f32 * adv + 2.0 * WELL_PAD).max(WELL_MIN);
    }
    if p.spot.tile.is_some() || p.crumbs.is_empty() {
        return PAD + DOT + DOT_GAP + m(&p.spot.name, PART_BOLD) + PAD;
    }
    // Each step its own chip, a `›` between.
    let n = p.crumbs.len();
    let mut w = (n - 1) as f32 * (m("›", PART) + 2.0 * CRUMB_GAP);
    for (i, c) in p.crumbs.iter().enumerate() {
        w += 2.0 * PAD + m(&c.label, if i + 1 == n { PART_BOLD } else { PART });
    }
    w
}

fn part_bits(p: &CapPart, x: f32, w: f32, m: &mut dyn FnMut(&str, i32) -> f32) -> Vec<Bit> {
    let bit = |x: f32, w: f32, kind: i32| Bit {
        x,
        w,
        kind,
        text: String::new(),
        bold: false,
        dim: 0,
        disk: "",
    };
    if p.raw {
        return vec![bit(x, w, 4)];
    }
    let words = |at: f32, s: &str, bold: bool, dim: i32, m: &mut dyn FnMut(&str, i32) -> f32| Bit {
        text: s.to_string(),
        bold,
        dim,
        ..bit(at, m(s, if bold { PART_BOLD } else { PART }), 1)
    };
    if p.spot.tile.is_some() || p.crumbs.is_empty() {
        let mut out = vec![bit(x, w, 0)];
        out.push(Bit {
            disk: p.spot.kind.unwrap_or("fixed"),
            ..bit(x + PAD, DOT, 2)
        });
        out.push(words(x + PAD + DOT + DOT_GAP, &p.spot.name, true, 0, m));
        return out;
    }
    // A folder: each step a chip of its own, the last one bold.
    let mut out = Vec::new();
    let mut at = x;
    let n = p.crumbs.len();
    for (i, c) in p.crumbs.iter().enumerate() {
        if i > 0 {
            at += CRUMB_GAP;
            let s = words(at, "›", false, 2, m);
            at += s.w + CRUMB_GAP;
            out.push(s);
        }
        let last = i + 1 == n;
        let b = words(at + PAD, &c.label, last, 0, m);
        out.push(bit(at, b.w + 2.0 * PAD, 0));
        at += b.w + 2.0 * PAD;
        out.push(b);
    }
    out
}

/// What a stretch of the capsule stands for: a part drawn, or a block of
/// hidden parts drawn as its "+N", by its first part.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Slot {
    Part(usize),
    Block(usize),
}

/// A "+n" at `x`, quiet and without a chip of its own; how wide.
fn plus(bits: &mut Vec<Bit>, x: f32, n: usize, m: &mut dyn FnMut(&str, i32) -> f32) -> f32 {
    let label = format!("+{n}");
    let lw = m(&label, PART_BOLD);
    bits.push(Bit {
        x: x + PAD / 2.0,
        w: lw,
        kind: 1,
        text: label,
        bold: true,
        dim: 2,
        disk: "",
    });
    lw + PAD
}

/// One fold of the group laid out from `head_w`, the header's width.
pub fn lay(
    text: &str,
    cap: PlaceCap,
    head_w: f32,
    adv: f32,
    m: &mut dyn FnMut(&str, i32) -> f32,
) -> Drawn {
    let mut x = head_w;
    let mut bits = Vec::new();
    let mut slots: Vec<(Slot, f32, f32)> = Vec::new();
    let sep = |x: f32, bits: &mut Vec<Bit>| {
        bits.push(Bit {
            x: x + SEP / 2.0 - 1.5,
            w: 3.0,
            kind: 3,
            text: String::new(),
            bold: false,
            dim: 0,
            disk: "",
        })
    };
    let blocks = cap.blocks();
    for (k, p) in cap.parts.iter().enumerate() {
        let block = blocks.iter().find(|b| b.0 == k);
        if !p.shown && block.is_none() {
            continue;
        }
        if !slots.is_empty() {
            sep(x, &mut bits);
            x += SEP;
        }
        if let Some(&(first, n)) = block {
            let w = plus(&mut bits, x, n, m);
            slots.push((Slot::Block(first), x, x + w));
            x += w;
            continue;
        }
        let w = part_w(text, p, adv, m);
        bits.extend(part_bits(p, x, w, m));
        slots.push((Slot::Part(k), x, x + w));
        x += w;
    }
    let width = x + END;
    let slot_of = |k: usize| -> Option<&(Slot, f32, f32)> {
        let first = blocks
            .iter()
            .find(|b| b.0 <= k && k < b.0 + b.1)
            .map(|b| b.0);
        slots.iter().find(|s| match (s.0, first) {
            (Slot::Block(f), Some(b)) => f == b,
            (Slot::Part(p), None) => p == k,
            _ => false,
        })
    };
    let mut dx = Vec::new();
    for (i, _) in text[cap.start..cap.end]
        .char_indices()
        .chain(std::iter::once((cap.end - cap.start, ' ')))
    {
        let at = cap.start + i;
        dx.push(match cap.part_at(at).filter(|_| at > cap.start) {
            None => 0.0,
            Some(k) => {
                let p = &cap.parts[k];
                let t = &p.spot.terms[0];
                match slot_of(k) {
                    Some(s) if p.shown && p.raw && t.start < at && at <= t.end => {
                        let v = value_of(text, t);
                        s.1 + WELL_PAD + text[v..at.max(v)].chars().count() as f32 * adv
                    }
                    Some(s) => s.2,
                    None => width,
                }
            }
        });
    }
    let aim = |s: Slot| -> Aim {
        let k = match s {
            Slot::Part(k) => k,
            Slot::Block(first) => first,
        };
        let p = &cap.parts[k];
        let t = &p.spot.terms[p.spot.terms.len() - 1];
        if p.raw && p.shown {
            Aim::Letters(value_of(text, t), t.end)
        } else {
            Aim::At(t.end)
        }
    };
    let mut zones = Vec::new();
    for (i, s) in slots.iter().enumerate() {
        let x0 = if i == 0 { 0.0 } else { s.1 - SEP / 2.0 };
        let x1 = if i + 1 == slots.len() {
            width
        } else {
            s.2 + SEP / 2.0
        };
        zones.push((x0, x1, aim(s.0)));
    }
    let head = cap.parts[0].spot.terms[0].clone();
    let mut letters: Vec<(usize, usize)> = cap
        .parts
        .iter()
        .filter(|p| p.raw && p.shown)
        .map(|p| (value_of(text, &p.spot.terms[0]), p.spot.terms[0].end))
        .collect();
    letters.sort_unstable();
    Drawn {
        start: cap.start,
        end: cap.end,
        head,
        field: "under",
        cap: Some(cap),
        head_w,
        width,
        dx,
        bits,
        zones,
        letters,
        disk: "",
        shown: "",
    }
}

/// Backspace at the caret `c`: the part drawn just before it goes whole, when
/// `c` is not among a member's letters. Spots remove themselves.
pub fn backspace(text: &str, home: &str, caps: &[PlaceCap], c: usize) -> Option<(String, usize)> {
    let cap = caps.iter().find(|p| p.start < c && c <= p.end)?;
    let p = &cap.parts[cap.part_at(c)?];
    let inside = p.spot.terms.iter().any(|t| t.start < c && c <= t.end);
    (!inside && !p.raw).then(|| p.spot.remove(text, home))
}

/// Delete at the caret `c`: the part whose member starts here or next goes
/// whole, when `c` is not among a member's letters.
pub fn delete(text: &str, home: &str, caps: &[PlaceCap], c: usize) -> Option<(String, usize)> {
    let cap = caps.iter().find(|p| p.start <= c && c < p.end)?;
    let all = cap
        .parts
        .iter()
        .flat_map(|p| p.spot.terms.iter().map(move |t| (p, t)));
    if all.clone().any(|(_, t)| t.start < c && c < t.end) {
        return None;
    }
    let (p, _) = all
        .filter(|(_, t)| t.start >= c)
        .min_by_key(|(_, t)| t.start)?;
    (!p.raw).then(|| p.spot.remove(text, home))
}

/// A press of the pill over the whole group `t` (its bytes): `drop` takes
/// it out; `neg` turns "in any of them" into "in none of them" — one
/// `!under:` each — and a block of those back. None for one term alone.
pub fn whole(text: &str, t: &Term, what: &str) -> Option<(String, usize)> {
    let members = scour_ui::lens::terms(text)
        .into_iter()
        .filter(|m| t.start <= m.start && m.end <= t.end)
        .count();
    if members < 2 {
        return None;
    }
    match what {
        "drop" => {
            let (mut a, mut b) = (t.start, t.end);
            if text[b..].starts_with(' ') {
                b += 1;
            } else if text[..a].ends_with(' ') {
                a -= 1;
            }
            Some((format!("{}{}", &text[..a], &text[b..]), a))
        }
        "neg" if t.not => Some(scour_ui::lens::unnegate_places(text, t.start)),
        "neg" => Some(scour_ui::lens::negate_places(text, t.start)),
        _ => None,
    }
}

#[cfg(test)]
#[path = "qgroup_tests.rs"]
mod tests;
