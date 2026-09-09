//! Query text, cut into pieces a search box can colour, so that a term the
//! parser will read as its own text (`kind:zurna`) says so while it is typed.
//!
//! Two rules: a value is coloured usable only if the parser's own value parsers
//! accept it, and the spans cover the input byte for byte, whitespace included.

use scour_core::text::DefaultFolder;
use scour_core::{Completion, CompletionKind, Role, Span};

use crate::fields::{self, FIELDS, KIND_VALUES, TIME_VALUES, Takes};
use crate::time::now_secs;

/// Cut a query into coloured runs, against the current clock.
pub fn spans(input: &str) -> Vec<Span> {
    spans_at(input, now_secs())
}

/// Cut a query into coloured runs against `now`, so `dm:7d` is testable.
pub fn spans_at(input: &str, now: i64) -> Vec<Span> {
    let mut out = Vec::new();
    let toks = tokens(input);
    // A list is a context, not a character: in `ext:rs ; toml` the spaces are
    // separators and the word after one is a value, not a new term.
    let mut list: Option<&'static fields::Field> = None;
    let mut expect_value = false;
    // A `!` on a list field reaches every value in it, past the end of the
    // token: `!ext:rs; toml` excludes both.
    let mut list_not = false;
    // A `!` on its own belongs to the term after it, as `join_operators`
    // binds it: `! main` and `!main` are the same query.
    let mut lone_bang = false;
    for (i, (start, token)) in toks.iter().enumerate() {
        let (start, token) = (*start, *token);
        if is_space(token) {
            // A space beside an `or` is inside the group: `a ;b` and `a; b`
            // are one term with two alternatives, and a lone `!` is looked
            // through because it belongs to the term after it.
            // While a list is open the `;` belongs to it — `join_lists` runs
            // before `join_operators` — but a `|` is an operator either way.
            let beside_alt = next_term_word(&toks, i).is_some_and(|t| t.starts_with('|'))
                || prev_word(&toks, i).is_some_and(|t| t.ends_with('|'));
            // A value continued across the space carries the term's exclusion;
            // two alternatives either side of one do not.
            let list_join = list.is_some()
                && next_word(&toks, i).is_some_and(|t| !t.starts_with('"'))
                && (expect_value || next_word(&toks, i).is_some_and(|t| t.starts_with(';')));
            let joins = beside_alt || list_join;
            let role = if joins { Role::Sep } else { Role::Space };
            let span = Span::new(start, token.len(), role);
            // The gap between a lone `!` and its term is inside the exclusion.
            let inside = (list_join && list_not) || lone_bang;
            out.push(if inside { span.excluded() } else { span });
            if !list_join {
                list = None;
                expect_value = false;
                list_not = false;
            }
            continue;
        }
        // A quoted run is never a continuation; it closes the list behind it.
        if let Some(f) = list
            && !token.starts_with('"')
            && (expect_value || token.starts_with(';'))
        {
            let mark = out.len();
            let mut at = start;
            let mut rest = token;
            if let Some(r) = rest.strip_prefix(';') {
                out.push(Span::new(at, 1, Role::Sep));
                at += 1;
                rest = r;
                // A separator alone means its value is not written yet.
                expect_value = true;
            }
            // A `|` ends the value and starts an alternative: `ext:rs ; i|j`
            // is a two-extension filter or the word `j`.
            let mut pieces = crate::parse::split_outside_quotes(rest, '|').into_iter();
            let first = pieces.next().unwrap_or("");
            if !first.is_empty() {
                expect_value = first.ends_with(';');
            }
            value_spans(&mut out, at, first, f, now);
            if list_not {
                for span in &mut out[mark..] {
                    span.not = true;
                }
            }
            at += first.len();
            for piece in pieces {
                out.push(Span::new(at, 1, Role::Or));
                at += 1;
                one_term(&mut out, at, piece, now);
                at += piece.len();
                // Past the `|` the list is over.
                list = None;
                expect_value = false;
                list_not = false;
            }
            continue;
        }
        let mark = out.len();
        list_not = term(&mut out, start, token, now) | lone_bang;
        // Only the last term in a token continues across the space.
        let tail = *bang_pieces(token).last().unwrap_or(&token);
        // The first alternative of the first term and no further: `! d;e` is
        // `!d e`, two terms with only the first excluded.
        let had_bang = std::mem::take(&mut lone_bang);
        if had_bang {
            // Past a leading separator (`! ;c` is `!c`), stopping at the next.
            let mut reached = false;
            for span in &mut out[mark..] {
                if span.role == Role::Or {
                    break;
                }
                if matches!(span.role, Role::Space | Role::Sep) {
                    if reached {
                        break;
                    }
                } else {
                    reached = true;
                }
                span.not = true;
            }
        }
        // A token of nothing but separators does not use up a pending `!`:
        // `! ; a` excludes `a`.
        lone_bang = token == "!" || (had_bang && token.chars().all(|c| c == ';'));
        list = field_of(tail);
        expect_value = list.is_some() && tail.ends_with(';');
    }
    absorb_separators(&mut out, input);
    out
}

/// A `;` beside a `|` says nothing the `|` has not: in `a|;c` the parser keeps
/// the operator and drops the separator, so the line must draw one term. A pass
/// over the finished spans, because "beside" reaches across tokens.
fn absorb_separators(spans: &mut [Span], query: &str) {
    let speaks = |s: &Span| !matches!(s.role, Role::Space | Role::Sep) || s.of(query) == ";";
    for i in 0..spans.len() {
        if spans[i].role != Role::Space || spans[i].of(query) != ";" {
            continue;
        }
        let before = spans[..i].iter().rev().find(|s| speaks(s)).map(|s| s.role);
        let after = spans[i + 1..].iter().find(|s| speaks(s)).map(|s| s.role);
        if before == Some(Role::Or) || after == Some(Role::Or) {
            spans[i].role = Role::Sep;
        }
    }
}

