//! Where a query wraps, in a box `cols` characters wide, in a face where every
//! character is one column. The page's text area wraps after spaces and breaks
//! a word only when it is longer than a line; this is that rule, so a window
//! that draws its own lines puts every letter where the page does.

/// One drawn line, in characters of the query: `start..end`, the spaces a
/// break hangs at the end of it included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Line {
    pub start: usize,
    pub end: usize,
}

/// The lines `text` takes. Never empty: an empty query is one empty line.
pub fn lines(text: &str, cols: usize) -> Vec<Line> {
    let chars: Vec<char> = text.chars().collect();
    let cols = cols.max(1);
    let mut out = Vec::new();
    let mut start = 0;
    while start < chars.len() {
        let mut end = start;
        // The last place a line may end: after a space, before a non-space.
        let mut fit = None;
        while end < chars.len() && end - start < cols {
            end += 1;
            if chars[end - 1].is_whitespace() && end < chars.len() && !chars[end].is_whitespace() {
                fit = Some(end);
            }
        }
        if end < chars.len() {
            if chars[end].is_whitespace() {
                // The break falls on a space: the spaces hang on this line.
                while end < chars.len() && chars[end].is_whitespace() {
                    end += 1;
                }
            } else if let Some(f) = fit {
                end = f;
            }
        }
        out.push(Line { start, end });
        start = end;
    }
    if out.is_empty() {
        out.push(Line { start: 0, end: 0 });
    }
    out
}

/// The line and column a character offset is drawn at. An offset where one
/// line ends and the next begins is the next line's start, except at the end.
pub fn place(lines: &[Line], at: usize) -> (usize, usize) {
    for (i, l) in lines.iter().enumerate() {
        let last = i + 1 == lines.len();
        if at < l.end || (last && at <= l.end) {
            return (i, at.saturating_sub(l.start));
        }
    }
    let i = lines.len() - 1;
    (i, lines[i].end - lines[i].start)
}

/// The character offset under a line and a column, the column clamped to what
/// the line holds. A press past a line's end lands before its hanging spaces'
/// end only on the last line; elsewhere at the end of the line's own text.
pub fn offset(lines: &[Line], line: usize, col: usize) -> usize {
    let i = line.min(lines.len() - 1);
    let l = lines[i];
    let room = if i + 1 == lines.len() {
        l.end - l.start
    } else {
        (l.end - l.start).saturating_sub(1)
    };
    l.start + col.min(room)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cut(text: &str, cols: usize) -> Vec<String> {
        let c: Vec<char> = text.chars().collect();
        lines(text, cols)
            .iter()
            .map(|l| c[l.start..l.end].iter().collect())
            .collect()
    }

    #[test]
    fn words_go_whole_to_the_next_line() {
        assert_eq!(
            cut("rapor ext:pdf dm:7d", 12),
            ["rapor ", "ext:pdf ", "dm:7d"]
        );
        assert_eq!(cut("rapor ext:pdf", 40), ["rapor ext:pdf"]);
        assert_eq!(cut("", 10), [""]);
    }

    #[test]
    fn a_break_on_a_space_hangs_the_space() {
        assert_eq!(cut("abcde fgh", 5), ["abcde ", "fgh"]);
        assert_eq!(cut("abcde   fgh", 5), ["abcde   ", "fgh"]);
    }

    #[test]
    fn a_word_longer_than_a_line_is_broken() {
        assert_eq!(
            cut("under:/home/u/Belgeler/Hukuk", 10),
            ["under:/hom", "e/u/Belgel", "er/Hukuk"]
        );
        assert_eq!(
            cut("ab averyverylongword", 6),
            ["ab ", "averyv", "erylon", "gword"]
        );
    }

    #[test]
    fn turkish_letters_are_one_column_each() {
        assert_eq!(cut("şğü ıİö", 4), ["şğü ", "ıİö"]);
    }

    #[test]
    fn places_and_offsets_agree() {
        let l = lines("rapor ext:pdf dm:7d", 12);
        assert_eq!(place(&l, 0), (0, 0));
        assert_eq!(place(&l, 6), (1, 0));
        assert_eq!(place(&l, 19), (2, 5));
        assert_eq!(offset(&l, 0, 99), 5);
        assert_eq!(offset(&l, 1, 3), 9);
        assert_eq!(offset(&l, 2, 99), 19);
        assert_eq!(offset(&l, 9, 0), 14);
    }
}
