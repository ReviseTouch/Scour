//! The words of a paragraph in a lightweight markup — reStructuredText,
//! AsciiDoc, Org, Typst, a man page's fonts — with their emphasis, read by
//! one set of rules: each format gives its delimiters and its links, and a
//! delimiter counts only where its language would count it — a constrained
//! one at a word's edge, around words; an unconstrained one anywhere.

use crate::build::Builder;

/// How deep a reader nests blocks in blocks: past it, what is deeper is
/// drawn as plain words. Without it, a file of a thousand indents costs
/// seconds, each level reading all the lines under it again.
pub(crate) const MOST_DEPTH: usize = 32;

/// `lines` as plain paragraphs, unread: what lies past [`MOST_DEPTH`].
pub(crate) fn flat(lines: &[&str], b: &mut Builder) {
    b.flush();
    for part in lines
        .split(|l| l.trim().is_empty())
        .filter(|p| !p.is_empty())
    {
        b.folded(&joined(part));
        b.flush();
    }
}

/// What a pair of delimiters marks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mark {
    Bold,
    Italic,
    Underline,
    Strike,
    /// Fixed-width, and what is inside it is still markup.
    Mono,
    /// No look of its own: a highlight, a span, a sub- or superscript.
    Plain,
    /// Fixed-width and verbatim: nothing inside it is markup.
    Code,
    /// Verbatim with no look: a passthrough.
    Verbatim,
}

/// Which characters a backslash makes plain.
#[derive(Clone, Copy)]
pub(crate) enum Escape<'a> {
    None,
    /// Any character; before white space, both are dropped, as
    /// reStructuredText has it.
    Any,
    /// Only these; before any other, the backslash is a backslash.
    Only(&'a str),
}

/// A construct of a format's own at the start of the text it is given — a
/// link, a role, a macro — as its length, the words it shows, and whether
/// those are a link. Told whether the text starts at a word's edge.
pub(crate) type Special<'a> = &'a dyn Fn(&str, bool) -> Option<(usize, String, bool)>;

/// A format's inline rules.
pub(crate) struct Rules<'a> {
    /// Delimiters that count at a word's edge only, longest first where one
    /// begins another.
    pub pairs: &'a [(&'a str, &'a str, Mark)],
    /// Delimiters that count anywhere, inside a word too.
    pub loose: &'a [(&'a str, &'a str, Mark)],
    pub special: Special<'a>,
    pub escape: Escape<'a>,
}

/// `text`'s words into `b`, marked as `rules` read them.
pub(crate) fn inline(text: &str, b: &mut Builder, rules: &Rules<'_>) {
    let mut plain = String::new();
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        if let Some(after) = rest.strip_prefix('\\') {
            let next = after.chars().next();
            let n = next.map_or(0, char::len_utf8);
            match (rules.escape, next) {
                (Escape::None, _) | (_, None) => {}
                (Escape::Any, Some(c)) if c.is_whitespace() => {
                    i += 1 + n;
                    continue;
                }
                (Escape::Any, Some(c)) => {
                    plain.push(c);
                    i += 1 + n;
                    continue;
                }
                (Escape::Only(these), Some(c)) if these.contains(c) => {
                    plain.push(c);
                    i += 1 + n;
                    continue;
                }
                _ => {}
            }
        }
        let edge = before_is_edge(text, i);
        if let Some((n, shown, link)) = (rules.special)(rest, edge) {
            flush(b, &mut plain);
            if link {
                b.look.link += 1;
            }
            b.folded(&shown);
            if link {
                b.look.link -= 1;
            }
            i += n.max(1);
            continue;
        }
        let pairs = rules
            .loose
            .iter()
            .map(|p| (p, true))
            .chain(rules.pairs.iter().map(|p| (p, false)).filter(|_| edge));
        let mut taken = false;
        for (&(open, close, mark), loose) in pairs {
            let Some(inner) = rest.strip_prefix(open) else {
                continue;
            };
            if inner.starts_with(char::is_whitespace) || inner.is_empty() {
                continue;
            }
            let Some(end) = closing(inner, close, loose) else {
                continue;
            };
            flush(b, &mut plain);
            let body = &inner[..end];
            match mark {
                Mark::Code => b.words(body, true),
                Mark::Verbatim => b.folded(body),
                _ => {
                    set(b, mark, true);
                    inline(body, b, rules);
                    set(b, mark, false);
                }
            }
            i += open.len() + end + close.len();
            taken = true;
            break;
        }
        if !taken {
            let c = rest.chars().next().expect("not at the end");
            plain.push(c);
            i += c.len_utf8();
        }
    }
    flush(b, &mut plain);
}

/// A paragraph's lines, a hard break between each: each read by `rules`.
pub(crate) fn broken(lines: &[String], b: &mut Builder, rules: &Rules<'_>) {
    for (n, line) in lines.iter().enumerate() {
        if n > 0 {
            b.words("\n", false);
        }
        inline(line, b, rules);
    }
}

fn flush(b: &mut Builder, plain: &mut String) {
    if !plain.is_empty() {
        b.folded(plain);
        plain.clear();
    }
}