/// The same query with every term on one of `names` taken out of it.
///
/// A rail cannot filter itself out of existence: the kind list and the age
/// strip are counted over the query *without* the term they set, or every other
/// bar reads zero. `None` when nothing was taken out, so a caller can ask one
/// question instead of two. Names are the canonical ones from
/// [`FIELDS`](crate::FIELDS), so `"kind"` takes `tür:` too.
///
/// ```
/// # use scour_query::without;
/// assert_eq!(without("rapor kind:code", &["kind"]).as_deref(), Some("rapor"));
/// assert_eq!(without("rapor tür:code", &["kind"]).as_deref(), Some("rapor"));
/// assert_eq!(without("rapor", &["kind"]), None);
/// ```
pub fn without(input: &str, names: &[&str]) -> Option<String> {
    without_at(input, names, now_secs())
}

/// The same, against `now`: `dm:7d` is a term only if the clock says it parses.
pub fn without_at(input: &str, names: &[&str], now: i64) -> Option<String> {
    let toks = tokens(input);
    let mut kept: Vec<&str> = Vec::with_capacity(toks.len());
    for (_, token) in &toks {
        if is_space(token) || !drops(token, names, now) {
            kept.push(token);
        }
    }
    // By count, not by text: dropping a term leaves the space beside it, and
    // a query differing only by a double space costs a second walk.
    if kept.len() == toks.len() {
        return None;
    }
    Some(kept.concat().trim().to_string())
}

/// Is this one token a *usable* term on one of `names`? `kind:zurna` is
/// searched for as text, so dropping it would change the question, not widen it.
fn drops(token: &str, names: &[&str], now: i64) -> bool {
    if token.starts_with('"') {
        return false;
    }
    let bare = token.strip_prefix('!').unwrap_or(token);
    let Some((name, value)) = field_split(bare) else {
        return false;
    };
    let Some(f) = fields::lookup(&DefaultFolder::of(name)) else {
        return false;
    };
    names.contains(&f.name) && fields::accepts(f, &DefaultFolder::of(&value.replace('"', "")), now)
}

/// The field a token filters on, if it is a field term at all.
fn field_of(token: &str) -> Option<&'static fields::Field> {
    if token.starts_with('"') {
        return None;
    }
    let t = token.strip_prefix('!').unwrap_or(token);
    let (name, _) = field_split(t)?;
    fields::lookup(&DefaultFolder::of(name))
}

/// The next token that is not whitespace.
fn next_word<'a>(toks: &[(usize, &'a str)], i: usize) -> Option<&'a str> {
    toks[i + 1..]
        .iter()
        .find(|(_, t)| !is_space(t))
        .map(|(_, t)| *t)
}

/// The next token that is not whitespace and not a lone `!`.
fn next_term_word<'a>(toks: &[(usize, &'a str)], i: usize) -> Option<&'a str> {
    toks[i + 1..]
        .iter()
        .find(|(_, t)| !is_space(t) && *t != "!")
        .map(|(_, t)| *t)
}

/// The previous token that is not whitespace.
fn prev_word<'a>(toks: &[(usize, &'a str)], i: usize) -> Option<&'a str> {
    toks[..i]
        .iter()
        .rev()
        .find(|(_, t)| !is_space(t))
        .map(|(_, t)| *t)
}

fn is_space(t: &str) -> bool {
    t.chars().all(char::is_whitespace)
}

/// Split into terms and the whitespace between them, keeping quoted runs whole
/// and keeping every byte.
fn tokens(input: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < input.len() {
        let start = i;
        let space = bytes[i].is_ascii_whitespace();
        let mut in_quotes = false;
        while i < input.len() {
            let c = bytes[i];
            // A run of whitespace ends at the first character that is not
            // whitespace, quote or no quote: a quote toggled inside a space
            // run swallows the phrase after it.
            if space {
                if !c.is_ascii_whitespace() {
                    break;
                }
            } else {
                if c == b'"' {
                    in_quotes = !in_quotes;
                } else if !in_quotes && c.is_ascii_whitespace() {
                    break;
                }
            }
            // Multi-byte characters are never whitespace or a quote, so a
            // one-byte step can only land inside a run already being consumed.
            i += 1;
        }
        out.push((start, &input[start..i]));
    }
    out
}

/// One whitespace-separated term, which may itself hold alternatives. Answers
/// whether it is excluded — for a list field that travels past the token's end.
fn term(out: &mut Vec<Span>, start: usize, text: &str, now: i64) -> bool {
    // A lone operator is an operator; `hello!` is a word, as for the parser.
    if text == "|" {
        out.push(Span::new(start, 1, Role::Or));
        return false;
    }
    // A `;` between terms wears the role a space does: `hasan;genel` wants
    // both words, exactly as `hasan genel` does.
    if text == ";" {
        out.push(Span::new(start, 1, Role::Space));
        return false;
    }
    if text == "!" {
        out.push(Span::new(start, 1, Role::Not).excluded());
        return true;
    }

    // A `;` or a `!` inside a word starts a new term, the same cuts
    // `split_semicolons` and `split_bangs` make.
    let mut at = start;
    let mut negated = false;
    for (piece, cut) in term_pieces(text) {
        if cut {
            out.push(Span::new(at, 1, Role::Space));
            at += 1;
        }
        negated |= one_term(out, at, piece, now);
        at += piece.len();
    }
    negated
}

