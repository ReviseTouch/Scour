//! What a content capsule's pill, and the keys that write an operator, do to
//! the pattern: the text after, and where the caret goes. The pattern stays
//! the only model; these rewrite it.

use super::{Piece, PieceKind, hole_at, pieces};

fn cut(input: &str, from: usize, to: usize, with: &str) -> String {
    let c: Vec<char> = input.chars().collect();
    let head: String = c[..from].iter().collect();
    let tail: String = c[to..].iter().collect();
    format!("{head}{with}{tail}")
}

/// The pattern without the piece at `at`, and one space less.
pub fn remove(input: &str, at: usize) -> String {
    let p = pieces(input);
    let Some(piece) = p.get(at) else {
        return input.to_string();
    };
    let c: Vec<char> = input.chars().collect();
    let mut to = piece.end;
    while to < c.len() && c[to].is_whitespace() {
        to += 1;
    }
    let mut from = piece.start;
    if to == c.len() {
        while from > 0 && c[from - 1].is_whitespace() {
            from -= 1;
        }
    }
    cut(input, from, to, "")
}

/// The piece at `at` excluded, or no longer: a `!` before it, or its `!` gone.
pub fn toggle_not(input: &str, at: usize) -> String {
    let p = pieces(input);
    let Some(piece) = p.get(at) else {
        return input.to_string();
    };
    if piece.kind == PieceKind::Not {
        let c: Vec<char> = input.chars().collect();
        let bang = piece
            .ops
            .iter()
            .copied()
            .find(|&i| c[i] == '!')
            .unwrap_or(piece.start);
        let mut to = bang + 1;
        while to < piece.end && c[to].is_whitespace() {
            to += 1;
        }
        cut(input, bang, to, "")
    } else {
        cut(input, piece.start, piece.start, "!")
    }
}

/// Whether the alternative `alt` of the piece at `at` forgives a letter off.
pub fn similar(piece: &Piece, alt: usize) -> bool {
    piece.alts.get(alt).is_some_and(|a| a.similar)
}

/// The alternative `alt` of the piece at `at` forgiving, or exact again. A
/// phrase is never forgiving: its words are looked for as typed.
pub fn toggle_similar(input: &str, at: usize, alt: usize) -> String {
    let p = pieces(input);
    let Some(a) = p.get(at).and_then(|p| p.alts.get(alt)) else {
        return input.to_string();
    };
    if a.phrase {
        return input.to_string();
    }
    if a.similar {
        cut(input, a.start - 1, a.start, "")
    } else {
        cut(input, a.start, a.start, "~")
    }
}

/// Backspace in a hole takes the operator that opened it: an empty capsule
/// goes whole, `!`, `~` and both quotes with it; an empty last alternative
/// takes its `;`. Just inside a phrase's opening quote, both quotes go and
/// its words stay. `None`: an ordinary backspace.
pub fn backspace(input: &str, caret: usize) -> Option<(String, usize)> {
    let c: Vec<char> = input.chars().collect();
    let p = pieces(input);
    if let Some((k, _)) = hole_at(&p, caret) {
        let piece = &p[k];
        if piece.is_empty() {
            return Some((cut(input, piece.start, piece.end, ""), piece.start));
        }
        let sep = (piece.start..caret)
            .rev()
            .find(|&i| matches!(c[i], ';' | '|'))?;
        return Some((cut(input, sep, caret, ""), sep));
    }
    let a = p
        .iter()
        .flat_map(|p| p.alts.iter())
        .find(|a| a.phrase && a.start == caret && caret > 0 && c[caret - 1] == '"')?;
    let close = usize::from(c.get(a.end) == Some(&'"'));
    let words: String = c[a.start..a.end].iter().collect();
    Some((cut(input, caret - 1, a.end + close, &words), caret - 1))
}

/// A space in a hole would leave the capsule with nothing in it: it is not
/// written. Between quotes it is a letter of the phrase. `None`: typed.
pub fn space(input: &str, caret: usize) -> Option<(String, usize)> {
    let p = pieces(input);
    let (k, j) = hole_at(&p, caret)?;
    (!p[k].alts[j].phrase).then(|| (input.to_string(), caret))
}

