//! A content pattern as a field draws it: `icra "dava dosyası" !karar a;b`
//! taken apart into the pieces the search takes it apart into, with where
//! each stands, so a face can give each its capsule and each operator its
//! colour without asking the service on every keystroke.

/// What one piece of a text pattern is, as a field draws it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PieceKind {
    /// A plain word: the file must hold it.
    Word,
    /// `"…"`: these words, in this order.
    Phrase,
    /// `a;b`, `a | b`, `(a | b)`: either.
    Or,
    /// `!word`: the file must not hold it.
    Not,
    /// `~word`: a letter off still matches.
    Similar,
}

impl PieceKind {
    /// The token a page's class or a test names it by.
    pub fn as_str(self) -> &'static str {
        match self {
            PieceKind::Word => "word",
            PieceKind::Phrase => "phrase",
            PieceKind::Or => "or",
            PieceKind::Not => "not",
            PieceKind::Similar => "similar",
        }
    }

    /// The msgid a capsule of this kind is headed by; none for a plain word.
    pub fn label(self) -> Option<&'static str> {
        match self {
            PieceKind::Word => None,
            PieceKind::Phrase => Some("exact phrase"),
            PieceKind::Or => Some("either"),
            PieceKind::Not => Some("without"),
            PieceKind::Similar => Some("similar"),
        }
    }
}

/// The msgid drawn between two pieces: every piece must hold.
pub const AND: &str = "and";
/// The msgid drawn between two alternatives of an `Or` or a `Not`.
pub const OR: &str = "or";

/// One alternative of a piece: the text it looks for, operators left out.
/// An empty one (`start == end`) is a hole: an operator written and its word
/// not yet — after `!`, `~`, a trailing `;`/`|`, between two quotes — where
/// a face keeps the caret inside the capsule. A hole looks for nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Alt {
    pub start: usize,
    pub end: usize,
    /// Written `"…"`: these words, in this order.
    pub phrase: bool,
    /// Written `~…`: a letter off still matches.
    pub similar: bool,
}

/// One piece of a text pattern where it stands in it — character columns,
/// end excluded — and the columns of its operators (`!`, `;`, `|`, quotes,
/// parentheses), read by the rule `scour_grep::Expr` reads it by (a test in
/// `scour-engine` holds the two to one reading).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Piece {
    pub start: usize,
    pub end: usize,
    pub kind: PieceKind,
    pub ops: Vec<usize>,
    /// What it looks for, one inner capsule each: one for a word or a
    /// phrase, several for `a|b`, and a `Not`'s own words.
    pub alts: Vec<Alt>,
}

impl Alt {
    pub fn is_hole(&self) -> bool {
        self.start == self.end
    }
}

impl Piece {
    /// Written but holding no word yet: every alternative a hole.
    pub fn is_empty(&self) -> bool {
        self.alts.iter().all(Alt::is_hole)
    }
}

/// The piece the caret stands in or at the edge of: the one a face draws as
/// being written.
pub fn piece_at(pieces: &[Piece], caret: usize) -> Option<usize> {
    pieces
        .iter()
        .position(|p| p.start <= caret && caret <= p.end)
}

/// The hole the caret is in, as a piece and its alternative.
pub fn hole_at(pieces: &[Piece], caret: usize) -> Option<(usize, usize)> {
    pieces.iter().enumerate().find_map(|(k, p)| {
        p.alts
            .iter()
            .position(|a| a.is_hole() && a.start == caret)
            .map(|j| (k, j))
    })
}

pub mod edit;
mod launch;
pub use launch::{Item, ItemAction, asking, choose, items};

