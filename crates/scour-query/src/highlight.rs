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
                && next_word(&toks, i).is_some_and(|t| {
                    crate::parse::can_join_list(t) && (expect_value || t.starts_with(';'))
                });
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
        // A continuation stays in the list only while its items look like list values.
        if let Some(f) = list
            && crate::parse::can_join_list(token)
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
            let (list_part, after_semi) = if rest.is_empty() {
                ("", None)
            } else {
                crate::parse::split_list_token(rest)
            };
            // A `|` ends the value and starts an alternative: `ext:rs ; i|j`
            // is a two-extension filter or the word `j`.
            let mut pieces = crate::parse::split_outside_quotes(list_part, '|').into_iter();
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
            if let Some(tail_terms) = after_semi {
                out.push(Span::new(at, 1, Role::Space));
                at += 1;
                list_not = term(&mut out, at, tail_terms, now);
                let tail = last_term_piece(tail_terms);
                list = open_list_field(tail);
                expect_value = list.is_some() && tail.ends_with(';');
            }
            continue;
        }
        let mark = out.len();
        let pieces = term_pieces(token);
        list_not = term(&mut out, start, token, now) | (lone_bang && pieces.len() == 1);
        // Only the last term in a token continues across the space.
        let tail = last_term_piece(token);
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
        list = open_list_field(tail);
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

/// The list field `tail` leaves open for a following `;`, if any.
fn open_list_field(tail: &str) -> Option<&'static fields::Field> {
    crate::parse::list_field(tail)
        .filter(|_| crate::parse::split_outside_quotes(tail, '|').len() == 1)
}

/// The final term piece inside `token` after `;` and `!` cuts.
fn last_term_piece(token: &str) -> &str {
    term_pieces(token).last().map(|(p, _)| *p).unwrap_or(token)
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
        negated = one_term(out, at, piece, now);
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
    let mut out = Vec::new();
    for (n, part) in crate::parse::cut_semicolons(text).into_iter().enumerate() {
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
    // An empty `content:""` is left out of the search, not misread: its
    // quotes are quotes. See `parse_alt`.
    if f.name == "content" && folded.trim().is_empty() {
        for (i, c) in value.char_indices() {
            if c == '"' {
                out.push(Span::new(start + i, 1, Role::Quote));
            }
        }
        return;
    }
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
    if f.takes.is_list() {
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
    let (head, tail) = if f.takes == Takes::Kind
        && let Some(idx) = typed.rfind(';')
    {
        (&typed[..=idx], &typed[idx + 1..])
    } else {
        ("", typed)
    };
    let prefix = DefaultFolder::of(tail);
    let list: &[&str] = match f.takes {
        Takes::Kind => KIND_VALUES,
        Takes::Time => TIME_VALUES,
        _ => return Vec::new(),
    };
    list.iter()
        .filter(|v| v.starts_with(&prefix))
        .map(|v| Completion {
            insert: format!("{written_name}:{head}{v}"),
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
mod tests;

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
