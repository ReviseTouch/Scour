//! What a content capsule's pill, and the keys that write an operator, do to
//! the pattern: the text after, and where the caret goes. The pattern stays
//! the only model; these rewrite it.

use super::{Piece, PieceKind, hole_at, piece_at, pieces};

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

/// A space stays in its capsule: after a letter of a word, the word becomes
/// a phrase holding the space (`osmanlı` → `"osmanlı "`), so the words typed
/// in one capsule are looked for side by side. In a hole it is not written;
/// between quotes it is a letter of the phrase; before a word or between two
/// capsules it is typed. `None`: typed.
pub fn space(input: &str, caret: usize) -> Option<(String, usize)> {
    let p = pieces(input);
    if let Some((k, j)) = hole_at(&p, caret) {
        return (!p[k].alts[j].phrase).then(|| (input.to_string(), caret));
    }
    let a = p
        .iter()
        .flat_map(|p| p.alts.iter())
        .find(|a| !a.phrase && a.start < caret && caret <= a.end)?;
    let c: Vec<char> = input.chars().collect();
    let part = |from: usize, to: usize| c[from..to].iter().collect::<String>();
    let out = format!(
        "{}\"{} {}\"{}",
        part(0, a.start),
        part(a.start, caret),
        part(caret, a.end),
        part(a.end, c.len())
    );
    Some((out, caret + 2))
}

/// Tab leaves the capsule the caret is in for a new one after it: an empty
/// capsule goes, a phrase loses the spaces at its ends — and its quotes when
/// one plain word is left — and the caret lands past a space after it, where
/// the next piece starts. `None`: not in a capsule.
pub fn leave(input: &str, caret: usize) -> Option<(String, usize)> {
    let p = pieces(input);
    let piece = &p[piece_at(&p, caret)?];
    if piece.is_empty() {
        return Some((cut(input, piece.start, piece.end, ""), piece.start));
    }
    let mut c: Vec<char> = input.chars().collect();
    let mut end = piece.end;
    // Last first, so the columns of the ones before still hold.
    for a in piece.alts.iter().rev().filter(|a| a.phrase && !a.is_hole()) {
        let closed = usize::from(c.get(a.end) == Some(&'"'));
        let words: String = c[a.start..a.end]
            .iter()
            .collect::<String>()
            .trim()
            .to_string();
        let plain = !words.is_empty()
            && !words.chars().any(|x| {
                x.is_whitespace() || matches!(x, '"' | ';' | '|' | '!' | '~' | '(' | ')' | '\\')
            });
        let put = if plain || words.is_empty() {
            words
        } else {
            format!("\"{words}\"")
        };
        let (from, to) = (a.start - 1, a.end + closed);
        let put: Vec<char> = put.chars().collect();
        end = end + put.len() - (to - from);
        c.splice(from..to, put);
    }
    let n = c.len();
    let spaced = end < n && c[end].is_whitespace() && (end + 1 == n || c[end + 1].is_whitespace());
    if !spaced {
        c.insert(end, ' ');
    }
    Some((c.into_iter().collect(), end + 1))
}

