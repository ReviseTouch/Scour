//! Text to [`Ast`].
//!
//! The parser never fails. Everything-style search is typed one character at a
//! time, and a query that is halfway through being written — `size:>`, `ext:`,
//! a lone opening quote — must still produce *some* sensible reading rather
//! than an error dialog. So an unrecognised field, or a field whose value makes
//! no sense, falls back to searching for the raw text: `http://example` is a
//! name to look for, not a broken `http:` field, and `size:abc` looks for the
//! literal string rather than quietly matching everything.

use scour_core::text::DefaultFolder;
use scour_core::{Ast, Cmp, Group, Kind, Match, TimeField};

use crate::time::{now_secs, parse_time};

/// Parse query text against the current clock.
pub fn parse(input: &str) -> Ast {
    parse_at(input, now_secs())
}

/// Parse query text as though `now` were the current unix time.
///
/// Relative windows (`dm:7d`) resolve against this, which is what makes them
/// testable — and, later, what will let a saved search be re-evaluated at a
/// stated moment rather than at whatever moment it happens to be replayed.
pub fn parse_at(input: &str, now: i64) -> Ast {
    let mut groups = Vec::new();
    for token in join_operators(tokenize(input)) {
        // Alternatives split on `|`. Quoted runs are already protected, so a
        // pipe inside quotes is a literal character.
        let alts: Vec<(bool, Match)> = token
            .split('|')
            .filter(|a| !a.is_empty())
            .filter_map(|a| parse_alt(a, now))
            .collect();
        if !alts.is_empty() {
            groups.push(Group { alts });
        }
    }
    Ast { groups }
}

