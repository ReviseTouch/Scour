//! The content field drawn: each piece of the pattern a capsule — its head,
//! its alternatives as inner capsules joined by "or", a quiet "and" between
//! two of them — except the piece the caret is in, which stays letters. The
//! pattern is the only model; this says where each column of it is drawn,
//! so a press, the caret and the selection land on the text.

use scour_ui::pattern::{self, Alt, Piece, PieceKind};

/// Inside a capsule, ring to what it holds; either side of a head's word.
const PAD: f32 = 7.0;
const HEAD_PAD: f32 = 6.0;
/// Inside an inner capsule, each side; either side of a quiet word.
const CHIP_PAD: f32 = 5.0;
const QUIET_PAD: f32 = 5.0;

/// What a drawn bit is.
pub const CHIP: i32 = 0;
pub const WORD: i32 = 1;
pub const QUIET: i32 = 2;
pub const MARK: i32 = 3;

/// The letters' roles: plain, `|`/`;`/parentheses, `!`, a quote, `~`.
pub const PLAIN: i32 = 0;
pub const OR_OP: i32 = 1;
pub const NOT_OP: i32 = 2;
pub const QUOTE: i32 = 3;
pub const TILDE: i32 = 4;

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

fn role(c: char) -> i32 {
    match c {
        '!' => NOT_OP,
        '"' => QUOTE,
        '~' => TILDE,
        _ => OR_OP,
    }
}

/// One alternative as it reads in a capsule: `≈`, its quotes, its words.
fn alt_bits(
    chars: &[char],
    a: &Alt,
    mark_similar: bool,
    x: f32,
    m: Metrics,
    not: bool,
) -> Vec<Bit> {
    let mut out = Vec::new();
    let mut at = x;
    let mut put = |kind: i32, text: String, out: &mut Vec<Bit>| {
        let w = text.chars().count() as f32 * m.adv;
        out.push(Bit {
            x: at,
            w,
            kind,
            text,
            not,
        });
        at += w;
    };
    if a.similar && mark_similar {
        put(MARK, "≈".into(), &mut out);
    }
    if a.phrase {
        put(MARK, "\u{201c}".into(), &mut out);
    }
    put(WORD, chars[a.start..a.end].iter().collect(), &mut out);
    if a.phrase {
        put(MARK, "\u{201d}".into(), &mut out);
    }
    out
}

fn bits_w(b: &[Bit]) -> f32 {
    b.iter().map(|b| b.w).sum()
}

/// One piece as a capsule from `x`: its box and what is drawn in it.
fn capsule(
    chars: &[char],
    at: usize,
    p: &Piece,
    x: f32,
    m: Metrics,
    words: &dyn Fn(&str) -> String,
) -> (CapBox, Vec<Bit>) {
    let not = p.kind == PieceKind::Not;
    let head = p.kind.label().map(words).unwrap_or_default();
    let hw = if head.is_empty() {
        0.0
    } else {
        2.0 * HEAD_PAD + head.chars().count() as f32 * m.small
    };
    let mut bits = Vec::new();
    let mut alts = Vec::new();
    let mut cur = x + if hw > 0.0 { hw + PAD - 1.0 } else { PAD };
    // A lone similar word says it in its head; an alternative says it with ≈.
    let mark = p.kind != PieceKind::Similar;
    match p.alts.len() {
        0 => {
            let raw: String = chars[p.start..p.end].iter().collect();
            let w = raw.chars().count() as f32 * m.adv;
            bits.push(Bit {
                x: cur,
                w,
                kind: WORD,
                text: raw,
                not,
            });
            alts.push((cur, cur + w));
            cur += w;
        }
        1 => {
            let b = alt_bits(chars, &p.alts[0], mark, cur, m, not);
            let w = bits_w(&b);
            alts.push((cur, cur + w));
            bits.extend(b);
            cur += w;
        }
        _ => {
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
                let inner = alt_bits(chars, a, mark, cur + CHIP_PAD, m, not);
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
    }
    let w = cur + PAD - x;
    let cap = CapBox {
        piece: at,
        start: p.start,
        end: p.end,
        x,
        w,
        hw,
        head,
        not,
        alts,
    };
    (cap, bits)
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
    let editing = pattern::piece_at(&pieces, caret);
    let capsule_at = |i: usize| {
        pieces
            .iter()
            .position(|p| p.start == i)
            .filter(|&k| Some(k) != editing)
    };
    let mut out = Lay {
        xs: vec![0.0; n + 1],
        ..Lay::default()
    };
    let (mut x, mut i) = (0.0f32, 0usize);
    let mut after_cap = false;
    while i < n {
        if let Some(k) = capsule_at(i) {
            let p = &pieces[k];
            let (cap, bits) = capsule(&chars, k, p, x, m, words);
            let span = (p.end - p.start).max(1) as f32;
            for (j, col) in (p.start..=p.end).enumerate() {
                out.xs[col] = x + cap.w * j as f32 / span;
            }
            x += cap.w;
            i = p.end;
            out.caps.push(cap);
            out.bits.extend(bits);
            after_cap = true;
            continue;
        }
        // Spaces between two capsules: the quiet "and" every piece must hold.
        if after_cap && chars[i].is_whitespace() {
            let mut e = i;
            while e < n && chars[e].is_whitespace() {
                e += 1;
            }
            if e < n && capsule_at(e).is_some() {
                let and = words(pattern::AND);
                let aw = and.chars().count() as f32 * m.small;
                let w = aw + 2.0 * QUIET_PAD + 2.0;
                out.bits.push(Bit {
                    x: x + QUIET_PAD + 1.0,
                    w: aw,
                    kind: QUIET,
                    text: and,
                    not: false,
                });
                for (j, col) in (i..=e).enumerate() {
                    out.xs[col] = x + w * j as f32 / (e - i) as f32;
                }
                x += w;
                i = e;
                continue;
            }
        }
        after_cap = false;
        out.xs[i] = x;
        let c = chars[i];
        if !c.is_whitespace() {
            let r = editing
                .map(|k| &pieces[k])
                .filter(|p| p.ops.contains(&i))
                .map_or(PLAIN, |_| role(c));
            match out.runs.last_mut() {
                Some(last)
                    if last.role == r
                        && (last.x + last.text.chars().count() as f32 * m.adv - x).abs() < 0.01 =>
                {
                    last.text.push(c);
                }
                _ => out.runs.push(Run {
                    text: c.to_string(),
                    x,
                    role: r,
                }),
            }
        }
        x += m.adv;
        i += 1;
    }
    out.xs[n] = x;
    out
}

/// The column a press at `x` puts the caret at: a capsule's end, or the
/// letter edge nearest it.
pub fn hit(l: &Lay, x: f32) -> usize {
    if let Some(c) = l.caps.iter().find(|c| x >= c.x && x <= c.x + c.w) {
        return c.end;
    }
    let inside = |i: usize| l.caps.iter().any(|c| c.start < i && i < c.end);
    (0..l.xs.len())
        .filter(|&i| !inside(i))
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
