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
}

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
            if let Some(last) = groups.last_mut() {
                last.1 = tok.1;
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
    groups
        .into_iter()
        .map(|(start, end)| {
            let mut ops = Vec::new();
            let (mut quotes, mut or, mut not, mut phrase) = (false, false, false, false);
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
                    _ => {}
                }
            }
            let kind = if not {
                PieceKind::Not
            } else if or {
                PieceKind::Or
            } else if phrase {
                PieceKind::Phrase
            } else {
                PieceKind::Word
            };
            Piece {
                start,
                end,
                kind,
                ops,
            }
        })
        .collect()
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
}