/// Split on whitespace, keeping quoted runs together.
///
/// The quote characters are kept in the token so that a later stage can tell a
/// phrase from a bare word and leave its wildcards alone.
fn tokenize(input: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_quotes = false;
    for c in input.chars() {
        match c {
            '"' => {
                in_quotes = !in_quotes;
                cur.push('"');
            }
            c if c.is_whitespace() && !in_quotes => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Reattach `|` and `!` to what they operate on.
///
/// Whitespace splitting happens first, so `a | b` arrives as three tokens and
/// `! main` as two. Left alone, the lone `|` parsed to nothing and the query
/// silently became `a AND b`; the lone `!` did the same and `! main` searched
/// *for* main rather than against it. Both are the opposite of what was asked,
/// and neither reported anything.
///
/// Only a pipe or bang that stands alone, or sits at the edge of a token, is
/// treated as an operator — so a file called `hello!` and a query `hello! doc`
/// are still two ordinary words.
fn join_operators(tokens: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut pending_bang = false;
    let mut want_alt = false;
    for t in tokens {
        // A quoted run is literal all the way through.
        let quoted = t.starts_with('"');
        if !quoted && t == "|" {
            want_alt = true;
            continue;
        }
        let mut t = t;
        if !quoted && t.starts_with('|') && !out.is_empty() {
            want_alt = true;
            t.remove(0);
        }
        let trailing_alt = !quoted && t.len() > 1 && t.ends_with('|');
        if trailing_alt {
            t.pop();
        }
        if !quoted && t == "!" {
            pending_bang = true;
            continue;
        }
        if t.is_empty() {
            want_alt |= trailing_alt;
            continue;
        }
        if std::mem::take(&mut pending_bang) {
            t.insert(0, '!');
        }
        if std::mem::take(&mut want_alt)
            && let Some(prev) = out.last_mut()
        {
            prev.push('|');
            prev.push_str(&t);
        } else {
            out.push(t);
        }
        want_alt = trailing_alt;
    }
    out
}

fn parse_alt(raw: &str, now: i64) -> Option<(bool, Match)> {
    let (negated, rest) = match raw.strip_prefix('!') {
        Some(r) => (true, r),
        None => (false, raw),
    };
    if rest.is_empty() {
        return None;
    }

    if let Some((field, value)) = split_field(rest) {
        let raw = unquote(value);
        let folded = DefaultFolder::of(&raw);
        // The field table is the only place a field name is written down. It
        // used to be this match arm, with the reference text in `syntax.rs` as
        // a hand-kept second copy — and the two had already drifted.
        let m: Option<Match> = match crate::fields::lookup(&field).map(|f| f.name) {
            Some("ext") => Some(Match::Ext(
                folded
                    .split(';')
                    .map(|e| e.trim_start_matches('.'))
                    .filter(|e| !e.is_empty())
                    .map(str::to_owned)
                    .collect(),
            )),
            Some("path") => Some(Match::PathContains(folded)),
            // Paths are compared as the filesystem stores them. Folding them
            // would make `under:` disagree with the tokens the index actually
            // holds, and a scope that silently matches nothing is worse than
            // one that refuses.
            Some("under") => (!raw.is_empty()).then(|| Match::Under(trim_dir(&raw))),
            Some("parent") => (!raw.is_empty()).then(|| Match::ParentIs(trim_dir(&raw))),
            Some("file") => Some(Match::IsDir(false)),
            Some("folder") => Some(Match::IsDir(true)),
            Some("size") => parse_size(&folded),
            // Turkish spellings are aliases on purpose; see `Kind::from_name`.
            Some("kind") => Kind::from_name(&folded).map(Match::Kind),
            Some("dm") => parse_time(TimeField::Modified, &folded, now),
            Some("dc") => parse_time(TimeField::Created, &folded, now),
            Some("da") => parse_time(TimeField::Accessed, &folded, now),
            // Reserved from the first day so the language does not have to
            // change shape when content indexing arrives. An index built
            // without contents rejects it explicitly rather than silently
            // finding nothing.
            Some("content") => (!folded.is_empty()).then(|| Match::ContentContains(folded.clone())),
            _ => None,
        };
        // An unknown field, or a value that will not parse, becomes a search
        // for the raw text. Dropping the term instead would turn a typo into a
        // query that matches everything.
        return Some((
            negated,
            m.unwrap_or_else(|| name_match(&DefaultFolder::of(&unquote(rest)))),
        ));
    }

    let quoted = rest.starts_with('"');
    let text = DefaultFolder::of(&unquote(rest));
    if text.is_empty() {
        return None;
    }
    // Inside quotes a wildcard is an ordinary character.
    Some((
        negated,
        if quoted {
            Match::NameContains(text)
        } else {
            name_match(&text)
        },
    ))
}

fn name_match(text: &str) -> Match {
    if text.contains('*') || text.contains('?') {
        Match::NameGlob(text.to_owned())
    } else {
        Match::NameContains(text.to_owned())
    }
}

/// Split `field:value`.
///
/// A field name is at least two letters and nothing else. The length rule is
/// what keeps `C:/Users` from being read as a field, which is load-bearing on
/// Windows where every absolute path begins with something that looks like a
/// one-letter one. `http://example` clears the rule and is then rejected for a
/// different reason — `http` is not a field — and falls back to text.
///
/// The letters do not have to be ASCII. Requiring that was a quiet bug: it
/// meant the Turkish aliases `tür:` and `içerik:` were listed as fields, looked
/// like fields, and were silently searched for as literal text instead.
fn split_field(s: &str) -> Option<(String, &str)> {
    let idx = s.find(':')?;
    let name = &s[..idx];
    if name.chars().count() < 2 || !name.chars().all(char::is_alphabetic) {
        return None;
    }
    Some((DefaultFolder::of(name), &s[idx + 1..]))
}

fn unquote(s: &str) -> String {
    s.replace('"', "")
}

/// A directory path in the form the index stores it: `/`-separated, with no
/// trailing slash. `/home/u/` and `/home/u` are the same folder.
fn trim_dir(s: &str) -> String {
    let t = s.replace('\\', "/");
    let trimmed = t.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Split off a leading comparison operator. Absent means `>=`.
///
/// `size:1mb` reading as "at least a megabyte" matches what people mean when
/// they type it; nobody is looking for files of exactly 1048576 bytes.
pub(crate) fn split_cmp(v: &str) -> (Cmp, &str) {
    for (prefix, cmp) in [
        (">=", Cmp::Ge),
        ("<=", Cmp::Le),
        (">", Cmp::Gt),
        ("<", Cmp::Lt),
        ("=", Cmp::Eq),
    ] {
        if let Some(rest) = v.strip_prefix(prefix) {
            return (cmp, rest);
        }
    }
    (Cmp::Ge, v)
}

/// `>1mb`, `<=500kb`, `=0`.
///
/// Multipliers are binary, matching how file managers report sizes on the
/// platforms this runs on.
fn parse_size(v: &str) -> Option<Match> {
    let (cmp, rest) = split_cmp(v);
    let rest = rest.trim();
    let (num, mult) = if let Some(n) = rest.strip_suffix("tb") {
        (n, 1024_i64.pow(4))
    } else if let Some(n) = rest.strip_suffix("gb") {
        (n, 1024_i64.pow(3))
    } else if let Some(n) = rest.strip_suffix("mb") {
        (n, 1024 * 1024)
    } else if let Some(n) = rest.strip_suffix("kb") {
        (n, 1024)
    } else if let Some(n) = rest.strip_suffix('b') {
        (n, 1)
    } else {
        (rest, 1)
    };
    let value: f64 = num.trim().parse().ok()?;
    Some(Match::Size(cmp, (value * mult as f64) as i64))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every alternative in the query, flattened — most assertions here are
    /// about what a token turned into, not about grouping.
    fn m(q: &str) -> Vec<(bool, Match)> {
        parse_at(q, 0)
            .groups
            .into_iter()
            .flat_map(|g| g.alts)
            .collect()
    }

    #[test]
    fn whitespace_is_and_pipe_is_or_bang_is_not() {
        assert_eq!(
            m("rapor"),
            vec![(false, Match::NameContains("rapor".into()))]
        );
        assert_eq!(
            parse_at("a bc", 0).groups.len(),
            2,
            "whitespace makes two groups"
        );
        let q = parse_at("abc|def", 0);
        assert_eq!(q.groups.len(), 1);
        assert_eq!(
            q.groups[0].alts.len(),
            2,
            "a pipe makes one group with two alternatives"
        );
        assert_eq!(m("!tmp"), vec![(true, Match::NameContains("tmp".into()))]);
    }

    #[test]
    fn wildcards_and_quotes() {
        assert_eq!(m("*.rs"), vec![(false, Match::NameGlob("*.rs".into()))]);
        assert_eq!(
            m("\"a*b\""),
            vec![(false, Match::NameContains("a*b".into()))]
        );
        assert_eq!(
            parse_at("\"iki kelime\"", 0).groups.len(),
            1,
            "quotes keep the space"
        );
    }

    #[test]
    fn fields() {
        assert_eq!(
            m("ext:rs;toml"),
            vec![(false, Match::Ext(vec!["rs".into(), "toml".into()]))]
        );
        assert_eq!(m("ext:.RS"), vec![(false, Match::Ext(vec!["rs".into()]))]);
        assert_eq!(m("folder:"), vec![(false, Match::IsDir(true))]);
        assert_eq!(m("file:"), vec![(false, Match::IsDir(false))]);
        assert_eq!(
            m("path:src"),
            vec![(false, Match::PathContains("src".into()))]
        );
        assert_eq!(
            m("size:>1mb"),
            vec![(false, Match::Size(Cmp::Gt, 1_048_576))]
        );
        assert_eq!(
            m("size:<=500kb"),
            vec![(false, Match::Size(Cmp::Le, 512_000))]
        );
        assert_eq!(
            m("size:2tb"),
            vec![(false, Match::Size(Cmp::Ge, 2 * 1024_i64.pow(4)))]
        );
        assert_eq!(m("kind:kod"), vec![(false, Match::Kind(Kind::Code))]);
        assert_eq!(m("kind:KLASÖR"), vec![(false, Match::Kind(Kind::Dir))]);
    }

    #[test]
    fn scope_fields_keep_the_path_as_written() {
        assert_eq!(
            m("under:/home/U/Projeler"),
            vec![(false, Match::Under("/home/U/Projeler".into()))]
        );
        assert_eq!(
            m("in:/home/u/x/"),
            vec![(false, Match::Under("/home/u/x".into()))]
        );
        assert_eq!(
            m("parent:/etc"),
            vec![(false, Match::ParentIs("/etc".into()))]
        );
        assert_eq!(m("under:/"), vec![(false, Match::Under("/".into()))]);
        // Empty is not a scope; it falls back to text like any unusable value.
        assert_eq!(
            m("under:"),
            vec![(false, Match::NameContains("under:".into()))]
        );
    }

    #[test]
    fn field_names_may_contain_non_ascii_letters() {
        // `tür:` and `içerik:` were listed as aliases but could never match,
        // because the field-name rule demanded ASCII. They are the only two
        // fields whose Turkish spelling is not ASCII, so nothing else was hit.
        assert_eq!(m("tür:kod"), vec![(false, Match::Kind(Kind::Code))]);
        assert_eq!(m("TÜR:kod"), vec![(false, Match::Kind(Kind::Code))]);
        assert_eq!(
            m("içerik:x"),
            vec![(false, Match::ContentContains("x".into()))]
        );
        // The rule that made it worth having is untouched.
        assert_eq!(
            m("C:/Users"),
            vec![(false, Match::NameContains("c:/users".into()))]
        );
    }

    #[test]
    fn a_field_that_cannot_be_read_becomes_plain_text() {
        // This is the rule that keeps a half-typed query usable, and keeps
        // Windows paths and URLs from being mistaken for fields.
        assert_eq!(
            m("http://x"),
            vec![(false, Match::NameContains("http://x".into()))]
        );
        assert_eq!(
            m("kind:zurna"),
            vec![(false, Match::NameContains("kind:zurna".into()))]
        );
        assert_eq!(
            m("size:abc"),
            vec![(false, Match::NameContains("size:abc".into()))]
        );
        assert_eq!(
            m("C:/Users"),
            vec![(false, Match::NameContains("c:/users".into()))]
        );
    }

    #[test]
    fn content_is_parsed_even_though_nothing_can_answer_it_yet() {
        assert_eq!(
            m("content:gizli"),
            vec![(false, Match::ContentContains("gizli".into()))]
        );
        assert_eq!(
            m("içerik:gizli"),
            vec![(false, Match::ContentContains("gizli".into()))]
        );
        // An empty value is not a content search; it is the literal text.
        assert_eq!(
            m("content:"),
            vec![(false, Match::NameContains("content:".into()))]
        );
    }

    #[test]
    fn terms_are_case_folded_the_turkish_way() {
        assert_eq!(
            m("İSTANBUL"),
            vec![(false, Match::NameContains("istanbul".into()))]
        );
        assert_eq!(m("ISTANBUL"), m("ısтanbul".replace('т', "t").as_str()));
    }

    #[test]
    fn narrowing_terms_survive_the_round_trip() {
        let q = parse_at("rapor !tmp *.rs ab ext:pdf", 0);
        assert_eq!(q.narrowing_terms(3), vec!["rapor"]);
    }
}