/// One term: alternatives separated by `|`, each with an optional `!`.
fn one_term(out: &mut Vec<Span>, start: usize, text: &str, now: i64) -> bool {
    let mut at = start;
    let mut negated = false;
    // A line that hides an `or` hides the reason its answer is enormous.
    for (alt, sep) in alternatives(text) {
        if let Some(c) = sep {
            out.push(Span::new(at, c.len_utf8(), Role::Or));
            at += c.len_utf8();
        }
        negated |= alternative(out, at, alt, now);
        at += alt.len();
    }
    negated
}

/// The terms inside one token, and whether each was cut off the one before by a
/// `;`. Mirrors `parse::split_semicolons` and `parse::split_bangs`: the `;` is
/// consumed, a `!` stays at the head of the piece it negates.
fn term_pieces(text: &str) -> Vec<(&str, bool)> {
    if text.starts_with('"') || field_of(text).is_some() {
        return vec![(text, false)];
    }
    let mut out = Vec::new();
    for (n, part) in crate::parse::split_outside_quotes(text, ';')
        .into_iter()
        .enumerate()
    {
        let mut first = true;
        for piece in bang_pieces(part) {
            out.push((piece, first && n > 0));
            first = false;
        }
    }
    out
}

/// One term cut at every `!` that starts a new one inside it. The pieces are
/// contiguous and cover the text; the `!` stays at the head of its piece.
fn bang_pieces(text: &str) -> Vec<&str> {
    if text.is_empty() || text.starts_with('"') || field_of(text).is_some() {
        return vec![text];
    }
    let mut out = Vec::new();
    let mut from = 0;
    for (i, c) in text.char_indices() {
        if c != '!' || i == 0 || i + 1 == text.len() {
            continue;
        }
        let before = text[..i].chars().next_back();
        if before == Some('|') {
            continue;
        }
        if i > from {
            out.push(&text[from..i]);
        }
        from = i;
    }
    out.push(&text[from..]);
    out
}

/// The alternatives in a term, each with the character that separated it from
/// the one before. Only `|` makes one; `;` has already separated terms.
fn alternatives(text: &str) -> Vec<(&str, Option<char>)> {
    let mut out = Vec::new();
    let mut sep = None;
    // The split is the parser's own, so the two cannot cut in different places.
    for part in crate::parse::split_outside_quotes(text, '|') {
        out.push((part, sep));
        sep = Some('|');
    }
    out
}

/// One alternative: an optional `!`, then a field term or plain text. The `!`
/// marks the whole alternative, so every run it produces carries the mark, and
/// the answer travels out because a list field runs past the spaces after it.
fn alternative(out: &mut Vec<Span>, start: usize, text: &str, now: i64) -> bool {
    let mark = out.len();
    let negated = text.starts_with('!');
    alternative_spans(out, start, text, now);
    if negated {
        for span in &mut out[mark..] {
            span.not = true;
        }
    }
    negated
}

fn alternative_spans(out: &mut Vec<Span>, start: usize, text: &str, now: i64) {
    let mut at = start;
    let mut rest = text;
    if let Some(r) = rest.strip_prefix('!') {
        out.push(Span::new(at, 1, Role::Not));
        at += 1;
        rest = r;
    }
    if rest.is_empty() {
        return;
    }

    match field_split(rest) {
        Some((name, value)) => {
            let folded = DefaultFolder::of(name);
            match fields::lookup(&folded) {
                Some(f) => {
                    out.push(Span::new(at, name.len(), Role::Field));
                    out.push(Span::new(at + name.len(), 1, Role::Colon));
                    value_spans(out, at + name.len() + 1, value, f, now);
                }
                // Letters and a colon but not a field: the whole term is text.
                None => out.push(Span::new(at, rest.len(), Role::UnknownField)),
            }
        }
        None => plain(out, at, rest),
    }
}

/// The value after a field's colon.
fn value_spans(out: &mut Vec<Span>, start: usize, value: &str, f: &fields::Field, now: i64) {
    if value.is_empty() {
        // `folder:` is complete; `ext:` is unfinished, not yet wrong.
        return;
    }
    let folded = DefaultFolder::of(&value.replace('"', ""));
    if !fields::accepts(f, &folded, now) {
        out.push(Span::new(start, value.len(), Role::BadValue));
        return;
    }

    let mut at = start;
    let mut rest = value;
    // A comparison is part of the syntax, not part of the value.
    if matches!(f.takes, Takes::Size | Takes::Time) {
        let op_len = ["<=", ">=", "<", ">", "="]
            .into_iter()
            .find(|p| rest.starts_with(p))
            .map(str::len)
            .unwrap_or(0);
        if op_len > 0 {
            out.push(Span::new(at, op_len, Role::Cmp));
            at += op_len;
            rest = &rest[op_len..];
        }
    }
    // A list field's separators are syntax too.
    if f.takes == Takes::Ext {
        for (i, part) in rest.split(';').enumerate() {
            if i > 0 {
                out.push(Span::new(at, 1, Role::Sep));
                at += 1;
            }
            if !part.is_empty() {
                out.push(Span::new(at, part.len(), Role::Value));
                at += part.len();
            }
        }
        return;
    }
    if !rest.is_empty() {
        out.push(Span::new(at, rest.len(), Role::Value));
    }
}

