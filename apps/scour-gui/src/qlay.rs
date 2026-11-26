//! The query bar laid out: each capsule dressed (`qdress`), each place group
//! folded into one Location capsule (`qgroup`), the lines wrapped by what is
//! drawn, every column measured. `qwire::draw` hands the result to the window.

use scour_ui::lens;

use crate::qbar::{Laid, Zone, chars_before};
use crate::qdress::{Dress, dress, dressed, head, width_of};
use crate::qgroup::{self, Aim, Drawn};
use crate::qwire::{QueryBar, home};
use crate::ui::QFrame;
use crate::{Cap, MainWindow, QBit, Span};

/// Take the letters of one stretch of a line out of the runs, or draw them
/// small: a header's, or a value said in words instead.
fn carve(runs: Vec<Span>, line: i32, from: i32, to: i32, small: Option<bool>) -> Vec<Span> {
    runs.into_iter()
        .flat_map(|r| {
            let len = r.text.chars().count() as i32;
            if r.line != line || r.at + len <= from || r.at >= to {
                return vec![r];
            }
            let chars: Vec<char> = r.text.chars().collect();
            let piece = |a: i32, b: i32, small: bool| Span {
                text: chars[(a - r.at) as usize..(b - r.at) as usize]
                    .iter()
                    .collect::<String>()
                    .into(),
                at: a,
                small,
                ..r.clone()
            };
            let mut kept = Vec::new();
            if r.at < from {
                kept.push(piece(r.at, from, r.small));
            }
            if let Some(small) = small {
                kept.push(piece(r.at.max(from), (r.at + len).min(to), small));
            }
            if r.at + len > to {
                kept.push(piece(to, r.at + len, r.small));
            }
            kept
        })
        .collect()
}

/// Everything the window draws of the bar, laid out.
pub struct Laidout {
    pub caps: Vec<Cap>,
    pub runs: Vec<Span>,
    pub bits: Vec<QBit>,
    pub frames: Vec<QFrame>,
}

/// The query's place groups and `kind:`/`ext:` lists, each folded to fit a
/// line, or left out when one part alone would not.
fn fold_groups(qb: &mut QueryBar, text: &str, d: &Dress) -> Vec<Drawn> {
    let room = if qb.field.room > 0.0 {
        qb.field.room
    } else {
        80.0 * d.adv
    };
    let tiles = crate::qplace::tiles(&qb.disks, &qb.sources);
    let caret = d.caret.map(|at| lens::Caret {
        at,
        editing: qb.lenses.editing,
    });
    let hm = home();
    let mut caps = lens::place_caps_at(text, &hm, &tiles, caret);
    caps.extend(lens::exclusion_caps(text, &hm, &tiles, caret));
    let mut out: Vec<Drawn> = caps
        .iter()
        .filter(|p| p.parts.len() > 1 || p.parts.iter().any(|p| !p.raw))
        .filter_map(|p| {
            // An exclusion's header holds its `!` too.
            let not = text[p.start..].starts_with('!');
            let (mut head_w, _) = head(qb, d, "under", 6 + usize::from(not), not);
            // On one disk, the header is that disk's picture and its state's dot.
            let disk = p.disk().filter(|_| !not && !d.words).map(|k| &tiles[k]);
            if disk.is_some() {
                head_w += crate::qbar::HEAD_DOT;
            }
            let mut m = |s: &str, face: i32| width_of(qb, s, face);
            let mut g = qgroup::fit(text, p, room, head_w, d.adv, &mut m)?;
            if let Some(t) = disk {
                (g.disk, g.shown) = (t.kind, t.shown.as_str());
            }
            Some(g)
        })
        .collect();
    for t in crate::qlist::lists(text, d.caret) {
        if out.iter().any(|g| g.start <= t.start && t.end <= g.end) {
            continue;
        }
        let chars = scour_ui::frame::head_of(text, &t, |_| true)
            .map_or(0, |e| chars_before(text, e) - chars_before(text, t.start));
        let (hw, _) = head(qb, d, t.field.unwrap_or("kind"), chars, t.not);
        let labels = crate::qlist::labels(text, &t, d.cat);
        let mut m = |s: &str, face: i32| width_of(qb, s, face);
        out.extend(crate::qlist::fit(text, &t, &labels, room, hw, &mut m));
    }
    out
}

/// A group's chars in the query.
fn span_of(text: &str, g: &Drawn) -> (usize, usize) {
    (chars_before(text, g.start), chars_before(text, g.end))
}

