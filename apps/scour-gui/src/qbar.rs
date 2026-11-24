//! The query field as this window draws it. Slint's `TextInput` can neither
//! colour a range nor wrap where the page wraps, so it stays one invisible
//! line that holds the text, the keys, the clipboard and the input method, and
//! this says where everything is drawn: the lines, the coloured runs, the
//! capsules, the caret and the selection, and which term a point is over.

use scour_ui::{frame, lens, wrap};

use crate::{Cap, Sel, Span};

/// Lines shown before the field scrolls inside itself.
pub const VISIBLE: usize = 3;

/// A filter's `field:` header is drawn small and close, so a capsule spends its
/// width on what it holds: the header's face over the query's, 12.5 over 16px.
pub const HEAD_SCALE: f32 = 12.5 / 16.0;
/// And this much closer, letter to letter: the `letter-spacing` it is drawn with.
pub const HEAD_TIGHT: f32 = 0.5;
/// Room between a header and what it holds, past the header's last letter:
/// drawn small and close, `kind:` ran into its `doc`.
pub const HEAD_GAP: f32 = 4.0;
/// Room inside a capsule after what it holds, so the last letter does not
/// touch its ring.
pub const CAP_TAIL: f32 = 3.0;
/// And after what a filter holds, whose capsule has a header: the value sits
/// in a room of its own, as far from the ring as from the header.
pub const VALUE_TAIL: f32 = 8.0;
/// A filter being written is at least this wide past its header: a place to
/// type, before there is anything in it.
pub const WELL_MIN: f32 = 88.0;
/// A header drawn as a picture and a name: the ring to the picture, the
/// picture, the picture to the name, the name to the header's edge, and the
/// edge to what the filter holds.
pub const HEAD_LEAD: f32 = 3.0;
/// Room every capsule keeps at its right end for its `×`: always there, so
/// nothing moves when the pointer comes or goes.
pub const CLOSE_W: f32 = 22.0;
pub const HEAD_PIC: f32 = 16.0;
pub const HEAD_JOIN: f32 = 5.0;
pub const HEAD_TRAIL: f32 = 8.0;
pub const HEAD_ROOM: f32 = 9.0;
/// A header drawn as a picture is this wide, whatever its letters were; an
/// excluded one this much wider, for the `!` in front of it.
pub const ICON_W: f32 = 27.0;
pub const NOT_W: f32 = 11.0;

/// The fields a header can be drawn as a picture for.
pub fn has_icon(field: &str) -> bool {
    matches!(
        field,
        "fuzzy" | "under" | "parent" | "dm" | "kind" | "ext" | "size" | "content"
    )
}

#[derive(Default)]
pub struct Field {
    /// Characters a line holds; zero until the window has measured.
    pub cols: usize,
    /// The first line shown, once there are more than [`VISIBLE`].
    pub first: usize,
    pub lines: Vec<wrap::Line>,
    /// One character of the query's face, as the window measured it.
    pub advance: f32,
    /// Where every column of every line is drawn, from the field's padding:
    /// one more than the line's characters. Headers are narrower than letters,
    /// so a column is not `advance` times itself.
    pub xs: Vec<Vec<f32>>,
}

/// A byte offset into `s` as a character offset.
pub fn chars_before(s: &str, byte: usize) -> usize {
    let mut b = byte.min(s.len());
    while !s.is_char_boundary(b) {
        b -= 1;
    }
    s[..b].chars().count()
}

/// A character offset into `s` as a byte offset.
pub fn byte_of(s: &str, ch: usize) -> usize {
    s.char_indices().nth(ch).map_or(s.len(), |(b, _)| b)
}

impl Field {
    pub fn relayout(&mut self, query: &str) {
        self.lines = wrap::lines(query, self.cols.max(8));
        self.first = self.first.min(self.lines.len().saturating_sub(VISIBLE));
    }

    /// How many lines the field is tall.
    pub fn shown(&self) -> usize {
        self.lines.len().clamp(1, VISIBLE)
    }