/// Plain text, which may be quoted or contain wildcards.
fn plain(out: &mut Vec<Span>, start: usize, text: &str) {
    if text.starts_with('"') {
        let mut at = start;
        out.push(Span::new(at, 1, Role::Quote));
        at += 1;
        let inner_end = if text.len() > 1 && text.ends_with('"') {
            text.len() - 1
        } else {
            text.len()
        };
        if inner_end > 1 {
            out.push(Span::new(at, inner_end - 1, Role::Phrase));
            at += inner_end - 1;
        }
        if inner_end < text.len() {
            out.push(Span::new(at, 1, Role::Quote));
        }
        return;
    }
    let role = if text.contains('*') || text.contains('?') {
        Role::Glob
    } else {
        Role::Text
    };
    out.push(Span::new(start, text.len(), role));
}

/// `field:value` by the parser's rule — two or more letters and nothing else —
/// exactly, or every Windows path is painted as a mistake.
fn field_split(s: &str) -> Option<(&str, &str)> {
    let idx = s.find(':')?;
    let name = &s[..idx];
    if name.chars().count() < 2 || !name.chars().all(char::is_alphabetic) {
        return None;
    }
    Some((name, &s[idx + 1..]))
}

// ───────────────────────────── completions ─────────────────────────────

/// What could be typed at `cursor`: always the word the cursor is in, always
/// from the field table.
pub fn complete(input: &str, cursor: usize) -> Vec<Completion> {
    let cursor = caret(input, cursor);
    let word = word_at(input, cursor);
    let typed = &input[word.0..cursor];
    let stripped = typed.strip_prefix('!').unwrap_or(typed);

    // Inside a field's value: offer values, not fields.
    if let Some((name, value)) = field_split(stripped) {
        let folded = DefaultFolder::of(name);
        if let Some(f) = fields::lookup(&folded) {
            return values_for(f, name, value);
        }
        return Vec::new();
    }

    let prefix = DefaultFolder::of(stripped);
    FIELDS
        .iter()
        .filter(|f| f.name.starts_with(&prefix) || f.aliases.iter().any(|a| a.starts_with(&prefix)))
        .map(|f| Completion {
            insert: format!("{}:", f.name),
            label: if f.example.is_empty() {
                format!("{}:", f.name)
            } else {
                f.example.to_owned()
            },
            about: f.about.to_owned(),
            kind: CompletionKind::Field,
        })
        .collect()
}

/// The values worth offering for one field.
fn values_for(f: &fields::Field, written_name: &str, typed: &str) -> Vec<Completion> {
    let prefix = DefaultFolder::of(typed);
    let list: &[&str] = match f.takes {
        Takes::Kind => KIND_VALUES,
        Takes::Time => TIME_VALUES,
        _ => return Vec::new(),
    };
    list.iter()
        .filter(|v| v.starts_with(&prefix))
        .map(|v| Completion {
            insert: format!("{written_name}:{v}"),
            label: format!("{}:{v}", f.name),
            about: f.about.to_owned(),
            kind: CompletionKind::Value,
        })
        .collect()
}

/// The caret, moved back to a boundary this string actually has: a browser's
/// `selectionStart` counts UTF-16 code units, and slicing mid-character panics.
fn caret(input: &str, cursor: usize) -> usize {
    let mut at = cursor.min(input.len());
    while !input.is_char_boundary(at) {
        at -= 1;
    }
    at
}