/// The pieces of a text pattern, for a field to draw: each clause the
/// search takes apart, where it is.
pub fn pieces(input: &str) -> Vec<Piece> {
    let chars: Vec<char> = input.chars().collect();
    // Tokens on unquoted whitespace outside parentheses, as `split_clauses`.
    let mut tokens: Vec<(usize, usize)> = Vec::new();
    let (mut start, mut in_quotes, mut depth) = (None, false, 0usize);
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                start.get_or_insert(i);
            }
            '(' if !in_quotes && start.is_none() => {
                depth += 1;
                start = Some(i);
            }
            ')' if !in_quotes && depth > 0 => depth -= 1,
            c if c.is_whitespace() && !in_quotes && depth == 0 => {
                if let Some(s) = start.take() {
                    tokens.push((s, i));
                }
            }
            _ => {
                start.get_or_insert(i);
            }
        }
    }
    if let Some(s) = start {
        tokens.push((s, chars.len()));
    }
    let text = |(a, b): (usize, usize)| chars[a..b].iter().collect::<String>();
    // Glued as the search glues them: a lone `;`/`|` joins its neighbours, a
    // lone `!` the next, a leading or trailing one the neighbour on its side.
    let mut groups: Vec<(usize, usize)> = Vec::new();
    let (mut glue, mut bang): (bool, Option<usize>) = (false, None);
    for &tok in &tokens {
        let t = text(tok);
        let quoted = t.starts_with('"');
        if !quoted && (t == "|" || t == ";") {
            glue = true;
            match groups.last_mut() {
                Some(last) => last.1 = tok.1,
                // Nothing to be the other of yet: an empty "either".
                None => groups.push(tok),
            }
            continue;
        }
        if !quoted && t == "!" {
            bang = Some(tok.0);
            continue;
        }
        let lead = !quoted && (t.starts_with('|') || t.starts_with(';'));
        let trail = !quoted && t.len() > 1 && (t.ends_with('|') || t.ends_with(';'));
        let from = bang.take().unwrap_or(tok.0);
        match groups.last_mut() {
            Some(last) if glue || lead => last.1 = tok.1,
            _ => groups.push((from, tok.1)),
        }
        glue = trail;
    }
    // A `!` with no word after it yet: an empty "without".
    if let Some(b) = bang
        && groups.last().is_none_or(|g| g.1 <= b)
    {
        groups.push((b, b + 1));
    }
    groups
        .into_iter()
        .map(|(start, end)| {
            let mut ops = Vec::new();
            let (mut quotes, mut or, mut not, mut phrase, mut similar) =
                (false, false, false, false, false);
            for (i, &c) in chars.iter().enumerate().take(end).skip(start) {
                match c {
                    '"' => {
                        quotes = !quotes;
                        phrase = true;
                        ops.push(i);
                    }
                    ';' | '|' if !quotes => {
                        or = true;
                        ops.push(i);
                    }
                    '(' | ')' if !quotes => ops.push(i),
                    '!' if !quotes && (i == start || chars[i - 1].is_whitespace()) => {
                        not = true;
                        ops.push(i);
                    }
                    '~' if !quotes && tilde_at(&chars, i, start) => {
                        similar = true;
                        ops.push(i);
                    }
                    _ => {}
                }
            }
            let kind = if not {
                PieceKind::Not
            } else if or {
                PieceKind::Or
            } else if similar {
                PieceKind::Similar
            } else if phrase {
                PieceKind::Phrase
            } else {
                PieceKind::Word
            };
            Piece {
                start,
                end,
                kind,
                alts: alts(&chars, start, end),
                ops,
            }
        })
        .collect()
}

/// A `~` that makes a term forgiving: at a term's start, after `!`, `;`, `|`,
/// `(` or a space, and followed by a letter of the word or by nothing yet.
fn tilde_at(chars: &[char], i: usize, start: usize) -> bool {
    let before =
        i == start || matches!(chars[i - 1], '!' | ';' | '|' | '(') || chars[i - 1].is_whitespace();
    let after = chars
        .get(i + 1)
        .is_none_or(|c| !c.is_whitespace() && !matches!(c, '"' | ';' | '|' | ')' | '~'));
    before && after
}

