//! Where one term ends and the next begins, as the service reads it
//! (`scour_query::parse`'s `tokenize`, `join_lists` and `cut_semicolons`):
//! words split on whitespace and `|` outside quotes, and an `ext:` or `kind:`
//! list that goes on across a `;` with spaces round it.

use super::{Term, canonical, term};

/// Every term with its offsets: split on whitespace and `|` outside quotes.
/// A list is one term across `ext:pdf ; odt`, its `value` without the spaces
/// (`pdf;odt`), and ends before a piece that cannot be a value
/// (`kind:doc;!code` is two). A word of only `;` is a separator; elsewhere a
/// `;` stays in its word, though the service reads it as a space.
pub fn terms(text: &str) -> Vec<Term> {
    let mut out = Vec::new();
    for (s, e) in joined(text, &words(text)) {
        let raw = &text[s..e];
        if raw.chars().all(|c| c == ';') {
            continue;
        }
        let pieces = if is_list(&condensed(raw)) {
            cut(text, s, e)
        } else {
            vec![(s, e)]
        };
        for (a, b) in pieces {
            alternatives(text, a, b, &mut out);
        }
    }
    out
}

/// Byte ranges of the words: whitespace outside quotes cuts.
fn words(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let (mut start, mut quoted) = (None::<usize>, false);
    for (i, c) in text.char_indices() {
        let cut = !quoted && c.is_whitespace();
        if c == '"' {
            quoted = !quoted;
        }
        if cut {
            if let Some(s) = start.take() {
                out.push((s, i));
            }
        } else {
            start.get_or_insert(i);
        }
    }
    if let Some(s) = start {
        out.push((s, text.len()));
    }
    out
}

/// The words with an open list joined to what continues it: `ext:pdf`, `;`
/// and `odt` are one range.
fn joined(text: &str, words: &[(usize, usize)]) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::with_capacity(words.len());
    let mut open = false;
    for &(s, e) in words {
        let t = &text[s..e];
        let after_list = out
            .last()
            .is_some_and(|&(a, b)| openable(&condensed(&text[a..b])));
        match out.last_mut() {
            Some(last) if after_list && can_join(t) && (open || t.starts_with(';')) => last.1 = e,
            _ => out.push((s, e)),
        }
        let (a, b) = out[out.len() - 1];
        let last = condensed(&text[a..b]);
        open = last.ends_with(';') && openable(&last);
    }
    out
}

/// A list range cut where a `;` starts a term of its own; each piece trimmed.
fn cut(text: &str, s: usize, e: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut from = s;
    loop {
        let cur = &text[from..e];
        let at = if is_list(&condensed(cur)) {
            split_list(cur)
        } else {
            unquoted(cur, ';')
        };
        match at {
            Some(i) => {
                out.push(trimmed(text, from, from + i));
                from += i + 1;
            }
            None => {
                out.push(trimmed(text, from, e));
                return out;
            }
        }
    }
}

/// The terms of one piece: cut on `|` outside quotes.
fn alternatives(text: &str, s: usize, e: usize, out: &mut Vec<Term>) {
    let mut from = s;
    let mut quoted = false;
    for (i, c) in text[s..e].char_indices() {
        if c == '"' {
            quoted = !quoted;
        } else if c == '|' && !quoted {
            push(text, from, s + i, out);
            from = s + i + 1;
        }
    }
    push(text, from, e, out);
}

fn push(text: &str, s: usize, e: usize, out: &mut Vec<Term>) {
    let (s, e) = trimmed(text, s, e);
    if s == e {
        return;
    }
    let mut t = term(text, s, e);
    if matches!(t.field, Some("ext" | "kind")) {
        t.value.retain(|c| !c.is_whitespace());
    }
    out.push(t);
}

fn trimmed(text: &str, s: usize, e: usize) -> (usize, usize) {
    let raw = &text[s..e];
    let lead = raw.len() - raw.trim_start().len();
    let s = s + lead;
    (s, s + raw.trim().len())
}

/// The text without the whitespace outside quotes: a joined list as the
/// service holds it.
fn condensed(raw: &str) -> String {
    let mut quoted = false;
    raw.chars()
        .filter(|&c| {
            if c == '"' {
                quoted = !quoted;
            }
            quoted || !c.is_whitespace()
        })
        .collect()
}

fn unquoted(s: &str, mark: char) -> Option<usize> {
    let mut quoted = false;
    s.char_indices().find_map(|(i, c)| {
        if c == '"' {
            quoted = !quoted;
        }
        (c == mark && !quoted).then_some(i)
    })
}

/// Whether a word opens an `ext:` or `kind:` list, one `!` set aside.
fn is_list(word: &str) -> bool {
    if word.starts_with('"') {
        return false;
    }
    let bare = word.strip_prefix('!').unwrap_or(word);
    let Some((name, _)) = bare.split_once(':') else {
        return false;
    };
    name.chars().count() >= 2
        && name.chars().all(char::is_alphabetic)
        && matches!(canonical(name), Some("ext" | "kind"))
}

/// A list no `|` has closed and no stranger has ended: more can follow.
fn openable(word: &str) -> bool {
    is_list(word) && unquoted(word, '|').is_none() && split_list(word).is_none()
}

/// One `;`-piece that can stay a value of the list.
fn is_piece(piece: &str) -> bool {
    let piece = piece.trim();
    if piece.is_empty() {
        return true;
    }
    if piece.starts_with('"') {
        return false;
    }
    let head = piece.split('|').next().unwrap_or("");
    !head.is_empty()
        && !head.starts_with(['!', '"', '(', '<'])
        && !head.contains([':', '/', '*', '?', '!'])
}

/// Whether a word continues an open list across a space.
fn can_join(word: &str) -> bool {
    let rest = word.trim_start_matches(';');
    rest.is_empty() || is_piece(&rest[..unquoted(rest, ';').unwrap_or(rest.len())])
}

/// Where in a list a `;` starts a new term: before the first piece after the
/// first that cannot be a value. Not past a `|`.
fn split_list(raw: &str) -> Option<usize> {
    let (mut quoted, mut from, mut first) = (false, 0, true);
    for (i, c) in raw.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '|' if !quoted => break,
            ';' if !quoted => {
                if !first && !is_piece(&raw[from..i]) {
                    return Some(from - 1);
                }
                first = false;
                from = i + 1;
            }
            _ => {}
        }
    }
    (!first && !is_piece(&raw[from..])).then(|| from - 1)
}

#[cfg(test)]
#[path = "split_tests.rs"]
mod tests;
