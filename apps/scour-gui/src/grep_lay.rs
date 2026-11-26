//! The content field drawn: every piece of the pattern a capsule — its head,
//! its alternatives as inner capsules joined by "or", a quiet "and" between
//! two of them — the one being written too, so nothing moves when the caret
//! comes or goes. An operator is not drawn as itself: `!` is the "without"
//! head, `~` the "similar" head or a ≈, quotes are “ ”, `;` is the "or". Each
//! still has its column, and the pattern is the only model: this says where
//! each column is drawn, so a press, the caret and the selection land on the
//! text.

use scour_ui::pattern::{self, Alt, Piece, PieceKind};

/// Inside a capsule, ring to what it holds; either side of a head's word.
const PAD: f32 = 7.0;
const HEAD_PAD: f32 = 6.0;
/// Inside an inner capsule, each side; either side of a quiet word.
const CHIP_PAD: f32 = 5.0;
const QUIET_PAD: f32 = 5.0;
/// An alternative not written yet, in characters: room for the caret.
const HOLE: f32 = 1.5;

/// What a drawn bit is.
pub const CHIP: i32 = 0;
pub const WORD: i32 = 1;
pub const QUIET: i32 = 2;
pub const MARK: i32 = 3;
/// An empty alternative on its own: where the word will be written.
pub const SLOT: i32 = 4;

/// The letters' roles outside capsules: a regex or hex is all plain.
pub const PLAIN: i32 = 0;