/// The alternatives of the piece at `start..end`, each without its operators,
/// and a hole where the last one, or the only one, is not written yet.
fn alts(chars: &[char], start: usize, end: usize) -> Vec<Alt> {
    let mut out = Vec::new();
    let close = |a: usize, b: usize, last: bool, seps: usize, out: &mut Vec<Alt>| {
        let (mut a, mut b) = (a, b);
        let mut similar = false;
        while a < b && (chars[a].is_whitespace() || matches!(chars[a], '(' | '!' | '~')) {
            similar |= chars[a] == '~' && tilde_at(chars, a, start);
            a += 1;
        }
        while b > a && (chars[b - 1].is_whitespace() || chars[b - 1] == ')') {
            b -= 1;
        }
        // A phrase closed, or one still being written: no quote after it.
        let quoted = a < b && chars[a] == '"';
        let closed = quoted && b > a + 1 && chars[b - 1] == '"';
        let open = quoted && !closed && !chars[a + 1..b].contains(&'"');
        let phrase = closed || open;
        let (a, b) = match (closed, open) {
            (true, _) => (a + 1, b - 1),
            (_, true) => (a + 1, b),
            _ => (a, b),
        };
        if a < b || (last && (seps > 0 || out.is_empty())) {
            out.push(Alt {
                start: a,
                end: b.max(a),
                phrase,
                similar,
            });
        }
    };
    let (mut quotes, mut seps, mut from) = (false, 0usize, start);
    for (i, &c) in chars.iter().enumerate().take(end).skip(start) {
        match c {
            '"' => quotes = !quotes,
            ';' | '|' if !quotes => {
                close(from, i, false, seps, &mut out);
                from = i + 1;
                seps += 1;
            }
            _ => {}
        }
    }
    close(from, end, true, seps, &mut out);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(input: &str) -> Vec<(String, PieceKind, Vec<char>)> {
        let c: Vec<char> = input.chars().collect();
        pieces(input)
            .into_iter()
            .map(|p| {
                (
                    c[p.start..p.end].iter().collect(),
                    p.kind,
                    p.ops.iter().map(|&i| c[i]).collect(),
                )
            })
            .collect()
    }

    #[test]
    fn a_pattern_is_taken_apart_as_the_search_takes_it() {
        use PieceKind::*;
        assert_eq!(
            shown(r#"icra "dava dosyası" !karar a;b"#),
            vec![
                ("icra".into(), Word, vec![]),
                (r#""dava dosyası""#.into(), Phrase, vec!['"', '"']),
                ("!karar".into(), Not, vec!['!']),
                ("a;b".into(), Or, vec![';']),
            ]
        );
        // Spaced ORs, parentheses and a lone `!` glue as the search glues them.
        assert_eq!(
            shown("(a | b) x ; y ! z"),
            vec![
                ("(a | b)".into(), Or, vec!['(', '|', ')']),
                ("x ; y".into(), Or, vec![';']),
                ("! z".into(), Not, vec!['!']),
            ]
        );
        assert_eq!(shown("şirket"), vec![("şirket".into(), Word, vec![])]);
    }

    fn alts(input: &str) -> Vec<Vec<(String, bool, bool)>> {
        let c: Vec<char> = input.chars().collect();
        pieces(input)
            .into_iter()
            .map(|p| {
                p.alts
                    .iter()
                    .map(|a| (c[a.start..a.end].iter().collect(), a.phrase, a.similar))
                    .collect()
            })
            .collect()
    }

    #[test]
    fn a_tilde_forgives_and_each_alternative_is_its_own_capsule() {
        use PieceKind::*;
        assert_eq!(shown("~rapor"), vec![("~rapor".into(), Similar, vec!['~'])]);
        assert_eq!(
            shown("!~taslak ~a|b"),
            vec![
                ("!~taslak".into(), Not, vec!['!', '~']),
                ("~a|b".into(), Or, vec!['~', '|']),
            ]
        );
        // Not a word's start: a tilde as text. Nothing after it yet: a
        // similar word still to be written.
        assert_eq!(shown("a~b"), vec![("a~b".into(), Word, vec![])]);
        assert_eq!(shown("~"), vec![("~".into(), Similar, vec!['~'])]);
        assert_eq!(shown(r#""~x""#)[0].1, Phrase);
        let s = |t: &str, p: bool, m: bool| (t.to_string(), p, m);
        assert_eq!(
            alts(r#"icra ~a|"b c" !(x | y)"#),
            vec![
                vec![s("icra", false, false)],
                vec![s("a", false, true), s("b c", true, false)],
                vec![s("x", false, false), s("y", false, false)],
            ]
        );
    }

    /// Each hole as (piece, its column, phrase, similar).
    fn holes(input: &str) -> Vec<(usize, usize, bool, bool)> {
        pieces(input)
            .iter()
            .enumerate()
            .flat_map(|(k, p)| {
                p.alts
                    .iter()
                    .filter(|a| a.is_hole())
                    .map(move |a| (k, a.start, a.phrase, a.similar))
            })
            .collect()
    }

    #[test]
    fn an_operator_with_no_word_yet_is_a_capsule_with_a_hole() {
        use PieceKind::*;
        // A lone `!`, `;` or `~`, and two quotes, each a piece of its own.
        assert_eq!(shown("!"), vec![("!".into(), Not, vec!['!'])]);
        assert_eq!(
            shown("ara !"),
            vec![("ara".into(), Word, vec![]), ("!".into(), Not, vec!['!'])]
        );
        assert_eq!(shown(";"), vec![(";".into(), Or, vec![';'])]);
        assert_eq!(shown("; x"), vec![("; x".into(), Or, vec![';'])]);
        assert_eq!(
            shown(r#""""#),
            vec![(r#""""#.into(), Phrase, vec!['"', '"'])]
        );
        assert_eq!(holes("!"), vec![(0, 1, false, false)]);
        assert_eq!(holes("ara !"), vec![(1, 5, false, false)]);
        assert_eq!(holes("!~"), vec![(0, 2, false, true)]);
        assert_eq!(holes("~"), vec![(0, 1, false, true)]);
        assert_eq!(holes(r#""""#), vec![(0, 1, true, false)]);
        assert_eq!(holes(r#"!"""#), vec![(0, 2, true, false)]);
        assert_eq!(holes("\""), vec![(0, 1, true, false)]);
        // After the last `;`, not before the first: "a or …", and `;x` is x.
        assert_eq!(holes("ara;"), vec![(0, 4, false, false)]);
        assert_eq!(holes("ara ;"), vec![(0, 5, false, false)]);
        assert_eq!(holes("a;;"), vec![(0, 3, false, false)]);
        assert_eq!(holes(";x"), vec![]);
        assert_eq!(holes("a;b"), vec![]);
        assert!(pieces("!").first().is_some_and(Piece::is_empty));
        assert!(!pieces("ara;").first().is_some_and(Piece::is_empty));
        assert_eq!(hole_at(&pieces("x ara;"), 6), Some((1, 1)));
        assert_eq!(hole_at(&pieces("x ara;"), 5), None);
        // A phrase still being written reads as one.
        assert_eq!(alts("\"ab c"), vec![vec![("ab c".into(), true, false)]]);
    }

    #[test]
    fn the_caret_keeps_its_piece_as_letters() {
        let p = pieces("ab cd");
        assert_eq!(piece_at(&p, 0), Some(0));
        assert_eq!(piece_at(&p, 2), Some(0));
        assert_eq!(piece_at(&p, 4), Some(1));
        assert_eq!(piece_at(&pieces("ab  cd"), 3), None);
    }
}