/// A quote at a word's start writes both, the caret between them; at a
/// phrase's closing quote it steps over it. `None`: typed as it is.
pub fn quote(input: &str, caret: usize) -> Option<(String, usize)> {
    let c: Vec<char> = input.chars().collect();
    let inside = c[..caret.min(c.len())]
        .iter()
        .filter(|&&x| x == '"')
        .count()
        % 2
        == 1;
    if inside {
        return (c.get(caret) == Some(&'"')).then(|| (input.to_string(), caret + 1));
    }
    let start = caret == 0
        || c[caret - 1].is_whitespace()
        || matches!(c[caret - 1], '!' | ';' | '|' | '(' | '~');
    let end = c.get(caret).is_none_or(|x| x.is_whitespace() || *x == ')');
    (start && end).then(|| (cut(input, caret, caret, "\"\""), caret + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_operator_opens_a_capsule_and_backspace_in_its_hole_takes_it() {
        let b = |t: &str, c: usize| backspace(t, c);
        let s = |t: &str, c: usize| Some((t.to_string(), c));
        assert_eq!(b("!", 1), s("", 0));
        assert_eq!(b("ara !", 5), s("ara ", 4));
        assert_eq!(b("!~", 2), s("", 0));
        assert_eq!(b("~", 1), s("", 0));
        assert_eq!(b("\"\"", 1), s("", 0));
        assert_eq!(b("x !\"\" y", 4), s("x  y", 2));
        assert_eq!(b("ara;", 4), s("ara", 3));
        assert_eq!(b("ara ;", 5), s("ara ", 4));
        assert_eq!(b("a;~", 3), s("a", 1));
        assert_eq!(b(";", 1), s("", 0));
        // Just inside a phrase: its quotes go, its words stay.
        assert_eq!(b("\"ab c\" x", 1), s("ab c x", 0));
        assert_eq!(b("\"ab", 1), s("ab", 0));
        // Anywhere else: an ordinary backspace.
        assert_eq!(b("ara", 3), None);
        assert_eq!(b("!ara", 1), None);
        assert_eq!(b("a;b", 2), None);
        assert_eq!(b("", 0), None);
    }

    #[test]
    fn a_space_in_a_hole_is_not_written_and_quotes_come_in_pairs() {
        let s = |t: &str, c: usize| Some((t.to_string(), c));
        assert_eq!(space("!", 1), s("!", 1));
        assert_eq!(space("ara;", 4), s("ara;", 4));
        assert_eq!(space("\"\"", 1), None);
        assert_eq!(space("ara", 3), None);
        assert_eq!(space("!a", 2), None);
        assert_eq!(quote("", 0), s("\"\"", 1));
        assert_eq!(quote("x !", 3), s("x !\"\"", 4));
        assert_eq!(quote("x", 1), None);
        assert_eq!(quote("\"ab\"", 3), s("\"ab\"", 4));
        assert_eq!(quote("\"ab", 3), None);
        assert_eq!(quote("x y", 2), None);
    }

    #[test]
    fn the_pill_rewrites_the_pattern() {
        assert_eq!(remove("a b c", 1), "a c");
        assert_eq!(remove("a b c", 2), "a b");
        assert_eq!(remove("a", 0), "");
        assert_eq!(toggle_not("a b", 1), "a !b");
        assert_eq!(toggle_not("a !b", 1), "a b");
        assert_eq!(toggle_not("a ! b", 1), "a b");
        assert_eq!(toggle_not("x|y", 0), "!x|y");
        assert_eq!(toggle_not("x ; !y", 0), "x ; y");
        assert_eq!(toggle_similar("rapor", 0, 0), "~rapor");
        assert_eq!(toggle_similar("~rapor", 0, 0), "rapor");
        assert_eq!(toggle_similar("a|b", 0, 1), "a|~b");
        assert_eq!(toggle_similar("!~x", 0, 0), "!x");
        assert_eq!(toggle_similar("\"a b\"", 0, 0), "\"a b\"");
        assert!(similar(&pieces("~a")[0], 0));
    }
}
