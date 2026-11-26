//! What a content capsule's pill does to the pattern: the text after, and
//! where the caret goes. The pattern stays the only model; these rewrite it.

use super::{Piece, PieceKind, pieces};

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
        let bang = piece.ops.first().copied().unwrap_or(piece.start);
        let c: Vec<char> = input.chars().collect();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pill_rewrites_the_pattern() {
        assert_eq!(remove("a b c", 1), "a c");
        assert_eq!(remove("a b c", 2), "a b");
        assert_eq!(remove("a", 0), "");
        assert_eq!(toggle_not("a b", 1), "a !b");
        assert_eq!(toggle_not("a !b", 1), "a b");
        assert_eq!(toggle_not("a ! b", 1), "a b");
        assert_eq!(toggle_not("x|y", 0), "!x|y");
        assert_eq!(toggle_similar("rapor", 0, 0), "~rapor");
        assert_eq!(toggle_similar("~rapor", 0, 0), "rapor");
        assert_eq!(toggle_similar("a|b", 0, 1), "a|~b");
        assert_eq!(toggle_similar("!~x", 0, 0), "!x");
        assert_eq!(toggle_similar("\"a b\"", 0, 0), "\"a b\"");
        assert!(similar(&pieces("~a")[0], 0));
    }
}