    /// The runs, cut where the lines break, each placed by line and column.
    pub fn place(&self, runs: Vec<Span>) -> Vec<Span> {
        let mut out = Vec::with_capacity(runs.len() + 2);
        for r in runs {
            let text: Vec<char> = r.text.chars().collect();
            let mut at = r.at.max(0) as usize;
            let mut i = 0;
            while i < text.len() {
                let (line, col) = wrap::place(&self.lines, at);
                let room = self.lines[line].end.saturating_sub(at).max(1);
                let take = room.min(text.len() - i);
                out.push(Span {
                    text: text[i..i + take].iter().collect::<String>().into(),
                    role: r.role,
                    not: r.not,
                    at: col as i32,
                    line: line as i32,
                    ..Span::default()
                });
                i += take;
                at += take;
            }
        }
        out
    }

    /// A capsule behind every filter and every excluded term, one per line it
    /// is drawn on. A term the service could not read has none: its colour
    /// says so instead.
    pub fn caps(&self, query: &str, read: &[scour_core::Span]) -> Vec<Cap> {
        let mut out = Vec::new();
        for t in lens::terms(query) {
            let inside = |s: &&scour_core::Span| {
                (s.start as usize) < t.end && (s.start + s.len) as usize > t.start
            };
            let named = read
                .iter()
                .filter(inside)
                .any(|s| s.role == scour_core::Role::Field);
            // A value that cannot be read yet keeps its capsule — it is a
            // filter being written, and its colour says the rest. A field
            // that is not one has none.
            let wrong = read.iter().filter(inside).any(|s| s.role.is_warning());
            if (wrong && !named) || !(t.not || named || t.field.is_some()) {
                continue;
            }
            // A filter with nothing after its colon is left out of the search:
            // drawn idle, a ring with no tint, until it holds something.
            let tone = if t.field.is_some() && t.value.trim().is_empty() {
                3
            } else if t.not {
                2
            } else if t.field == Some("content") {
                1
            } else {
                0
            };
            let (a, b) = (chars_before(query, t.start), chars_before(query, t.end));
            let head_ch =
                frame::head_of(query, &t, |_| named).map_or(a, |h| chars_before(query, h));
            for (i, l) in self.lines.iter().enumerate() {
                let (s, e) = (a.max(l.start), b.min(l.end));
                if s < e {
                    out.push(Cap {
                        line: i as i32,
                        at: (s - l.start) as i32,
                        len: (e - s) as i32,
                        head: (head_ch.clamp(s, e) - s) as i32,
                        tone,
                        hov: false,
                        field: t.field.unwrap_or("").into(),
                        from: t.start as i32,
                        to: t.end as i32,
                        whole: s == a && e == b,
                        first: s == a,
                        last: e == b,
                        raw: t.value.as_str().into(),
                        ..Cap::default()
                    });
                }
            }
        }
        out
    }

    fn each_line(&self, a: usize, b: usize, mut put: impl FnMut(i32, i32, i32)) {
        for (i, l) in self.lines.iter().enumerate() {
            let (s, e) = (a.max(l.start), b.min(l.end));
            if s < e {
                put(i as i32, (s - l.start) as i32, (e - s) as i32);
            }
        }
    }

    /// Where the caret is drawn, and the selection's rectangles. Scrolls so
    /// the caret's line is in sight.
    pub fn caret(&mut self, query: &str, anchor: usize, cursor: usize) -> (i32, i32, Vec<Sel>) {
        let (a, c) = (chars_before(query, anchor), chars_before(query, cursor));
        let (line, col) = wrap::place(&self.lines, c);
        if line < self.first {
            self.first = line;
        } else if line >= self.first + VISIBLE {
            self.first = line + 1 - VISIBLE;
        }
        let mut sels = Vec::new();
        self.each_line(a.min(c), a.max(c), |line, at, len| {
            sels.push(Sel {
                line,
                at,
                len,
                ..Sel::default()
            })
        });
        (line as i32, col as i32, sels)
    }