fn set(b: &mut Builder, mark: Mark, on: bool) {
    let n = match mark {
        Mark::Bold => &mut b.look.bold,
        Mark::Italic => &mut b.look.italic,
        Mark::Underline => &mut b.look.underline,
        Mark::Strike => &mut b.look.strike,
        Mark::Mono | Mark::Code => &mut b.look.code,
        Mark::Plain | Mark::Verbatim => return,
    };
    *n = if on {
        n.saturating_add(1)
    } else {
        n.saturating_sub(1)
    };
}

/// A delimiter may open where the text starts or after a space or a mark of
/// punctuation: `a*b` is no emphasis.
fn before_is_edge(text: &str, i: usize) -> bool {
    text[..i]
        .chars()
        .next_back()
        .is_none_or(|c| !c.is_alphanumeric())
}

/// Where `close` ends what `inner` opened: after a non-space, and — for a
/// constrained pair — before the text's end, a space or punctuation.
fn closing(inner: &str, close: &str, loose: bool) -> Option<usize> {
    let mut from = 0;
    while let Some(at) = inner[from..].find(close).map(|a| a + from) {
        let before = inner[..at].chars().next_back();
        let after = inner[at + close.len()..].chars().next();
        if at > 0
            && before.is_some_and(|c| !c.is_whitespace())
            && (loose || after.is_none_or(|c| !c.is_alphanumeric()))
        {
            return Some(at);
        }
        from = at + close.len().max(1);
    }
    None
}

/// `text`'s lines joined as one paragraph's: each trimmed, a space between.
pub(crate) fn joined(lines: &[&str]) -> String {
    lines
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A line's indent, in columns, a tab to the next multiple of eight.
pub(crate) fn indent(line: &str) -> usize {
    let mut n = 0;
    for c in line.chars() {
        match c {
            ' ' => n += 1,
            '\t' => n = (n / 8 + 1) * 8,
            _ => break,
        }
    }
    n
}

/// `lines` with `by` columns of indent taken off each.
pub(crate) fn dedent<'a>(lines: &[&'a str], by: usize) -> Vec<&'a str> {
    lines
        .iter()
        .map(|l| {
            let mut cols = 0;
            for (i, c) in l.char_indices() {
                if cols >= by || !matches!(c, ' ' | '\t') {
                    return &l[i..];
                }
                cols = if c == '\t' {
                    (cols / 8 + 1) * 8
                } else {
                    cols + 1
                };
            }
            ""
        })
        .collect()
}

/// The common indent of the lines that say something.
pub(crate) fn least_indent(lines: &[&str]) -> usize {
    lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .map(|l| indent(l))
        .min()
        .unwrap_or(0)
}

/// The text inside the brackets `text` opens with, `[` to its matching `]`,
/// and the length taken: brackets nest, `\]` does not close.
pub(crate) fn bracketed(text: &str) -> Option<(&str, usize)> {
    if !text.starts_with('[') {
        return None;
    }
    let mut depth = 0;
    let mut escaped = false;
    for (i, c) in text.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&text[1..i], i + 1));
                }
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use scour_flow::Block;

    fn runs(text: &str) -> Vec<(String, bool, bool, bool)> {
        let rules = Rules {
            pairs: &[
                ("**", "**", Mark::Bold),
                ("*", "*", Mark::Italic),
                ("``", "``", Mark::Code),
            ],
            loose: &[("__", "__", Mark::Bold)],
            special: &|_, _| None,
            escape: Escape::Any,
        };
        let mut b = Builder::default();
        inline(text, &mut b, &rules);
        match b.finish().blocks.first() {
            Some(Block::Para(p)) => p
                .runs
                .iter()
                .map(|r| (r.text.clone(), r.bold, r.italic, r.code))
                .collect(),
            _ => Vec::new(),
        }
    }

    #[test]
    fn a_delimiter_counts_at_a_words_edge_only() {
        assert_eq!(
            runs("a **kalın** ve *eğik* ``kod*x*`` 2*3*4"),
            [
                ("a ".into(), false, false, false),
                ("kalın".into(), true, false, false),
                (" ve ".into(), false, false, false),
                ("eğik".into(), false, true, false),
                (" ".into(), false, false, false),
                ("kod*x*".into(), false, false, true),
                (" 2*3*4".into(), false, false, false),
            ]
        );
        assert_eq!(runs("* not emphasis *").len(), 1);
        assert_eq!(runs(r"\*plain\* a\ b \\")[0].0, r"*plain* ab \");
        // An unconstrained pair counts inside a word.
        assert_eq!(
            runs("iç__kalın__dış")[1],
            ("kalın".into(), true, false, false)
        );
    }

    #[test]
    fn brackets_nest_and_escape() {
        assert_eq!(bracketed("[a [b] c]d"), Some(("a [b] c", 9)));
        assert_eq!(bracketed(r"[a \] b]"), Some((r"a \] b", 8)));
        assert_eq!(bracketed("[open"), None);
        assert_eq!(bracketed("x[a]"), None);
    }
}
