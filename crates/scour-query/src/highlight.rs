//! Query text, cut into pieces a search box can colour.
//!
//! The parser is forgiving on purpose — a term it cannot read is searched for
//! as its own text — and that forgiveness is what makes a half-typed query
//! usable. It is also the one way the engine can confidently answer the wrong
//! question: `kind:zurna` finds files *named* "kind:zurna", and says so
//! nowhere. Everything here exists to put that on the screen while it is being
//! typed, rather than in the results afterwards.
//!
//! Two rules keep it honest:
//!
//! * **Same judgement as the parser.** A value is coloured as usable only if
//!   the parser's own value parsers accept it. There is no second opinion.
//! * **Nothing is dropped.** The spans cover the input byte for byte,
//!   whitespace included, so a frontend can concatenate them and get the query
//!   back. A highlighter that quietly loses a character would put every colour
//!   after it in the wrong place.

use scour_core::text::DefaultFolder;
use scour_core::{Completion, CompletionKind, Role, Span};

use crate::fields::{self, FIELDS, KIND_VALUES, TIME_VALUES, Takes};
use crate::time::now_secs;

/// Cut a query into coloured runs, against the current clock.
pub fn spans(input: &str) -> Vec<Span> {
    spans_at(input, now_secs())
}

/// Cut a query into coloured runs as though `now` were the current unix time.
///
/// Relative windows are judged against this, so `dm:7d` is testable — and a
/// saved query can be coloured as it was read at the time it ran.
pub fn spans_at(input: &str, now: i64) -> Vec<Span> {
    let mut out = Vec::new();
    let toks = tokens(input);
    // A list is a context, not a character. `ext:rs ; toml` is one filter, so
    // the spaces in it are separators rather than term boundaries — and the
    // word after the separator is a value, not a new search term. Colouring
    // either of them the other way would show a query the engine does not see.
    let mut list: Option<&'static fields::Field> = None;
    let mut expect_value = false;
    // A `!` in front of a list field reaches every value in it, and the values
    // of a list are the one thing that outlives its token: `!ext:rs; toml`
    // excludes both.
    let mut list_not = false;
    // **A `!` on its own belongs to the term after it.** The parser binds it
    // there — `expand`'s `pending_bang` — so `! main` and `!main` ask exactly
    // the same question. The colouring did not: it drew one red character and
    // then the excluded term in the colour of the thing being looked for,
    // which is the disagreement between parser and highlighter this whole
    // module exists to prevent.
    let mut lone_bang = false;
    for (i, (start, token)) in toks.iter().enumerate() {
        let (start, token) = (*start, *token);
        if is_space(token) {
            // **A space beside an `or` is inside the group, not between two
            // of them.** `a ;b` and `a; b` are one term with two alternatives
            // — the parser joins across the space either way — and drawing an
            // ordinary gap there showed two terms where there is one.
            // Looking *through* a lone `!`, which belongs to the term after
            // it rather than being one: `a ! ;c` is `a` or `not c`, one term.
            // **While a list is open the `;` belongs to it**, which is the
            // order the parser works in: `join_lists` runs before
            // `join_operators`, so `ext:rs ; "x y"` is a filter and a phrase
            // and not one term with two alternatives.
            // **While a list is open the `;` belongs to it**, which is the
            // order the parser works in: `join_lists` runs before
            // `join_operators`, so `ext:rs ; "x y"` is a filter and a phrase
            // and not one term with two alternatives. A `|` is an operator
            // either way — no list has ever claimed one.
            let alt = |t: &str, at_start: bool| {
                let mark = if at_start {
                    t.starts_with('|')
                } else {
                    t.ends_with('|')
                };
                let semi = if at_start {
                    t.starts_with(';')
                } else {
                    t.ends_with(';')
                };
                mark || (semi && list.is_none())
            };
            let beside_alt = next_term_word(&toks, i).is_some_and(|t| alt(t, true))
                || prev_word(&toks, i).is_some_and(|t| alt(t, false));
            // A field's value continuing across the space is a different
            // thing from two alternatives sitting either side of one, and
            // only the first carries the term's exclusion with it.
            let list_join = list.is_some()
                && next_word(&toks, i).is_some_and(|t| !t.starts_with('"'))
                && (expect_value || next_word(&toks, i).is_some_and(|t| t.starts_with(';')));
            let joins = beside_alt || list_join;
            let role = if joins { Role::Sep } else { Role::Space };
            let span = Span::new(start, token.len(), role);
            // The gap between a lone `!` and its term is inside the exclusion:
            // one red run, not two with a hole in it.
            let inside = (list_join && list_not) || lone_bang;
            out.push(if inside { span.excluded() } else { span });
            if !list_join {
                list = None;
                expect_value = false;
                list_not = false;
            }
            continue;
        }
        // A quoted run is never a continuation: `join_lists` pushes it
        // through untouched and closes the list behind it.
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
                // A separator standing on its own means the value it
                // separates has not been written yet.
                expect_value = true;
            }
            // **A `|` ends the value and starts an alternative.** The
            // parser glues the token onto the list and *then* cuts it there,
            // so `ext:rs ; i|j` is a two-extension filter or the word `j`;
            // drawn as one value it was one thing.
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
        // A token can be several terms now, and only the last of them can be
        // continued across the space: `a!ext:rs; toml`.
        let tail = *bang_pieces(token).last().unwrap_or(&token);
        // **The first alternative only.** A `!` standing on its own binds to
        // the term after it exactly as though it had been written against it
        // — `! d;e` is `!d;e`, which is `(not d) or e` — so it reaches the
        // first alternative and stops. Marking the whole term drew `e` as
        // excluded when the engine was searching *for* it.
        if std::mem::take(&mut lone_bang) {
            for span in &mut out[mark..] {
                if span.role == Role::Or {
                    break;
                }
                span.not = true;
            }
        }
        // Nothing follows a `!` inside its own token, so this can only be the
        // operator standing alone.
        lone_bang = token == "!";
        list = field_of(tail);
        expect_value = list.is_some() && tail.ends_with(';');
    }
    out
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
            // **A run of whitespace ends at the first character that is not
            // whitespace, quote or no quote.** The quote was toggled here
            // too, so a space followed by a phrase — `rapor "iki kelime"` —
            // opened a quoted run *inside the whitespace token*, and the
            // whole of ` "iki kelime"` came out as one piece of plain text:
            // no quote colour, no phrase colour, and one term where the
            // engine reads two. Found by asking the parser.
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
            // Multi-byte characters are never whitespace or a quote, so
            // advancing by one byte here can only land inside a run that is
            // already being consumed whole.
            i += 1;
        }
        out.push((start, &input[start..i]));
    }
    out
}