/// One character of the field's face, and one of its small face.
#[derive(Debug, Clone, Copy)]
pub struct Metrics {
    pub adv: f32,
    pub small: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Run {
    pub text: String,
    pub x: f32,
    pub role: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CapBox {
    /// Which piece of `pattern::pieces` it is, and its columns.
    pub piece: usize,
    pub start: usize,
    pub end: usize,
    pub x: f32,
    pub w: f32,
    /// How wide its head is, nought for a plain word, and its words.
    pub hw: f32,
    pub head: String,
    pub not: bool,
    /// The caret is in it.
    pub live: bool,
    /// Where each alternative is drawn, for the pill to act on the one
    /// under the pointer.
    pub alts: Vec<(f32, f32)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Bit {
    pub x: f32,
    pub w: f32,
    pub kind: i32,
    pub text: String,
    pub not: bool,
}

#[derive(Debug, Default, Clone)]
pub struct Lay {
    pub runs: Vec<Run>,
    pub caps: Vec<CapBox>,
    pub bits: Vec<Bit>,
    /// Where the caret before each column is drawn, one more than the
    /// characters.
    pub xs: Vec<f32>,
}

/// Where a column is drawn, the first said winning: a letter's or a mark's
/// before a capsule's edge.
fn stop(stops: &mut Vec<(usize, f32)>, col: usize, x: f32) {
    if !stops.iter().any(|&(c, _)| c == col) {
        stops.push((col, x));
    }
}

/// One alternative as it reads in a capsule — `≈`, its quotes, its words or
/// the room for them — and where each of its columns is drawn.
#[allow(clippy::too_many_arguments)]
fn alt_bits(
    chars: &[char],
    a: &Alt,
    mark_similar: bool,
    chip: bool,
    x: f32,
    m: Metrics,
    not: bool,
    stops: &mut Vec<(usize, f32)>,
) -> Vec<Bit> {
    let mut out = Vec::new();
    let mut at = x;
    let mut put = |kind: i32, text: String, w: f32, at: &mut f32| {
        out.push(Bit {
            x: *at,
            w,
            kind,
            text,
            not,
        });
        *at += w;
    };
    let quote = usize::from(a.phrase);
    if a.similar && mark_similar {
        stop(stops, a.start - 1 - quote, at);
        put(MARK, "≈".into(), m.adv, &mut at);
    }
    if a.phrase {
        stop(stops, a.start - 1, at);
        put(MARK, "\u{201c}".into(), m.adv, &mut at);
    }
    if a.is_hole() {
        stop(stops, a.start, at + 2.0);
        // In an inner capsule its ground is the room; alone, a slot of its own.
        let kind = if chip { QUIET } else { SLOT };
        put(kind, String::new(), HOLE * m.adv, &mut at);
    } else {
        for j in 0..a.end - a.start {
            stop(stops, a.start + j, at + j as f32 * m.adv);
        }
        let word: String = chars[a.start..a.end].iter().collect();
        let w = word.chars().count() as f32 * m.adv;
        put(WORD, word, w, &mut at);
        stop(stops, a.end, at);
    }
    if a.phrase && chars.get(a.end) == Some(&'"') {
        stop(stops, a.end, at);
        put(MARK, "\u{201d}".into(), m.adv, &mut at);
        stop(stops, a.end + 1, at);
    }
    out
}

fn bits_w(b: &[Bit]) -> f32 {
    b.iter().map(|b| b.w).sum()
}

/// One piece as a capsule from `x`: its box, what is drawn in it, and where
/// each of its columns is.
fn capsule(
    chars: &[char],
    at: usize,
    p: &Piece,
    x: f32,
    m: Metrics,
    words: &dyn Fn(&str) -> String,
) -> (CapBox, Vec<Bit>, Vec<f32>) {
    let not = p.kind == PieceKind::Not;
    let head = p.kind.label().map(words).unwrap_or_default();
    let hw = if head.is_empty() {
        0.0
    } else {
        2.0 * HEAD_PAD + head.chars().count() as f32 * m.small
    };
    let mut bits = Vec::new();
    let mut alts = Vec::new();
    let mut stops = Vec::new();
    let mut cur = x + if hw > 0.0 { hw + PAD - 1.0 } else { PAD };
    // A lone similar word says it in its head; an alternative says it with ≈.
    let mark = p.kind != PieceKind::Similar;
    if p.alts.len() <= 1 {
        let b = match p.alts.first() {
            Some(a) => alt_bits(chars, a, mark, false, cur, m, not, &mut stops),
            None => Vec::new(),
        };
        let w = bits_w(&b);
        alts.push((cur, cur + w));
        bits.extend(b);
        cur += w;
    } else {
        let or = words(pattern::OR);
        for (i, a) in p.alts.iter().enumerate() {
            if i > 0 {
                let w = or.chars().count() as f32 * m.small;
                bits.push(Bit {
                    x: cur + QUIET_PAD,
                    w,
                    kind: QUIET,
                    text: or.clone(),
                    not,
                });
                cur += w + 2.0 * QUIET_PAD;
            }
            let inner = alt_bits(chars, a, mark, true, cur + CHIP_PAD, m, not, &mut stops);
            let w = bits_w(&inner) + 2.0 * CHIP_PAD;
            bits.push(Bit {
                x: cur,
                w,
                kind: CHIP,
                text: String::new(),
                not,
            });
            bits.extend(inner);
            alts.push((cur, cur + w));
            cur += w;
        }
    }
    let w = cur + PAD - x;
    // `!`, `(`, a lone `~`: the head's start; `)`: the capsule's end. The
    // columns between two drawn ones share the room between them.
    stop(&mut stops, p.start, x + 2.0);
    stop(&mut stops, p.end, x + w - 2.0);
    stops.sort_by_key(|&(c, _)| c);
    let mut xs = vec![0.0; p.end - p.start + 1];
    for pair in stops.windows(2) {
        let ((c0, x0), (c1, x1)) = (pair[0], pair[1]);
        for c in c0..=c1 {
            let t = if c1 > c0 {
                (c - c0) as f32 / (c1 - c0) as f32
            } else {
                0.0
            };
            xs[c - p.start] = x0 + (x1 - x0) * t;
        }
    }
    // Never back: a column is never drawn left of the one before it.
    for i in 1..xs.len() {
        xs[i] = xs[i].max(xs[i - 1]);
    }
    let cap = CapBox {
        piece: at,
        start: p.start,
        end: p.end,
        x,
        w,
        hw,
        head,
        not,
        live: false,
        alts,
    };
    (cap, bits, xs)
}

/// `text` laid out with the caret at char column `caret`. `content` false
/// (a regex, hex) draws every character as a letter.
pub fn lay(
    text: &str,
    caret: usize,
    content: bool,
    m: Metrics,
    words: &dyn Fn(&str) -> String,
) -> Lay {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let pieces = if content {
        pattern::pieces(text)
    } else {
        Vec::new()
    };
    let live = pattern::piece_at(&pieces, caret);
    let mut out = Lay {
        xs: vec![0.0; n + 1],
        ..Lay::default()
    };
    let mut x = 0.0f32;
    let mut i = 0usize;
    for (k, p) in pieces.iter().enumerate() {
        if i < p.start {
            x = between(
                &mut out,
                &chars,
                i,
                p.start,
                k > 0,
                true,
                caret,
                x,
                m,
                words,
            );
        }
        let (mut cap, bits, xs) = capsule(&chars, k, p, x, m, words);
        cap.live = live == Some(k);
        for (j, cx) in xs.into_iter().enumerate() {
            out.xs[p.start + j] = cx;
        }
        x += cap.w;
        i = p.end;
        out.caps.push(cap);
        out.bits.extend(bits);
    }
    if i < n {
        between(
            &mut out,
            &chars,
            i,
            n,
            !pieces.is_empty(),
            false,
            caret,
            x,
            m,
            words,
        );
    }
    out
}

/// What stands between two capsules, or before or after them: spaces after
/// a capsule are the quiet "and" — written when a capsule follows or the
/// caret is there to start one, its room kept either way — and anything else
/// is letters.
#[allow(clippy::too_many_arguments)]
fn between(
    out: &mut Lay,
    chars: &[char],
    from: usize,
    to: usize,
    after_cap: bool,
    before_cap: bool,
    caret: usize,
    x: f32,
    m: Metrics,
    words: &dyn Fn(&str) -> String,
) -> f32 {
    if after_cap && chars[from..to].iter().all(|c| c.is_whitespace()) {
        let and = words(pattern::AND);
        let aw = and.chars().count() as f32 * m.small;
        let w = aw + 2.0 * QUIET_PAD + 2.0;
        if before_cap || (caret > from && caret <= to) {
            out.bits.push(Bit {
                x: x + QUIET_PAD + 1.0,
                w: aw,
                kind: QUIET,
                text: and,
                not: false,
            });
        }
        // Every column in it after the first is where a new piece would
        // start: past the word.
        for col in &mut out.xs[from + 1..=to] {
            *col = x + w;
        }
        return x + w;
    }
    let mut x = x;
    for (i, &c) in chars.iter().enumerate().take(to).skip(from) {
        out.xs[i] = x;
        if !c.is_whitespace() {
            match out.runs.last_mut() {
                Some(last)
                    if (last.x + last.text.chars().count() as f32 * m.adv - x).abs() < 0.01 =>
                {
                    last.text.push(c);
                }
                _ => out.runs.push(Run {
                    text: c.to_string(),
                    x,
                    role: PLAIN,
                }),
            }
        }
        x += m.adv;
    }
    out.xs[to] = x;
    x
}

/// The column a press at `x` puts the caret at: the nearest drawn one, in a
/// capsule as anywhere — the capsule stays one.
pub fn hit(l: &Lay, x: f32) -> usize {
    (0..l.xs.len())
        .min_by(|&a, &b| (l.xs[a] - x).abs().total_cmp(&(l.xs[b] - x).abs()))
        .unwrap_or(0)
}

/// The capsule under `x`, as an index into `caps`, and its alternative there.
pub fn over(l: &Lay, x: f32) -> Option<(usize, usize)> {
    let k = l.caps.iter().position(|c| x >= c.x && x <= c.x + c.w)?;
    let alt = l.caps[k]
        .alts
        .iter()
        .position(|&(a, b)| x >= a && x <= b)
        .unwrap_or(0);
    Some((k, alt))
}

#[cfg(test)]
#[path = "grep_lay_tests.rs"]
mod tests;