/// The whitespace-delimited word the cursor sits in, as byte offsets.
fn word_at(input: &str, cursor: usize) -> (usize, usize) {
    let bytes = input.as_bytes();
    let mut start = cursor;
    while start > 0 && !bytes[start - 1].is_ascii_whitespace() {
        start -= 1;
    }
    let mut end = cursor;
    while end < input.len() && !bytes[end].is_ascii_whitespace() {
        end += 1;
    }
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roles(q: &str) -> Vec<(Role, &str)> {
        spans_at(q, 1_800_000_000)
            .into_iter()
            .map(|s| (s.role, s.of(q)))
            .collect()
    }

    /// The invariant: put the spans back together and the query comes back.
    fn covers(q: &str) {
        let spans = spans_at(q, 1_800_000_000);
        let mut at = 0;
        let mut rebuilt = String::new();
        for s in &spans {
            assert_eq!(s.start as usize, at, "gap or overlap in {q:?}: {spans:?}");
            rebuilt.push_str(s.of(q));
            at += s.len as usize;
        }
        assert_eq!(at, q.len(), "spans stop short of the end of {q:?}");
        assert_eq!(rebuilt, q);
    }

    #[test]
    fn plain_words_and_the_space_between_them() {
        assert_eq!(
            roles("rapor pdf"),
            vec![
                (Role::Text, "rapor"),
                (Role::Space, " "),
                (Role::Text, "pdf")
            ]
        );
        covers("rapor pdf");
        covers("  leading and trailing   ");
    }

    #[test]
    fn a_field_is_three_pieces() {
        assert_eq!(
            roles("ext:pdf"),
            vec![
                (Role::Field, "ext"),
                (Role::Colon, ":"),
                (Role::Value, "pdf")
            ]
        );
        covers("ext:pdf");
    }

    #[test]
    fn a_value_the_field_cannot_read_is_marked() {
        // The case the module is for: the parser searches for these as text.
        assert_eq!(
            roles("kind:zurna"),
            vec![
                (Role::Field, "kind"),
                (Role::Colon, ":"),
                (Role::BadValue, "zurna")
            ]
        );
        assert_eq!(
            roles("size:abc"),
            vec![
                (Role::Field, "size"),
                (Role::Colon, ":"),
                (Role::BadValue, "abc")
            ]
        );
        assert_eq!(
            roles("dm:yarin"),
            vec![
                (Role::Field, "dm"),
                (Role::Colon, ":"),
                (Role::BadValue, "yarin")
            ]
        );
        covers("kind:zurna");
    }

    #[test]
    fn a_field_that_does_not_exist_is_the_whole_term() {
        assert_eq!(roles("boyut:1mb"), vec![(Role::UnknownField, "boyut:1mb")]);
        covers("boyut:1mb");
    }

    #[test]
    fn a_field_name_is_case_insensitive_and_coloured_as_one() {
        // Field names are folded, so this is the size field, not a mistake.
        assert_eq!(
            roles("sizE:>1mb"),
            vec![
                (Role::Field, "sizE"),
                (Role::Colon, ":"),
                (Role::Cmp, ">"),
                (Role::Value, "1mb")
            ]
        );
    }

    #[test]
    fn windows_paths_and_urls_are_not_fields() {
        // One letter is not a field name, so this is ordinary text.
        assert_eq!(roles("C:/Users"), vec![(Role::Text, "C:/Users")]);
        assert_eq!(
            roles("http://example"),
            vec![(Role::UnknownField, "http://example")]
        );
    }

    #[test]
    fn comparisons_and_separators_are_syntax() {
        assert_eq!(
            roles("size:>1mb"),
            vec![
                (Role::Field, "size"),
                (Role::Colon, ":"),
                (Role::Cmp, ">"),
                (Role::Value, "1mb")
            ]
        );
        assert_eq!(
            roles("ext:rs;toml"),
            vec![
                (Role::Field, "ext"),
                (Role::Colon, ":"),
                (Role::Value, "rs"),
                (Role::Sep, ";"),
                (Role::Value, "toml")
            ]
        );
        covers("ext:rs;toml;md");
    }

    #[test]
    fn negation_glob_and_phrase() {
        assert_eq!(roles("!tmp"), vec![(Role::Not, "!"), (Role::Text, "tmp")]);
        assert_eq!(roles("*.rs"), vec![(Role::Glob, "*.rs")]);
        assert_eq!(
            roles("\"iki kelime\""),
            vec![
                (Role::Quote, "\""),
                (Role::Phrase, "iki kelime"),
                (Role::Quote, "\"")
            ]
        );
        covers("\"iki kelime\"");
        // A quote that is still open must still cover the input.
        covers("\"yarim");
    }

    #[test]
    fn a_lone_bang_or_pipe_is_an_operator_but_an_exclaiming_word_is_not() {
        assert_eq!(
            roles("! main"),
            vec![(Role::Not, "!"), (Role::Space, " "), (Role::Text, "main")]
        );
        assert_eq!(roles("hello!"), vec![(Role::Text, "hello!")]);
        assert_eq!(
            roles("a|b"),
            vec![(Role::Text, "a"), (Role::Or, "|"), (Role::Text, "b")]
        );
        covers("a | b");
    }

    /// The `!` is not the exclusion — the term after it is.
    #[test]
    fn an_excluded_term_is_excluded_all_the_way_through() {
        fn excluded(q: &str) -> Vec<&str> {
            spans_at(q, 1_800_000_000)
                .into_iter()
                .filter(|s| s.not)
                .map(|s| s.of(q))
                .collect()
        }
        assert_eq!(excluded("!tmp"), vec!["!", "tmp"]);
        assert_eq!(excluded("rapor !tmp"), vec!["!", "tmp"]);
        // A field term: the name, the colon and the value are all inside it.
        assert_eq!(excluded("!ext:pdf"), vec!["!", "ext", ":", "pdf"]);
        // A list carries it past the token it started in.
        assert_eq!(
            excluded("!ext:rs; toml"),
            vec!["!", "ext", ":", "rs", ";", " ", "toml"]
        );
        // One alternative of a group, not the group.
        assert_eq!(excluded("a|!b"), vec!["!", "b"]);
        // A `!` on its own binds to the term after it: `! main` is `!main`.
        assert_eq!(excluded("! main"), vec!["!", " ", "main"]);
        assert_eq!(
            excluded("rapor ! ext:pdf"),
            vec!["!", " ", "ext", ":", "pdf"]
        );
        // A `!` with nothing after it is still only itself.
        assert_eq!(excluded("rapor !"), vec!["!"]);
        // And nothing at all when nothing is excluded.
        assert!(excluded("ext:rs rapor").is_empty());
        assert!(excluded("hello!").is_empty());
    }

    /// Marking the extent must not move a byte of it.
    #[test]
    fn exclusion_does_not_disturb_the_roles_or_the_cover() {
        assert_eq!(
            roles("!ext:pdf"),
            vec![
                (Role::Not, "!"),
                (Role::Field, "ext"),
                (Role::Colon, ":"),
                (Role::Value, "pdf")
            ]
        );
        covers("!ext:rs; toml");
        covers("a|!b !kind:zurna");
    }

    /// `;` between words separates two terms, and is drawn the way a space is.
    #[test]
    fn a_semicolon_between_words_is_a_separator() {
        assert_eq!(
            roles("HASAN;DENEME"),
            vec![
                (Role::Text, "HASAN"),
                (Role::Space, ";"),
                (Role::Text, "DENEME")
            ]
        );
        covers("HASAN;DENEME");
        assert_eq!(
            roles("a ;b"),
            vec![
                (Role::Text, "a"),
                (Role::Space, " "),
                (Role::Space, ";"),
                (Role::Text, "b")
            ]
        );
        covers("a ;b");
        covers("a ; b");
        // And `|` is still the operator it always was.
        assert_eq!(
            roles("a|b"),
            vec![(Role::Text, "a"), (Role::Or, "|"), (Role::Text, "b")]
        );
    }

    /// And a field's own list keeps its semicolons.
    #[test]
    fn a_semicolon_inside_a_list_is_still_a_separator_not_an_operator() {
        assert_eq!(
            roles("ext:rs;toml"),
            vec![
                (Role::Field, "ext"),
                (Role::Colon, ":"),
                (Role::Value, "rs"),
                (Role::Sep, ";"),
                (Role::Value, "toml")
            ]
        );
        assert_eq!(
            roles("!ext:rs;toml"),
            vec![
                (Role::Not, "!"),
                (Role::Field, "ext"),
                (Role::Colon, ":"),
                (Role::Value, "rs"),
                (Role::Sep, ";"),
                (Role::Value, "toml")
            ]
        );
        covers("!ext:rs;toml");
        // A quoted run is literal all the way through.
        assert_eq!(
            roles("\"a;b\""),
            vec![
                (Role::Quote, "\""),
                (Role::Phrase, "a;b"),
                (Role::Quote, "\"")
            ]
        );
    }

    /// The colouring and the parser read the same query: the same AND-ed terms,
    /// alternatives and exclusions. No rewritten spellings in the corpus —
    /// `expand` turns one token into several terms, which the line cannot show.
    #[test]
    fn the_colouring_and_the_parser_read_the_same_query() {
        const NOW: i64 = 1_800_000_000;
        for q in [
            "",
            "rapor",
            "rapor pdf",
            "  leading and trailing   ",
            "!tmp",
            "! main",
            "rapor !tmp",
            "rapor!tmp",
            "a!b!c",
            "hello!",
            "a!",
            "a|b",
            "a|!b",
            "!a|b",
            "a;b",
            "HASAN;DENEME",
            "hasan;genel",
            "a ;b",
            "a; b",
            "a ; b",
            "a;b;c",
            "a;!b",
            "a|b;c",
            "a;b|c",
            "a|;c",
            "; a",
            "! ; a",
            "!ama ;deneme",
            "HASAN;DENEME !ama ;deneme !dfd",
            "ext:pdf",
            "!ext:pdf",
            "ext:rs;toml",
            "!ext:rs;toml",
            "ext:rs; toml",
            "kind:image rapor",
            "kind:zurna",
            "!kind:zurna",
            "sizE:>1mb",
            "\"iki kelime\"",
            "rapor \"iki kelime\"",
            "\"a!b\"",
            "\"a;b\"",
            "*.rs !cache",
            "değiştirme !öğe",
            "path:/home/a!b",
            "under:/home rapor !tmp",
        ] {
            agree(q, NOW);
            covers(q);
        }
    }

    /// The same agreement, over every query a handful of pieces can build.
    #[test]
    fn the_colouring_and_the_parser_agree_on_everything_these_pieces_can_spell() {
        const NOW: i64 = 1_800_000_000;
        const PIECES: &[&str] = &[
            "a",
            "!b",
            ";c",
            "d;e",
            "f!g",
            "h!",
            "!",
            ";",
            "|",
            "i|j",
            "!k|l",
            "ext:rs",
            "!ext:rs;toml",
            "\"x y\"",
            "*.m",
            "kind:zurna",
        ];
        let mut checked = 0;
        for one in PIECES {
            for two in PIECES {
                for three in PIECES {
                    let q = format!("{one} {two} {three}");
                    // One shape no colouring can draw truthfully: a lone `!`
                    // before a term opening with `|` binds past the operator
                    // (`a ! |c` is `a` or `not c`), so it is written on the
                    // wrong side of the `or`.
                    let toks: Vec<&str> = q.split_whitespace().collect();
                    let looks_past = toks
                        .windows(2)
                        .any(|w| w[0] == "!" && w[1].starts_with('|'));
                    if looks_past {
                        continue;
                    }
                    agree(&q, NOW);
                    checked += 1;
                }
            }
        }
        assert!(checked > 3500, "the sweep did not run");
    }

    /// The line and the engine read `q` the same way, or say how they differ.
    /// Structure comes off the spans as a person reads them: spaces separate
    /// terms, an `or` joins alternatives, a `!` begins one unless it opens one.
    fn agree(q: &str, now: i64) {
        let ast = crate::parse::parse_at(q, now);
        let spans = spans_at(q, now);
        let gap = |s: &Span| s.role == Role::Space && !s.not;
        let mut groups: Vec<Vec<Span>> = Vec::new();
        let mut cur: Vec<Span> = Vec::new();
        let mut prev: Option<Role> = None;
        for s in &spans {
            let starts_term = s.role == Role::Not
                && !cur.is_empty()
                && prev != Some(Role::Or)
                && prev != Some(Role::Sep)
                && prev != Some(Role::Space);
            if (gap(s) || starts_term) && !cur.is_empty() {
                groups.push(std::mem::take(&mut cur));
            }
            if !gap(s) {
                cur.push(*s);
            }
            prev = Some(s.role);
        }
        if !cur.is_empty() {
            groups.push(cur);
        }
        // A term made only of operators has nothing to search for: the parser
        // drops it, and the line draws it because someone is mid-word.
        groups.retain(|g| {
            !g.iter()
                .all(|s| matches!(s.role, Role::Not | Role::Or | Role::Space | Role::Sep))
        });
        // A `!` at the end of a term excludes nothing: `a ; !` is one
        // alternative, not two with an empty second.
        for g in &mut groups {
            while g
                .last()
                .is_some_and(|s| matches!(s.role, Role::Not | Role::Space | Role::Sep))
            {
                g.pop();
            }
        }
        assert_eq!(
            groups.len(),
            ast.groups.len(),
            "{q:?}: the line shows {} terms and the engine reads {}",
            groups.len(),
            ast.groups.len()
        );
        for (i, (drawn, read)) in groups.iter().zip(&ast.groups).enumerate() {
            // An `or` with nothing after it, or right after another, joins
            // nothing: `a ; ;c` reads as `a` or `c`.
            let mut alts = 1;
            let mut last: Option<Role> = None;
            for s in drawn {
                if matches!(s.role, Role::Space | Role::Sep) {
                    continue;
                }
                if s.role == Role::Or && last.is_some_and(|r| r != Role::Or) {
                    alts += 1;
                }
                last = Some(s.role);
            }
            let dangling = last == Some(Role::Or);
            alts -= usize::from(dangling);
            assert_eq!(
                alts,
                read.alts.len(),
                "{q:?}: term {i} is drawn with {alts} alternatives and read with {}",
                read.alts.len()
            );
            // Cut the drawn run at its `or`s; each piece must carry the mark.
            if dangling {
                continue;
            }
            let mut drawn_neg = Vec::new();
            let mut any = false;
            let mut last: Option<Role> = None;
            for s in drawn {
                if matches!(s.role, Role::Space | Role::Sep) {
                    any |= s.not;
                    continue;
                }
                if s.role == Role::Or {
                    // Only an `or` with something in front starts a new one.
                    if last.is_some_and(|r| r != Role::Or) {
                        drawn_neg.push(any);
                        any = false;
                    }
                } else {
                    any |= s.not;
                }
                last = Some(s.role);
            }
            drawn_neg.push(any);
            let read_neg: Vec<bool> = read.alts.iter().map(|(n, _)| *n).collect();
            assert_eq!(
                drawn_neg, read_neg,
                "{q:?}: term {i} — the line marks {drawn_neg:?} excluded, \
                 the engine excludes {read_neg:?}"
            );
        }
        // Byte for byte, whatever else happened.
        let mut at = 0;
        for s in &spans {
            assert_eq!(s.start as usize, at, "gap or overlap in {q:?}");
            at += s.len as usize;
        }
        assert_eq!(at, q.len(), "spans stop short of the end of {q:?}");
    }

    #[test]
    fn an_unfinished_field_is_not_yet_a_mistake() {
        // Someone is mid-word: a warning here fires on the way to every field.
        assert_eq!(
            roles("ext:"),
            vec![(Role::Field, "ext"), (Role::Colon, ":")]
        );
        assert_eq!(
            roles("folder:"),
            vec![(Role::Field, "folder"), (Role::Colon, ":")]
        );
    }

    #[test]
    fn turkish_field_names_and_values_are_recognised() {
        assert_eq!(
            roles("tür:kod"),
            vec![
                (Role::Field, "tür"),
                (Role::Colon, ":"),
                (Role::Value, "kod")
            ]
        );
        covers("tür:kod içerik:gizli");
        assert_eq!(
            roles("TÜR:İMAGE"),
            vec![
                (Role::Field, "TÜR"),
                (Role::Colon, ":"),
                (Role::Value, "İMAGE")
            ],
            "folding is the parser's, so the spans stay on the original bytes"
        );
    }

    #[test]
    fn every_span_lands_on_a_character_boundary() {
        for q in [
            "İSTANBUL",
            "tür:görsel",
            "\"çağlayan iş\"",
            "ıq ext:pdf",
            "içerik:ğ",
        ] {
            covers(q);
            for s in spans_at(q, 0) {
                assert!(
                    q.is_char_boundary(s.start as usize)
                        && q.is_char_boundary((s.start + s.len) as usize),
                    "{q:?} span {s:?} splits a character"
                );
            }
        }
    }

    #[test]
    fn completions_offer_fields_by_prefix() {
        // Offered, not offered alone: `exe:` shares the first two letters.
        let c = complete("ex", 2);
        assert!(c.iter().any(|x| x.insert == "ext:"), "{c:?}");
        assert!(c.iter().all(|x| x.insert.starts_with("ex")), "{c:?}");
        assert!(
            complete("", 0).len() >= 10,
            "everything, when nothing typed"
        );
        // An alias matches too, and inserts the canonical spelling.
        let c = complete("tür", 4);
        assert_eq!(c[0].insert, "kind:");
    }

    #[test]
    fn completions_offer_values_once_the_field_is_typed() {
        let c = complete("kind:i", 6);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].insert, "kind:image");
        // The spelling the user chose is kept.
        let c = complete("tür:i", 6);
        assert_eq!(c[0].insert, "tür:image");
    }

    #[test]
    fn completions_are_for_the_word_under_the_cursor() {
        let c = complete("rapor ex", 8);
        assert!(
            c.iter().any(|x| x.insert == "ext:") && c.iter().all(|x| x.insert.starts_with("ex")),
            "the earlier word is not what is being typed: {c:?}"
        );
        assert!(
            complete("rapor ex", 5).iter().all(|c| c.insert != "ext:"),
            "a cursor inside the first word does not complete the second"
        );
    }

    #[test]
    fn a_caret_inside_a_character_does_not_take_the_connection_down() {
        // A browser counts `İ` as one code unit where this counts two bytes.
        // Every offset up to past the end must answer rather than panic.
        for input in ["İ", "kİ", "ad:İ", "rapor İş", "TRABZON.MÜZEKKERE"] {
            for cursor in 0..=input.len() + 4 {
                let _ = complete(input, cursor);
            }
        }

        // It lands on the character the caret is inside: `ad:İ` is 5 bytes,
        // so a browser's 4 is mid-`İ` and still means the word so far.
        assert_eq!(complete("ad:İ", 4), complete("ad:İ", 3));
    }

    #[test]
    fn every_offered_completion_parses_as_what_it_claims() {
        // Any offered field, filled in as its own example does, parses as
        // that field rather than as plain text.
        for f in FIELDS {
            let query = if f.example.is_empty() {
                format!("{}:", f.name)
            } else {
                f.example.to_owned()
            };
            let ast = crate::parse_at(&query, 1_800_000_000);
            let is_text =
                ast.groups.iter().flat_map(|g| &g.alts).any(
                    |(_, m)| matches!(m, scour_core::Match::NameContains(t) if t.contains(':')),
                );
            assert!(!is_text, "{query} was read as plain text");
            // And the highlighter agrees it is a field, with no warning on it.
            assert!(
                spans_at(&query, 1_800_000_000)
                    .iter()
                    .all(|s| !s.role.is_warning()),
                "{query} highlights as a mistake"
            );
        }
        for c in complete("kind:", 5) {
            let ast = crate::parse_at(&c.insert, 0);
            assert!(
                ast.groups
                    .iter()
                    .flat_map(|g| &g.alts)
                    .any(|(_, m)| matches!(m, scour_core::Match::Kind(_))),
                "{} was not read as a kind",
                c.insert
            );
        }
    }
}