fn laid(text: &str, line: usize, at: usize, g: &Drawn) -> Laid {
    let (a, b) = span_of(text, g);
    Laid {
        line,
        at,
        len: b - a,
        width: g.width,
        dx: g.dx.clone(),
    }
}

/// The lines, cut by what is drawn: measured on one line, then wrapped, a
/// group never broken.
fn wrap(qb: &mut QueryBar, d: &Dress, text: &str, spans: &[scour_core::Span], groups: &[Drawn]) {
    let flat = dressed(qb, d, text, spans, groups);
    let one: Vec<Laid> = groups
        .iter()
        .map(|g| laid(text, 0, span_of(text, g).0, g))
        .collect();
    let widths = qb.field.widths(0, &flat, &one);
    let keep: Vec<(usize, usize)> = groups.iter().map(|g| span_of(text, g)).collect();
    qb.field.relayout(text, &widths, &keep);
}

/// A group's capsule on its line, its header `h` chars, their letters
/// `small` apart where the settings ask for words.
fn group_cap(qb: &mut QueryBar, d: &Dress, text: &str, g: &Drawn, small: f32) -> (Cap, Laid) {
    let (a, b) = span_of(text, g);
    let (line, at) = scour_ui::wrap::place(&qb.field.lines, a);
    let mut l = laid(text, line, at, g);
    let h =
        scour_ui::frame::head_of(text, &g.head, |_| true).map_or(0, |e| chars_before(text, e) - a);
    for k in 1..h.min(l.len) {
        l.dx[k] = k as f32 * small;
    }
    let mut c = Cap {
        line: line as i32,
        at: at as i32,
        len: (b - a) as i32,
        head: h as i32,
        field: g.field.into(),
        tone: if g.head.not { 2 } else { 0 },
        from: g.start as i32,
        to: g.end as i32,
        whole: true,
        first: true,
        last: true,
        group: true,
        disk: g.disk.into(),
        shown: g.shown.into(),
        ..Cap::default()
    };
    let unwritten = Dress {
        cur: usize::MAX,
        caret: None,
        ..*d
    };
    dress(qb, &unwritten, &mut c);
    c.tail = 0.0;
    (c, l)
}

/// The coloured runs, less the letters drawn as something else: a header's
/// small or under its picture, a value said in words, a group's parts.
fn carved(
    qb: &QueryBar,
    text: &str,
    spans: &[scour_core::Span],
    caps: &[Cap],
    groups: &[Drawn],
) -> Vec<Span> {
    let mut runs = qb.field.place(crate::painted(text, spans));
    for c in caps.iter().filter(|c| c.head > 0) {
        if let Some(g) = groups.iter().find(|g| g.start == c.from as usize) {
            runs = carve_group(runs, text, g, c);
            continue;
        }
        runs = carve(runs, c.line, c.at, c.at + c.head, (!c.icon).then_some(true));
        if !c.nice.is_empty() {
            runs = carve(runs, c.line, c.at + c.head, c.at + c.len, None);
        }
    }
    for r in &mut runs {
        r.x = qb.field.x(r.line as usize, r.at);
    }
    runs
}

/// A group's parts as the window draws them, and the zones a press is read by.
fn bits(qb: &mut QueryBar, text: &str, groups: &[Drawn], laid: &[Laid]) -> Vec<QBit> {
    let mut bits = Vec::new();
    let mut zones = Vec::new();
    let ch = |b: usize| chars_before(text, b);
    for (g, l) in groups.iter().zip(laid) {
        let left = qb.field.x(l.line, l.at as i32);
        bits.extend(g.bits.iter().map(|b| QBit {
            line: l.line as i32,
            x: left + b.x,
            w: b.w,
            kind: b.kind,
            text: b.text.as_str().into(),
            bold: b.bold,
            dim: b.dim,
            disk: b.disk.into(),
            struck: (b.kind == 0 || b.kind == 1) && g.head.not,
        }));
        zones.extend(g.zones.iter().map(|&(x0, x1, aim)| Zone {
            line: l.line,
            x0: left + x0,
            x1: left + x1,
            aim: match aim {
                Aim::At(b) => Aim::At(ch(b)),
                Aim::Letters(a, b) => Aim::Letters(ch(a), ch(b)),
            },
        }));
    }
    qb.field.zones = zones;
    bits
}