/// What a paste writes in place of `from..to`, as if it were typed: line
/// breaks and runs of spaces are one space (one kept at either end, none for
/// the line break a copied line ends in), and a space between two words
/// keeps them in one capsule, as a phrase ([`space`]). Text that brings its
/// own operators — a quote, `;`, `|`, a parenthesis, a `!` or `~` before a
/// word — is a pattern already and goes in as it is.
pub fn paste(input: &str, from: usize, to: usize, pasted: &str) -> (String, usize) {
    let pasted = pasted.trim_end_matches(['\r', '\n']);
    let mut words = pasted.split_whitespace().collect::<Vec<_>>().join(" ");
    if !words.is_empty() {
        if pasted.starts_with(char::is_whitespace) {
            words.insert(0, ' ');
        }
        if pasted.ends_with(char::is_whitespace) {
            words.push(' ');
        }
    }
    let (from, to) = (from.min(to), to.max(from));
    let input = cut(input, from, to.min(input.chars().count()), "");
    let operators = words.contains(['"', ';', '|', '(', ')'])
        || words.split(' ').any(|w| w.starts_with(['!', '~']));
    if operators || !words.trim().contains(' ') {
        let at = from + words.chars().count();
        return (cut(&input, from, from, &words), at);
    }
    let (mut text, mut at) = (input, from);
    for ch in words.chars() {
        if ch == ' '
            && let Some((t, a)) = space(&text, at)
        {
            (text, at) = (t, a);
            continue;
        }
        text = cut(&text, at, at, ch.encode_utf8(&mut [0; 4]));
        at += 1;
    }
    (text, at)
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
    fn a_space_keeps_its_capsule_and_tab_leaves_it() {
        let s = |t: &str, c: usize| Some((t.to_string(), c));
        // After a letter: the word becomes a phrase holding the space.
        assert_eq!(space("osmanlı", 7), s("\"osmanlı \"", 9));
        assert_eq!(space("osmanlı", 3), s("\"osm anlı\"", 5));
        assert_eq!(space("!osm", 4), s("!\"osm \"", 6));
        assert_eq!(space("a;osm", 5), s("a;\"osm \"", 7));
        assert_eq!(space("~osm", 4), s("~\"osm \"", 6));
        // Before a word, between capsules, between quotes: typed.
        assert_eq!(space("osm", 0), None);
        assert_eq!(space("ara ", 4), None);
        assert_eq!(space("\"a\"", 2), None);
        // Tab: out of the capsule, past one space, the phrase tidied.
        assert_eq!(leave("osmanlı", 7), s("osmanlı ", 8));
        assert_eq!(leave("osmanlı", 2), s("osmanlı ", 8));
        assert_eq!(leave("\"osmanlı \"", 9), s("osmanlı ", 8));
        assert_eq!(
            leave("\"osmanlı devleti \"", 17),
            s("\"osmanlı devleti\" ", 18)
        );
        assert_eq!(leave("\"dava d", 7), s("\"dava d\" ", 9));
        assert_eq!(leave("!\"osmanlı \"", 10), s("!osmanlı ", 9));
        assert_eq!(leave("x;\"a b \"", 7), s("x;\"a b\" ", 8));
        assert_eq!(leave("\"a;b\"", 4), s("\"a;b\" ", 6));
        assert_eq!(leave("ara !", 5), s("ara ", 4));
        // Between two capsules: room for a new one between them.
        assert_eq!(leave("a b", 1), s("a  b", 2));
        assert_eq!(leave("a  b", 1), s("a  b", 2));
        assert_eq!(leave("ara ", 4), None);
    }

    #[test]
    fn a_paste_is_typed_and_a_pasted_pattern_stays_one() {
        let s = |t: &str, c: usize| (t.to_string(), c);
        // Words: one capsule, a phrase, wherever they land.
        assert_eq!(
            paste("", 0, 0, "osmanlı devleti"),
            s("\"osmanlı devleti\"", 16)
        );
        assert_eq!(
            paste("!", 1, 1, "osmanlı\ndevleti\n"),
            s("!\"osmanlı devleti\"", 17)
        );
        assert_eq!(paste("ara ", 4, 4, "a b"), s("ara \"a b\"", 8));
        assert_eq!(paste("\"x \"", 3, 3, "a b"), s("\"x a b\"", 6));
        // One word: as it is, into the word at the caret.
        assert_eq!(paste("ara", 3, 3, "ba"), s("araba", 5));
        // A pattern: as it is.
        assert_eq!(
            paste("", 0, 0, "\"iş kazası\" !ret"),
            s("\"iş kazası\" !ret", 16)
        );
        assert_eq!(paste("", 0, 0, "a;b c"), s("a;b c", 5));
        // A space at either end is kept, so a pasted piece stands apart.
        assert_eq!(paste("\"a\"", 3, 3, " !b"), s("\"a\" !b", 6));
        assert_eq!(paste("x", 0, 0, "!b "), s("!b x", 3));
        // A selection gives way to it.
        assert_eq!(paste("ara xyz", 4, 7, "a b"), s("ara \"a b\"", 8));
    }

    #[test]
    fn a_space_in_a_hole_is_not_written_and_quotes_come_in_pairs() {
        let s = |t: &str, c: usize| Some((t.to_string(), c));
        assert_eq!(space("!", 1), s("!", 1));
        assert_eq!(space("ara;", 4), s("ara;", 4));
        assert_eq!(space("\"\"", 1), None);
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