#[cfg(test)]
mod list_tests {
    use super::*;
    use scour_core::Role;

    fn roles(q: &str) -> Vec<(Role, &str)> {
        spans_at(q, 0)
            .into_iter()
            .map(|s| (s.role, s.of(q)))
            .collect()
    }

    #[test]
    fn whitespace_inside_a_list_is_a_separator_not_a_boundary() {
        // The parser closes these spaces up; they are not term boundaries.
        assert_eq!(
            roles("ext:rs ; toml"),
            vec![
                (Role::Field, "ext"),
                (Role::Colon, ":"),
                (Role::Value, "rs"),
                (Role::Sep, " "),
                (Role::Sep, ";"),
                (Role::Sep, " "),
                (Role::Value, "toml"),
            ]
        );
    }

    #[test]
    fn whitespace_that_is_a_boundary_stays_one() {
        assert_eq!(
            roles("ext:rs toml"),
            vec![
                (Role::Field, "ext"),
                (Role::Colon, ":"),
                (Role::Value, "rs"),
                (Role::Space, " "),
                (Role::Text, "toml"),
            ]
        );
        // With no list in front of it, `rapor ; pdf` is two AND-ed terms and
        // three ordinary gaps.
        assert_eq!(
            roles("rapor ; pdf")
                .into_iter()
                .filter(|(r, _)| *r == Role::Space)
                .count(),
            3
        );
        assert_eq!(crate::parse::parse_at("rapor ; pdf", 0).groups.len(), 2);
    }