/// Lay the query out: the field's lines, columns and zones, and what is drawn.
pub fn lay_out(
    w: &MainWindow,
    qb: &mut QueryBar,
    cat: &scour_i18n::Catalogue,
    text: &str,
    spans: &[scour_core::Span],
) -> Laidout {
    qb.field.advance = w.get_q_advance();
    let d = Dress {
        cat,
        words: w.get_field_words(),
        adv: match w.get_q_advance() {
            a if a > 0.0 => a,
            _ => 9.6,
        },
        cur: chars_before(text, qb.cursor),
        caret: (qb.anchor == qb.cursor && w.get_q_focused()).then_some(qb.cursor),
    };
    qb.field.unwrapped(text);
    let (_, small) = head(qb, &d, "under", 6, false);
    let groups = fold_groups(qb, text, &d);
    wrap(qb, &d, text, spans, &groups);
    let mut caps = dressed(qb, &d, text, spans, &groups);
    let mut all = Vec::new();
    for g in &groups {
        let (c, l) = group_cap(qb, &d, text, g, small);
        caps.push(c);
        all.push(l);
    }
    qb.field.measure(&caps, &all);
    let runs = carved(qb, text, spans, &caps, &groups);
    let field = &qb.field;
    for c in &mut caps {
        let l = c.line as usize;
        let group = all
            .iter()
            .zip(&groups)
            .find(|(g, _)| g.line == l && g.at == c.at as usize);
        c.x = field.x(l, c.at);
        c.w = field.x(l, c.at + c.len) - c.x;
        c.hw = match group {
            Some((_, g)) => g.head_w,
            None => field.x(l, c.at + c.head) - c.x,
        };
        // An empty filter's room is past its header, not part of it.
        if c.len == c.head {
            c.hw -= c.tail;
        }
        c.rescues = c.last
            && qb.rescue_for == text
            && crate::qeffect::rescuer(text, c.from as usize, c.to as usize, &qb.rescue).is_some();
    }
    let frames = frames(qb, text, &groups);
    let bits = bits(qb, text, &groups, &all);
    qb.caps = groups.iter().filter_map(|g| g.cap.clone()).collect();
    qb.groups = all
        .into_iter()
        .zip(&groups)
        .map(|(l, g)| (g.start, g.end, l))
        .collect();
    Laidout {
        caps,
        runs,
        bits,
        frames,
    }
}

/// A frame a line round each run joined by `|` that no capsule holds whole:
/// the pieces the lines cut it into, each from its first column to its last.
fn frames(qb: &QueryBar, text: &str, groups: &[Drawn]) -> Vec<QFrame> {
    let drawn: Vec<(usize, usize)> = groups.iter().map(|g| (g.start, g.end)).collect();
    let mut out = Vec::new();
    for (a, b) in crate::qeffect::or_runs(text, &drawn) {
        let (ca, cb) = (chars_before(text, a), chars_before(text, b));
        for (i, l) in qb.field.lines.iter().enumerate() {
            let (s, e) = (ca.max(l.start), cb.min(l.end));
            if s >= e {
                continue;
            }
            let x = qb.field.x(i, (s - l.start) as i32);
            let w = qb.field.x(i, (e - l.start) as i32) - x;
            out.push(QFrame {
                line: i as i32,
                x,
                w,
            });
        }
    }
    out
}

/// A group's letters taken out of the runs, but for the member being edited;
/// its header drawn small when it is not a picture.
fn carve_group(runs: Vec<Span>, text: &str, g: &Drawn, c: &Cap) -> Vec<Span> {
    let a = chars_before(text, g.start);
    let col = |b: usize| (chars_before(text, b) - a) as i32 + c.at;
    let (line, at, h) = (c.line, c.at, c.head);
    let mut runs = carve(runs, line, at, at + h, (!c.icon).then_some(true));
    let mut from = at + h;
    for &(v, e) in &g.letters {
        runs = carve(runs, line, from, col(v).max(from), None);
        from = from.max(col(e));
    }
    carve(runs, line, from, col(g.end), None)
}

/// Where the caret stands when it is in a place group: the group says.
pub fn caret_x(qb: &QueryBar, text: &str, cursor: usize) -> Option<f32> {
    let (a, _, l) = qb
        .groups
        .iter()
        .find(|(a, b, _)| *a < cursor && cursor <= *b)?;
    let k = chars_before(text, cursor) - chars_before(text, *a);
    Some(qb.field.x(l.line, l.at as i32) + l.dx.get(k).copied()?)
}