/// One whitespace-separated term, which may itself hold alternatives.
///
/// Answers whether the term is excluded, which for a list field has to travel
/// past the end of the token: `!ext:rs; toml` is one filter and the `toml`
/// after the space is inside it.
fn term(out: &mut Vec<Span>, start: usize, text: &str, now: i64) -> bool {
    // A lone operator is an operator; `hello!` is a word. The parser makes the
    // same distinction, and colouring it differently would teach the wrong rule.
    if text == "|" || text == ";" {
        out.push(Span::new(start, 1, Role::Or));
        return false;
    }
    if text == "!" {
        out.push(Span::new(start, 1, Role::Not).excluded());
        return true;
    }

    // **A `!` inside a word starts a new term**, the same cut `split_bangs`
    // makes. Without it the line drew `rapor!tmp` as one word being looked
    // for, which is what the engine did too — and both were wrong.
    let mut at = start;
    let mut negated = false;
    for piece in bang_pieces(text) {
        negated |= one_term(out, at, piece, now);
        at += piece.len();
    }
    negated
}

/// One term: alternatives separated by `;` or `|`, each with an optional `!`.
fn one_term(out: &mut Vec<Span>, start: usize, text: &str, now: i64) -> bool {
    let mut at = start;
    let mut negated = false;
    // **`;` is `|` outside a field's value**, and the parser has said so since
    // `OPUS ; SONNET` was reported as finding neither. This did not, so
    // `HASAN;DENEME` was drawn as one word somebody was looking for while the
    // engine read two alternatives — and `!ama ;deneme` drew four independent
    // terms where there were three, one of them a disjunction with the
    // exclusion inside it. A query line that hides an `or` hides the reason
    // its answer is enormous.
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

/// The terms inside one whitespace-separated token, cut at every `!` that
/// starts a new one. Mirrors `parse::split_bangs`, including its three
/// exceptions; the pieces are contiguous and cover the token.
fn bang_pieces(text: &str) -> Vec<&str> {
    if text.starts_with('"') || field_of(text).is_some() {
        return vec![text];
    }
    let mut out = Vec::new();
    let mut from = 0;
    for (i, c) in text.char_indices() {
        if c != '!' || i == 0 || i + 1 == text.len() {
            continue;
        }
        let before = text[..i].chars().next_back();
        if before == Some('|') || before == Some(';') {
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
/// the one before.
///
/// Mirrors `parse::ast`: a token that is a field term or a quoted run keeps
/// its own semicolons — `ext:rs;toml` is one filter — and everything else
/// splits on both marks.
fn alternatives(text: &str) -> Vec<(&str, Option<char>)> {
    let mut out = Vec::new();
    let mut sep = None;
    // A quoted run is literal all the way through, and the split is the
    // parser's own so the two cannot cut in different places.
    for part in crate::parse::split_outside_quotes(text, '|') {
        // A field's value keeps its own semicolons, and whether it is one is
        // a question about *this alternative*: `a|ext:rs;toml` is a word or a
        // filter, and asking the whole token gets the wrong answer.
        if field_of(part).is_some() {
            out.push((part, sep));
        } else {
            let mut inner = sep;
            for piece in crate::parse::split_outside_quotes(part, ';') {
                out.push((piece, inner));
                inner = Some(';');
            }
        }
        sep = Some('|');
    }
    out
}

/// One alternative: an optional `!`, then either a field term or plain text.
///
/// **The `!` marks the whole alternative, not the character it is.** A query
/// line is read for two things — what is wanted and what is not — and no role
/// says which: `pdf` in `!ext:pdf` is a `Value` exactly as it is in
/// `ext:pdf`. Colouring [`Role::Not`] alone put one red character in front of
/// a term drawn in the colour of the thing being looked for.
///
/// So the extent is decided here, where the `!` is read, and every run the
/// alternative produces carries it. Answers whether it was negated, because
/// a list field goes on across the spaces after it and the caller is the only
/// one that can see that far.
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
                // Two or more letters and a colon, but not a field: the whole
                // term is text. Colouring only the name would suggest the
                // colon still separates something.
                None => out.push(Span::new(at, rest.len(), Role::UnknownField)),
            }
        }
        None => plain(out, at, rest),
    }
}