    #[test]
    fn the_spans_still_cover_the_query() {
        for q in [
            "ext:rs ; toml",
            "ext:rs ;toml",
            "ext:rs; toml",
            "rapor ; pdf",
        ] {
            let rebuilt: String = spans_at(q, 0).iter().map(|s| s.of(q)).collect();
            assert_eq!(rebuilt, q, "{q}");
        }
    }
}

#[cfg(test)]
mod without_tests {
    use super::*;

    /// A fixed clock, so `dm:7d` parses the same way in a year.
    const NOW: i64 = 1_750_000_000;

    fn drop_kind(q: &str) -> Option<String> {
        without_at(q, &["kind"], NOW)
    }

    #[test]
    fn a_term_on_the_named_field_goes_and_the_rest_stays() {
        assert_eq!(drop_kind("rapor kind:code").as_deref(), Some("rapor"));
        assert_eq!(drop_kind("kind:code rapor").as_deref(), Some("rapor"));
        assert_eq!(drop_kind("kind:code").as_deref(), Some(""));
        assert_eq!(
            without_at("kind:code dm:7d size:>10kb", &["dm"], NOW).as_deref(),
            Some("kind:code  size:>10kb"),
        );
    }

    #[test]
    fn nothing_to_drop_is_none_rather_than_the_same_string() {
        // Must be distinguishable from a query that merely came back equal.
        assert_eq!(drop_kind("rapor"), None);
        assert_eq!(drop_kind(""), None);
        assert_eq!(without_at("rapor kind:code", &["dm"], NOW), None);
    }

