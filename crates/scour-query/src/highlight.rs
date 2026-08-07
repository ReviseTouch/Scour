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
    for (i, (start, token)) in toks.iter().enumerate() {
        let (start, token) = (*start, *token);
        if is_space(token) {
            let joins = list.is_some()
                && (expect_value || next_word(&toks, i).is_some_and(|t| t.starts_with(';')));
            let role = if joins { Role::Sep } else { Role::Space };
            out.push(Span::new(start, token.len(), role));
            if !joins {
                list = None;
                expect_value = false;
            }
            continue;
        }
        if let Some(f) = list
            && (expect_value || token.starts_with(';'))
        {
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
            if !rest.is_empty() {
                expect_value = rest.ends_with(';');
            }
            value_spans(&mut out, at, rest, f, now);
            continue;
        }
        term(&mut out, start, token, now);
        list = field_of(token);
        expect_value = list.is_some() && token.ends_with(';');
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
            if c == b'"' {
                in_quotes = !in_quotes;
            } else if !in_quotes && c.is_ascii_whitespace() != space {
                break;
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
fn term(out: &mut Vec<Span>, start: usize, text: &str, now: i64) {
    // A lone operator is an operator; `hello!` is a word. The parser makes the
    // same distinction, and colouring it differently would teach the wrong rule.
    if text == "|" {
        out.push(Span::new(start, 1, Role::Or));
        return;
    }
    if text == "!" {
        out.push(Span::new(start, 1, Role::Not));
        return;
    }

    let mut at = start;
    let quoted = text.starts_with('"');
    for (i, alt) in text.split('|').enumerate() {
        if i > 0 && !quoted {
            out.push(Span::new(at, 1, Role::Or));
            at += 1;
        } else if i > 0 {
            // A pipe inside quotes is an ordinary character.
            out.push(Span::new(at, 1, Role::Phrase));
            at += 1;
        }
        alternative(out, at, alt, now);
        at += alt.len();
    }
}

/// One alternative: an optional `!`, then either a field term or plain text.
fn alternative(out: &mut Vec<Span>, start: usize, text: &str, now: i64) {
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
        // No list in front of it, so the parser leaves this alone too.
        assert_eq!(
            roles("rapor ; pdf")
                .into_iter()
                .filter(|(r, _)| *r == Role::Space)
                .count(),
            2
        );
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