    /// Where each column is drawn, given the capsules: a header's letters are
    /// small, an ≈ header one icon wide, everything else a letter of the face.
    pub fn measure(&mut self, caps: &[Cap]) {
        let adv = if self.advance > 0.0 {
            self.advance
        } else {
            9.6
        };
        self.xs = self
            .lines
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let n = l.end.saturating_sub(l.start);
                let mut w = vec![adv; n];
                for c in caps.iter().filter(|c| c.line == i as i32 && c.len > 0) {
                    let (a, h, len) = (c.at.max(0) as usize, c.head as usize, c.len as usize);
                    if h > 0 {
                        let each = if c.icon && c.head_w > 0.0 {
                            c.head_w / h as f32
                        } else if c.icon {
                            (ICON_W + if c.tone == 2 { NOT_W } else { 0.0 }) / h as f32
                        } else {
                            adv * HEAD_SCALE - HEAD_TIGHT
                        };
                        for x in w.iter_mut().skip(a).take(h) {
                            *x = each;
                        }
                        if !c.icon
                            && let Some(x) = w.get_mut(a + h - 1)
                        {
                            *x += HEAD_GAP;
                        }
                    }
                    // What it holds, said in words: as wide as the words.
                    if c.nice_w > 0.0 && len > h {
                        let each = c.nice_w / (len - h) as f32;
                        for x in w.iter_mut().skip(a + h).take(len - h) {
                            *x = each;
                        }
                    }
                    if let Some(x) = w.get_mut(a + len - 1) {
                        *x += if c.tail > 0.0 { c.tail } else { CAP_TAIL };
                    }
                }
                let mut xs = Vec::with_capacity(n + 1);
                let mut at = 0.0;
                xs.push(at);
                for x in w {
                    at += x;
                    xs.push(at);
                }
                xs
            })
            .collect();
    }

    /// Where a column of a line (counted from the first line, not the first
    /// shown) is drawn. Past the end, a letter of the face per column.
    pub fn x(&self, line: usize, col: i32) -> f32 {
        let adv = if self.advance > 0.0 {
            self.advance
        } else {
            9.6
        };
        let col = col.max(0) as usize;
        match self.xs.get(line) {
            Some(xs) if col < xs.len() => xs[col],
            Some(xs) => xs.last().copied().unwrap_or(0.0) + (col + 1 - xs.len()) as f32 * adv,
            None => col as f32 * adv,
        }
    }

    /// The column a point on a shown line is at: the nearest gap between two
    /// characters when `round`, else the character the point is over.
    pub fn col_at(&self, shown_line: i32, x: f32, round: bool) -> i32 {
        let line = self.first + shown_line.max(0) as usize;
        let Some(xs) = self.xs.get(line) else {
            return 0;
        };
        let x = x.max(0.0);
        let mut col = 0;
        for (i, pair) in xs.windows(2).enumerate() {
            let mid = if round {
                (pair[0] + pair[1]) / 2.0
            } else {
                pair[1]
            };
            if x >= mid {
                col = i + 1;
            } else {
                break;
            }
        }
        col as i32
    }

    /// The byte offset a press at a shown line and a column lands on.
    pub fn hit(&self, query: &str, shown_line: i32, col: i32) -> usize {
        let line = self.first + shown_line.max(0) as usize;
        byte_of(query, wrap::offset(&self.lines, line, col.max(0) as usize))
    }

    /// The term drawn at a shown line and column, if a character of one is
    /// there; and the line and column just past its end, for its buttons.
    pub fn term_at(
        &self,
        query: &str,
        shown_line: i32,
        col: i32,
    ) -> Option<(lens::Term, i32, i32)> {
        let line = self.first + shown_line.max(0) as usize;
        let l = *self.lines.get(line)?;
        let ch = l.start + col.max(0) as usize;
        if ch >= l.end {
            return None;
        }
        let byte = byte_of(query, ch);
        let t = lens::terms(query)
            .into_iter()
            .find(|t| byte >= t.start && byte < t.end)?;
        let (eline, ecol) = wrap::place(&self.lines, chars_before(query, t.end).max(1) - 1);
        Some((t, eline as i32 - self.first as i32, ecol as i32 + 1))
    }

    /// Where a term begins, as a shown line and a column: the lens opens under it.
    pub fn start_of(&self, query: &str, byte: usize) -> (i32, i32) {
        let (line, col) = wrap::place(&self.lines, chars_before(query, byte));
        (line as i32 - self.first as i32, col as i32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A header drawn narrow moves what follows it, and a press is read back
    /// through the same columns: no column is lost or doubled on the way.
    #[test]
    fn a_narrow_header_is_pressed_where_it_is_drawn() {
        let mut f = field("ext:pdf rapor", 80);
        f.advance = 10.0;
        let cap = Cap {
            line: 0,
            at: 0,
            len: 7,
            head: 4,
            ..Cap::default()
        };
        f.measure(std::slice::from_ref(&cap));
        let narrow = 10.0 * HEAD_SCALE - HEAD_TIGHT;
        assert!((f.x(0, 4) - (4.0 * narrow + HEAD_GAP)).abs() < 0.01);
        assert!((f.x(0, 5) - (4.0 * narrow + HEAD_GAP + 10.0)).abs() < 0.01);
        assert!((f.x(0, 7) - (4.0 * narrow + HEAD_GAP + 30.0 + CAP_TAIL)).abs() < 0.01);
        for col in 0..=13 {
            let x = f.x(0, col);
            assert_eq!(f.col_at(0, x, true), col, "a gap at column {col}");
            if col < 13 {
                assert_eq!(f.col_at(0, x + 0.5, false), col, "a character at {col}");
            }
        }
        f.measure(&[Cap { icon: true, ..cap }]);
        assert!((f.x(0, 4) - ICON_W).abs() < 0.01);
    }

    fn field(q: &str, cols: usize) -> Field {
        let mut f = Field {
            cols,
            ..Default::default()
        };
        f.relayout(q);
        f
    }

    #[test]
    fn runs_are_cut_at_the_line_ends() {
        let q = "rapor ext:pdf";
        let f = field(q, 8);
        let runs = vec![Span {
            text: q.into(),
            role: 6,
            not: false,
            at: 0,
            line: 0,
            ..Span::default()
        }];
        let placed: Vec<(String, i32, i32)> = f
            .place(runs)
            .into_iter()
            .map(|s| (s.text.to_string(), s.line, s.at))
            .collect();
        assert_eq!(placed, [("rapor ".into(), 0, 0), ("ext:pdf".into(), 1, 0)]);
    }

    #[test]
    fn the_caret_and_a_press_agree_across_lines() {
        let q = "şirket under:~/Belgeler";
        let mut f = field(q, 10);
        let (line, col, sels) = f.caret(q, q.len(), q.len());
        assert_eq!((line, col), (2, 6));
        assert!(sels.is_empty());
        assert_eq!(f.hit(q, 2, 6), q.len());
        assert_eq!(f.hit(q, 0, 1), "ş".len());
        let (_, _, sels) = f.caret(q, 0, "şirket ".len());
        assert_eq!(sels.len(), 1);
    }

    #[test]
    fn a_term_is_found_where_it_is_drawn() {
        let q = "rapor !taslak";
        let f = field(q, 40);
        let (t, line, end) = f.term_at(q, 0, 8).unwrap();
        assert!(t.not);
        assert_eq!((line, end), (0, 13));
        assert!(f.term_at(q, 0, 5).is_none());
        assert!(f.term_at(q, 0, 30).is_none());
    }

    #[test]
    fn a_long_query_scrolls_to_the_caret() {
        let q = "a b c d e f g h i j k l m n o p q r s t u v w x y z";
        let mut f = field(q, 4);
        assert!(f.lines.len() > VISIBLE);
        f.caret(q, q.len(), q.len());
        assert_eq!(f.first, f.lines.len() - VISIBLE);
        f.caret(q, 0, 0);
        assert_eq!(f.first, 0);
    }
}