    #[test]
    fn the_alias_table_decides_rather_than_the_spelling() {
        assert_eq!(drop_kind("rapor tür:code").as_deref(), Some("rapor"));
        assert_eq!(drop_kind("rapor tur:code").as_deref(), Some("rapor"));
        assert_eq!(drop_kind("rapor type:code").as_deref(), Some("rapor"));
    }

    #[test]
    fn an_excluded_term_is_still_that_field() {
        // `!kind:image` narrows by kind as much as `kind:image` does.
        assert_eq!(drop_kind("rapor !kind:image").as_deref(), Some("rapor"));
    }

    #[test]
    fn a_value_the_parser_would_not_take_is_left_alone() {
        // `kind:zurna` is text: dropping it answers a different question.
        assert_eq!(drop_kind("rapor kind:zurna"), None);
        assert_eq!(without_at("rapor dm:soon", &["dm"], NOW), None);
    }

    #[test]
    fn a_quoted_run_is_text_even_when_it_reads_like_a_field() {
        assert_eq!(drop_kind("\"kind:code\""), None);
        assert_eq!(
            drop_kind("\"iki kelime\" kind:code").as_deref(),
            Some("\"iki kelime\""),
        );
    }

    #[test]
    fn a_term_out_of_the_middle_leaves_the_spaces_that_were_beside_it() {
        // Two spaces, deliberately: the parser reads them as one separator
        // and the ends are trimmed, so nothing downstream sees a difference.
        assert_eq!(drop_kind("a kind:code b").as_deref(), Some("a  b"));
        assert_eq!(drop_kind("kind:code b").as_deref(), Some("b"));
        assert_eq!(drop_kind("a kind:code").as_deref(), Some("a"));
    }
}