/// The value after a field's colon.
fn value_spans(out: &mut Vec<Span>, start: usize, value: &str, f: &fields::Field, now: i64) {
    if value.is_empty() {
        // `folder:` is complete; `ext:` is not yet wrong, only unfinished, and
        // marking it as a mistake while someone is mid-word would be noise.
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

/// `field:value`, by the parser's rule: two or more letters and nothing else.
///
/// The rule is what keeps `C:/Users` and `http://example` out, and it has to be
/// the parser's rule exactly — a highlighter that thought `C:` was a field
/// would paint every Windows path as a mistake.
fn field_split(s: &str) -> Option<(&str, &str)> {
    let idx = s.find(':')?;
    let name = &s[..idx];
    if name.chars().count() < 2 || !name.chars().all(char::is_alphabetic) {
        return None;
    }
    Some((name, &s[idx + 1..]))
}

// ───────────────────────────── completions ─────────────────────────────

/// What could be typed at `cursor`, given the query so far.
///
/// The offer is always for the word the cursor is in, so a completion accepted
/// mid-query replaces that word and leaves the rest alone. Everything comes
/// from the field table, which means a field that exists is offered and a field
/// that does not cannot be.
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

/// The whitespace-delimited word the cursor sits in, as byte offsets.
/// The caret, moved to a boundary this string actually has.
///
/// A caret is a number from somewhere else, and the somewhere else counts
/// differently. A browser's `selectionStart` counts UTF-16 code units; this
/// counts bytes; `İ` is one of the first and two of the second. So a caret
/// after a single Turkish capital I arrives one short of where it means, and
/// slicing there is not a wrong answer but a panic — which took the connection
/// with it, and the request that connection was carrying. Measured on this
/// machine: two of them in one second of ordinary typing.
///
/// Clients should send bytes and the page now does. This is what happens when
/// one does not: the caret moves back to the start of the character it landed
/// inside, and the offer is for a word that was very nearly the right one. The
/// alternative — refusing, or returning nothing — spends a crash to punish a
/// caller for a rounding error nobody can see.
fn caret(input: &str, cursor: usize) -> usize {
    let mut at = cursor.min(input.len());
    while !input.is_char_boundary(at) {
        at -= 1;
    }
    at
}

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

    /// The invariant everything else depends on: put the spans back together
    /// and the query comes back.
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
        // This is the case the whole module is for: the parser turns each of
        // these into a search for its own text, silently.
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
        // The reference text used `sizE:>1mb` as its example of a misspelling
        // that falls back to text. It does not: field names are folded, so
        // this is the size field, and a highlighter that marked it as a
        // mistake would be teaching a rule the parser does not have.
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
        // One letter is not a field name, so this is ordinary text — the same
        // rule the parser applies, for the same reason.
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
    ///
    /// Every one of these drew a single red character in front of a term in
    /// the colour of the thing being *searched for*, which is the opposite of
    /// what the query says.
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
        // **A `!` on its own binds to the term after it**, exactly as
        // `expand` does — `! main` and `!main` are the same query, so they
        // are the same colour.
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

    /// `;` is `|` outside a field's value, and the line has to say so.
    ///
    /// This is the fault that made a query nobody could read: `HASAN;DENEME`
    /// was drawn as one word being looked for, while the engine read two
    /// alternatives — and `!ama ;deneme` looked like four independent terms
    /// when the third was an `or` with the exclusion inside it.
    #[test]
    fn a_semicolon_between_words_is_an_operator() {
        assert_eq!(
            roles("HASAN;DENEME"),
            vec![
                (Role::Text, "HASAN"),
                (Role::Or, ";"),
                (Role::Text, "DENEME")
            ]
        );
        covers("HASAN;DENEME");
        // Leading, which is how it joins to the term before it.
        // The space is *inside* the group: the parser joins `;b` to `a`, so
        // an ordinary gap here would draw two terms where there is one.
        assert_eq!(
            roles("a ;b"),
            vec![
                (Role::Text, "a"),
                (Role::Sep, " "),
                (Role::Or, ";"),
                (Role::Text, "b")
            ]
        );
        covers("a ;b");
        // A lone one is an operator, like a lone pipe.
        assert_eq!(
            roles("a ; b")
                .iter()
                .filter(|(r, _)| *r == Role::Or)
                .count(),
            1
        );
        covers("a ; b");
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
        // Negated, which is where the parser used to come apart too.
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

    /// The colouring and the parser read the same query.
    ///
    /// **Every fault this module has ever had is the two disagreeing**, and
    /// each was found by a person staring at a line that did not match the
    /// answer: `;` drawn as a letter when it is an `or`; a lone `!` drawn as
    /// itself when it binds to the word after it; `rapor!tmp` drawn as one
    /// word when it is a term and an exclusion. None of them were caught by a
    /// test, because every test here asked the highlighter what it thought
    /// and never asked the parser.
    ///
    /// So this one asks both. For each query: the same number of AND-ed
    /// groups, the same number of alternatives in each, and the same
    /// alternatives negated.
    ///
    /// The corpus is deliberately free of the rewritten spellings —
    /// `size:1mb..2mb`, `empty:`, the Everything macros — because `expand`
    /// turns one token into several groups on purpose, which is a difference
    /// the line is not meant to show.
    /// The colouring and the parser read the same query.
    ///
    /// **Every fault this module has ever had is the two disagreeing**, and
    /// each was found by a person staring at a line that did not match the
    /// answer: `;` drawn as a letter when it is an `or`; a lone `!` drawn as
    /// itself when it binds to the word after it; `rapor!tmp` drawn as one
    /// word when it is a term and an exclusion; a phrase after a space drawn
    /// as neither. None were caught by a test, because every test here asked
    /// the highlighter what it thought and never asked the parser.
    ///
    /// So this asks both: the same number of AND-ed terms, the same number of
    /// alternatives in each, and the same ones excluded.
    ///
    /// The corpus has no rewritten spellings — `size:1mb..2mb`, `empty:`, the
    /// Everything macros — because `expand` turns one token into several
    /// terms on purpose, which is a difference the line is not meant to show.
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
            "a ;b",
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
    ///
    /// Thirty hand-written queries are thirty guesses about where the two
    /// disagree, and every fault so far has been somewhere nobody guessed.
    /// Four thousand is not a guess — the phrase-after-a-space fault above
    /// was found by this and by nothing else.
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
                    // **One shape no colouring can draw truthfully.** A lone
                    // `!` followed by a term that opens with `;` or `|` binds
                    // *past* the separator — `a ! ;c` is `a` or `not c` — so
                    // the `!` is written before the `;` and belongs after it.
                    // The line gets the terms and the alternatives right; it
                    // cannot get which side of the `or` the exclusion is on,
                    // because that is not where it was typed.
                    let toks: Vec<&str> = q.split_whitespace().collect();
                    let looks_past = toks
                        .windows(2)
                        .any(|w| w[0] == "!" && w[1].starts_with([';', '|']));
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
    ///
    /// The structure is read off the spans by the rules a person reads them
    /// by: terms are separated by the spaces between them, an `or` joins two
    /// alternatives inside one term, and a `!` begins a term unless it is
    /// already at the start of one or is starting an alternative.
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
        // A `!` with nothing after it excludes nothing — the parser drops it,
        // and the line draws it because somebody is halfway through typing
        // the word it will exclude. Neither is wrong; it is not a term.
        // A term made only of operators has nothing to search for: the
        // parser drops it, and the line draws it because somebody is halfway
        // through typing the word it will apply to.
        groups.retain(|g| {
            !g.iter()
                .all(|s| matches!(s.role, Role::Not | Role::Or | Role::Space | Role::Sep))
        });
        // And a `!` at the *end* of a term excludes nothing either: `a ; !`
        // is one alternative, not two with an empty second.
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
            // An `or` with nothing after it joins nothing yet, for the same
            // reason a `!` with nothing after it excludes nothing — and an
            // `or` straight after another one joins nothing either: `a ; ;c`
            // reads as `a` or `c`, which is what the engine makes of it.
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
            // And the same ones are excluded: cut the drawn run at its `or`s
            // and ask whether each piece carries the mark.
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
                    // The same rule the count uses: only an `or` with
                    // something in front of it starts a new alternative.
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
        // Someone is mid-word. Marking it red here would flash a warning on
        // the way to every correct query with a field in it.
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
        // Offered, not offered *alone*: `exe:` begins with the same two
        // letters, and asserting a count here was asserting that no field
        // would ever be added.
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
        // The spelling the user chose is kept, so accepting a completion does
        // not silently rewrite their query into another language.
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
        // What a browser sends after typing `İ`: one code unit per character,
        // where this counts two bytes for that one. Every offset from nowhere
        // to past the end has to answer rather than panic — the caller is a
        // socket, and the panic was killing the thread holding it.
        for input in ["İ", "kİ", "ad:İ", "rapor İş", "TRABZON.MÜZEKKERE"] {
            for cursor in 0..=input.len() + 4 {
                let _ = complete(input, cursor);
            }
        }

        // And it lands on the character the caret is inside, not before it:
        // `ad:İ` is 5 bytes, so a browser's 4 is mid-`İ` and means the word so
        // far — which is still `ad:`, still a field, still worth an offer.
        assert_eq!(complete("ad:İ", 4), complete("ad:İ", 3));
    }

    #[test]
    fn every_offered_completion_parses_as_what_it_claims() {
        // The strongest thing this can promise: take any field it offers,
        // fill it in the way its own example does, and the parser reads it as
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
        // The parser closes these spaces up, so colouring them as term
        // boundaries would show a query the engine does not see.
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
        // **And a `;` standing alone is not a boundary at all.** The
        // comment here used to say "no list in front of it, so the parser
        // leaves this alone too", and the parser had never left it alone:
        // `rapor ; pdf` is one term with two alternatives. Two ordinary gaps
        // drew it as two terms AND-ed, which is a different query.
        assert_eq!(
            roles("rapor ; pdf")
                .into_iter()
                .filter(|(r, _)| *r == Role::Space)
                .count(),
            0
        );
        assert_eq!(crate::parse::parse_at("rapor ; pdf", 0).groups.len(), 1);
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
