//! The query field as this window draws it. Slint's `TextInput` can neither
//! colour a range nor wrap where the page wraps, so it stays one invisible
//! line that holds the text, the keys, the clipboard and the input method, and
//! this says where everything is drawn: the lines, the coloured runs, the
//! capsules, the caret and the selection, and which term a point is over.

use scour_ui::{lens, wrap};

use crate::{Cap, Sel, Span};

/// Lines shown before the field scrolls inside itself.
pub const VISIBLE: usize = 3;

#[derive(Default)]
pub struct Field {
    /// Characters a line holds; zero until the window has measured.
    pub cols: usize,
    /// The first line shown, once there are more than [`VISIBLE`].
    pub first: usize,
    pub lines: Vec<wrap::Line>,
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
            let wrong = read.iter().filter(inside).any(|s| s.role.is_warning());
            if wrong || !(t.not || named || t.field.is_some()) {
                continue;
            }
            // An empty `content:` is left out of the search: drawn idle, a ring
            // with no tint, until something is typed in it.
            let tone = if t.field == Some("content") && t.value.trim().is_empty() {
                3
            } else if t.not {
                2
            } else if t.field == Some("content") {
                1
            } else {
                0
            };
            let (a, b) = (chars_before(query, t.start), chars_before(query, t.end));
            self.each_line(a, b, |line, at, len| {
                out.push(Cap {
                    line,
                    at,
                    len,
                    tone,
                    hov: false,
                })
            });
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
            sels.push(Sel { line, at, len })
        });
        (line as i32, col as i32, sels)
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
