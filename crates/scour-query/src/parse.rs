//! Text to [`Ast`].
//!
//! The parser never fails: a query halfway through being typed — `size:>`, a
//! lone opening quote — must still read as something. An unknown field, or one
//! whose value will not parse, becomes a search for the raw text.

use scour_core::text::DefaultFolder;
use scour_core::{Ast, Cmp, Group, Kind, Match, TimeField};

use crate::time::{now_secs, parse_time};

/// Parse query text against the current clock.
pub fn parse(input: &str) -> Ast {
    parse_at(input, now_secs())
}

/// Parse query text as though `now` were the current unix time; relative
/// windows (`dm:7d`) resolve against it, which is what makes them testable.
pub fn parse_at(input: &str, now: i64) -> Ast {
    let mut groups = Vec::new();
    for token in join_parens(join_operators(split_bangs(split_semicolons(join_lists(
        tokenize(input),
    )))))
    .into_iter()
    .flat_map(|t| expand(&t))
    {
        // Only `|` makes alternatives, and only outside quotes. Every `;`
        // still here is a field's own list or a character inside a phrase.
        let alts: Vec<(bool, Match)> = split_outside_quotes(&token, '|')
            .into_iter()
            .filter(|a| !a.is_empty())
            .filter_map(|a| parse_alt(a, now))
            .collect();
        if !alts.is_empty() {
            groups.push(Group { alts });
        }
    }
    Ast { groups }
}

/// Rewrite the spellings that are shorthand for something the language can
/// already say. Text in, text out, so `explain` reads back what actually ran.
fn expand(token: &str) -> Vec<String> {
    // A quoted run is literal all the way through.
    if token.starts_with('"') {
        return vec![token.to_owned()];
    }
    let (neg, body) = match token.strip_prefix('!') {
        Some(rest) => ("!", rest),
        None => ("", token),
    };
    let with =
        |parts: &[&str]| -> Vec<String> { parts.iter().map(|p| format!("{neg}{p}")).collect() };
    let Some((field, value)) = split_field(body) else {
        return vec![token.to_owned()];
    };
    let canonical = match crate::fields::lookup(&field) {
        Some(f) => f.name,
        None => return vec![token.to_owned()],
    };
    // The type macros: `kind:` under the names Everything gives them.
    let kind = match canonical {
        "audio" => Some("audio"),
        "video" => Some("video"),
        "pic" => Some("image"),
        "doc" => Some("doc"),
        "exe" => Some("exec"),
        "zip" => Some("archive"),
        _ => None,
    };
    if let Some(k) = kind {
        return with(&[&format!("kind:{k}")]);
    }
    match canonical {
        // A folder's size is stored as zero and its child count not at all, so
        // "empty" can only mean a file of no bytes.
        "empty" => return with(&["file:", "size:=0"]),
        "startwith" if !value.is_empty() => return with(&[&format!("{value}*")]),
        "endwith" if !value.is_empty() => return with(&[&format!("*{value}")]),
        _ => {}
    }
    // `a..b`, Everything's range. Two comparisons, which is what it means.
    if let Some((lo, hi)) = value.split_once("..")
        && matches!(
            crate::fields::lookup(&field).map(|f| f.takes),
            Some(crate::fields::Takes::Size | crate::fields::Takes::Time)
        )
    {
        return match (lo.is_empty(), hi.is_empty()) {
            (false, false) => with(&[&format!("{field}:>={lo}"), &format!("{field}:<={hi}")]),
            // Open at one end, which Everything also allows.
            (true, false) => with(&[&format!("{field}:<={hi}")]),
            (false, true) => with(&[&format!("{field}:>={lo}")]),
            (true, true) => vec![token.to_owned()],
        };
    }
    vec![token.to_owned()]
}

/// Glue `( a | b )` into one token, but only when the run holds a `|`: anywhere
/// else a parenthesis is an ordinary character in a name (`rapor (1).pdf`).
/// Nesting is not supported; the AST is one level of AND over OR by construction.
fn join_parens(tokens: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        // `<a|b>` groups as `(a|b)` does, under the same rule.
        let angle = tokens[i].starts_with('<');
        let open = (tokens[i].starts_with('(') || angle) && !tokens[i].starts_with("(\"");
        if !open {
            out.push(tokens[i].clone());
            i += 1;
            continue;
        }
        // How far does it reach, and is there an alternation inside?
        let mut j = i;
        let closer = if angle { '>' } else { ')' };
        while j < tokens.len() && !tokens[j].ends_with(closer) {
            j += 1;
        }
        let run = tokens.get(i..=j.min(tokens.len() - 1)).unwrap_or_default();
        let joined = run.join(" ");
        let inner = joined
            .trim_start_matches(['(', '<'])
            .trim_end_matches([')', '>'])
            .trim()
            .to_owned();
        if j >= tokens.len() || !inner.contains('|') {
            // Not a group: ordinary characters in a name.
            out.push(tokens[i].clone());
            i += 1;
            continue;
        }
        // `a | b` inside the parentheses is the same as `a|b`.
        out.push(
            inner
                .split('|')
                .map(str::trim)
                .collect::<Vec<_>>()
                .join("|"),
        );
        i = j + 1;
    }
    out
}

/// Split on whitespace, keeping quoted runs together.
///
/// The quotes stay in the token so a later stage can tell a phrase from a word.
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

/// Cut a token at every `;` that separates two terms: between terms `;` means
/// what a space means, both, while inside a field's value it stays "any of
/// these". After `join_lists`, which glues `ext:rs ; toml`, and before
/// `split_bangs`, so `a;!b` reaches it as `a` and `!b`.
fn split_semicolons(tokens: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(tokens.len());
    for t in tokens {
        if t.starts_with('"')
            || token_field(&t).is_some_and(|(f, _)| crate::fields::lookup(&f).is_some())
        {
            out.push(t);
            continue;
        }
        for piece in split_outside_quotes(&t, ';') {
            if !piece.is_empty() {
                out.push(piece.to_owned());
            }
        }
    }
    out
}

/// Cut a token where a `!` starts a new term: `rapor!tmp` excludes `tmp`, no
/// space needed. Must run after `join_lists`, or a `!` cuts an open list.
/// Not a separator at the end (`hello!`), right after `;` or `|` (`a|!b`, where
/// it belongs to the alternative starting there), or inside a value or quotes.
fn split_bangs(tokens: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(tokens.len());
    for t in tokens {
        if t.starts_with('"')
            || token_field(&t).is_some_and(|(f, _)| crate::fields::lookup(&f).is_some())
        {
            out.push(t);
            continue;
        }
        let mut from = 0;
        for (i, c) in t.char_indices() {
            if c != '!' || i == 0 || i + 1 == t.len() {
                continue;
            }
            let before = t[..i].chars().next_back();
            if before == Some('|') || before == Some(';') {
                continue;
            }
            if i > from {
                out.push(t[from..i].to_owned());
            }
            from = i;
        }
        out.push(t[from..].to_owned());
    }
    out
}

/// Close up the spaces around a `;` inside a field's value, so `ext:rs ; toml`
/// is one filter. Only after a real field: `rapor ; pdf` has no list to extend.
fn join_lists(tokens: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    // Set when the previous token ended in a way that wants what comes next.
    let mut open = false;
    for t in tokens {
        if t.starts_with('"') {
            out.push(t);
            open = false;
            continue;
        }
        // Both sides must be a real field's value, or `rapor ; pdf` invents a
        // list where there was none.
        let after_field = out.last().is_some_and(|p| token_field(p).is_some());
        if after_field && (open || t.starts_with(';')) {
            let prev = out.last_mut().expect("checked by after_field");
            // The separator stays exactly once, however it was spaced.
            if !prev.ends_with(';') {
                prev.push(';');
            }
            prev.push_str(t.trim_start_matches(';'));
        } else {
            out.push(t);
        }
        let last = out.last().map(String::as_str).unwrap_or("");
        open = last.ends_with(';') && token_field(last).is_some();
    }
    out
}

/// Reattach a `|` or `!` that stands alone — whitespace splitting turns `a | b`
/// into three tokens — but only at a token's edge, so `hello!` stays a word.
fn join_operators(tokens: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut pending_bang = false;
    let mut want_alt = false;
    for t in tokens {
        // A quoted run is literal all the way through.
        let quoted = t.starts_with('"');
        // Only `|` joins; `split_semicolons` has already cut on `;`.
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
        // Once, not twice: `!!ext:rs` reads as no field at all.
        if std::mem::take(&mut pending_bang) && !t.starts_with('!') {
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
        // The field table is the only place a field name is written down.
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
            // Paths are compared as the filesystem stores them: folding would
            // make `under:` disagree with the tokens the index holds.
            Some("under") => (!raw.is_empty()).then(|| Match::Under(trim_dir(&raw))),
            Some("parent") => (!raw.is_empty()).then(|| Match::ParentIs(trim_dir(&raw))),
            Some("file") => Some(Match::IsDir(false)),
            Some("folder") => Some(Match::IsDir(true)),
            Some("size") => parse_size(&folded),
            // Turkish spellings are aliases on purpose; see `Kind::from_name`.
            Some("kind") => Kind::from_name(&folded).map(|k| Match::Kind(k.to_vec())),
            Some("dm") => parse_time(TimeField::Modified, &folded, now),
            Some("dc") => parse_time(TimeField::Created, &folded, now),
            Some("da") => parse_time(TimeField::Accessed, &folded, now),
            // An index built without contents rejects this explicitly rather
            // than silently finding nothing.
            Some("content") => (!folded.is_empty()).then(|| Match::ContentContains(folded.clone())),
            Some("node") => parse_node(&folded),
            Some("perm") => parse_perm(&folded),
            // The four single-bit tests, named rather than written in octal.
            Some("suid") => Some(bits(0o4000, 0o4000, false)),
            Some("sgid") => Some(bits(0o2000, 0o2000, false)),
            Some("sticky") => Some(bits(0o1000, 0o1000, false)),
            Some("ww") => Some(bits(0o0002, 0, true)),
            Some("user") => resolve_owner(scour_core::NumField::Uid, &raw),
            Some("group") => resolve_owner(scour_core::NumField::Gid, &raw),
            Some("len") => {
                let (cmp, rest) = split_cmp(&folded);
                rest.trim()
                    .parse::<i64>()
                    .ok()
                    .map(|n| Match::NameLen(cmp, n))
            }
            // The raw text: the spelling is the whole question.
            Some("case") => (!raw.is_empty()).then(|| Match::NameContainsCased(raw.clone())),
            // A bare number means *equals*: `depth:3` reads as "three deep",
            // where `size:1mb` rightly reads as "at least".
            Some("depth") => {
                let (cmp, rest) = match split_cmp(&folded) {
                    (Cmp::Ge, r) if r == folded => (Cmp::Eq, r),
                    other => other,
                };
                rest.trim()
                    .parse::<i64>()
                    .ok()
                    .map(|n| Match::Depth(cmp, n))
            }
            // Folded, because it is matched against a folded name: `[A-Z]`
            // would never fire.
            Some("regex") => (!folded.is_empty()).then(|| Match::Regex(folded.clone())),
            _ => None,
        };
        // An unknown field or an unusable value becomes a search for the raw
        // text; dropping the term would turn a typo into "everything".
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
            // Quotes turn off wildcards, not which field the term is about.
            if text.contains('/') {
                Match::PathContains(text)
            } else {
                Match::NameContains(text)
            }
        } else {
            name_match(&text)
        },
    ))
}

/// `mode & mask` against `want`, or against zero when `any`.
fn bits(mask: i64, want: i64, any: bool) -> Match {
    Match::Bits {
        field: scour_core::NumField::Mode,
        mask,
        want,
        any,
    }
}

/// `node:` — the `S_IFMT` bits, named the way `find -type` names them.
fn parse_node(v: &str) -> Option<Match> {
    let want = match v {
        "f" | "file" | "dosya" => 0o100000,
        "d" | "dir" | "folder" | "klasor" => 0o040000,
        "l" | "link" | "symlink" | "bag" => 0o120000,
        "s" | "socket" | "soket" => 0o140000,
        "p" | "fifo" | "pipe" | "boru" => 0o010000,
        "b" | "block" | "blok" => 0o060000,
        "c" | "char" | "karakter" => 0o020000,
        _ => return None,
    };
    Some(bits(0o170000, want, false))
}

/// `perm:` — `find`'s three shapes: `644` exactly these bits, `-200` all of
/// these, `/222` any of these.
fn parse_perm(v: &str) -> Option<Match> {
    let (rest, all_of, any_of) = match v.as_bytes().first() {
        Some(b'-') => (&v[1..], true, false),
        Some(b'/') | Some(b'+') => (&v[1..], false, true),
        _ => (v, false, false),
    };
    let n = i64::from_str_radix(rest, 8).ok()?;
    if any_of {
        Some(bits(n, 0, true))
    } else if all_of {
        Some(bits(n, n, false))
    } else {
        // Only the permission bits: the type bits are `node:`'s business.
        Some(bits(0o7777, n, false))
    }
}

/// `user:` and `group:` — a number, or a name looked up in `/etc/passwd` on the
/// machine that owns the files.
fn resolve_owner(field: scour_core::NumField, raw: &str) -> Option<Match> {
    let name = raw.trim();
    if name.is_empty() {
        return None;
    }
    if let Ok(n) = name.parse::<i64>() {
        return Some(Match::Num(field, Cmp::Eq, n));
    }
    #[cfg(unix)]
    {
        let file = if field == scour_core::NumField::Uid {
            "/etc/passwd"
        } else {
            "/etc/group"
        };
        let text = std::fs::read_to_string(file).ok()?;
        for line in text.lines() {
            let mut parts = line.split(':');
            if parts.next() == Some(name)
                && let Some(id) = parts.nth(1).and_then(|s| s.parse::<i64>().ok())
            {
                return Some(Match::Num(field, Cmp::Eq, id));
            }
        }
    }
    None
}

fn name_match(text: &str) -> Match {
    // A term with a separator in it is about the path: no name holds a slash.
    // Nothing indexes paths, so this costs a column scan — 1.7 s over 2.7 M
    // rows — and there is no path glob: a wildcard here is matched as text.
    if text.contains('/') {
        return Match::PathContains(text.to_owned());
    }
    if text.contains('*') || text.contains('?') {
        Match::NameGlob(text.to_owned())
    } else {
        Match::NameContains(text.to_owned())
    }
}

/// Split on `mark`, ignoring any that falls inside a quoted run — a leading
/// quote is not enough of a guard, since `"x y" ;c` arrives as `"x y"|c`.
/// Shared with the highlighter, so the line cuts exactly where the parser does.
pub(crate) fn split_outside_quotes(s: &str, mark: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut quoted = false;
    let mut from = 0;
    for (i, c) in s.char_indices() {
        if c == '"' {
            quoted = !quoted;
        } else if c == mark && !quoted {
            out.push(&s[from..i]);
            from = i + c.len_utf8();
        }
    }
    out.push(&s[from..]);
    out
}

/// The field a whole token names, with any leading `!` set aside: `split_field`
/// alone reads `!ext` as not-a-field, and `!ext:rs;toml` then comes apart at the
/// semicolon into an exclusion OR a bare word — nearly everything.
fn token_field(s: &str) -> Option<(String, &str)> {
    split_field(s.strip_prefix('!').unwrap_or(s))
}

/// Split `field:value`. A name is at least two alphabetic characters, ASCII or
/// not; the length rule is what keeps the Windows `C:/Users` from being a field.
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

/// Split off a leading comparison operator; absent means `>=`, so `size:1mb`
/// reads as "at least a megabyte".
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

/// `>1mb`, `<=500kb`, `=0`. Multipliers are binary, as file managers report them.
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
mod paren_tests {
    use super::*;
    use scour_core::Match;

    fn parse_(q: &str) -> Ast {
        parse_at(q, 1_785_000_000)
    }

    #[test]
    fn parentheses_group_an_alternation_written_with_spaces() {
        let a = parse_("(rapor | belge) ext:pdf");
        assert_eq!(a.groups.len(), 2, "two things AND-ed: {a:?}");
        assert_eq!(a.groups[0].alts.len(), 2, "two alternatives");
        assert!(matches!(a.groups[1].alts[0].1, Match::Ext(_)));
        // And the same thing without the spaces still means the same thing.
        assert_eq!(parse_("(rapor|belge) ext:pdf"), a);
        assert_eq!(parse_("rapor|belge ext:pdf"), a);
    }

    #[test]
    fn a_parenthesis_in_a_filename_is_a_parenthesis() {
        for q in ["rapor (1).pdf", "IMG (2)", "(kopya)"] {
            let a = parse_(q);
            let text: Vec<&Match> = a.matches().collect();
            assert!(
                text.iter().any(|m| matches!(
                    m,
                    Match::NameContains(t) | Match::NameGlob(t) if t.contains('(')
                )),
                "{q:?} lost its parenthesis: {a:?}"
            );
        }
    }

    #[test]
    fn an_unclosed_parenthesis_is_text_too() {
        let a = parse_("(rapor|belge");
        assert!(
            a.matches().any(
                |m| matches!(m, Match::NameContains(t) | Match::NameGlob(t) if t.starts_with('('))
            ),
            "{a:?}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every alternative in the query, flattened.
    fn m(q: &str) -> Vec<(bool, Match)> {
        parse_at(q, 0)
            .groups
            .into_iter()
            .flat_map(|g| g.alts)
            .collect()
    }

    /// A `!` in front of a list does not take the list apart: `!ext:rs;toml`
    /// is one exclusion, not a negation OR-ed with a bare word.
    #[test]
    fn a_negated_list_stays_one_list() {
        assert_eq!(
            m("!ext:rs;toml"),
            vec![(true, Match::Ext(vec!["rs".into(), "toml".into()]))]
        );
        // One group, not two OR-ed alternatives.
        assert_eq!(parse_at("!ext:rs;toml", 0).groups.len(), 1);
        assert_eq!(parse_at("!ext:rs;toml", 0).groups[0].alts.len(), 1);
        // And across the space, which `join_lists` glues back together.
        assert_eq!(
            m("!ext:rs; toml"),
            vec![(true, Match::Ext(vec!["rs".into(), "toml".into()]))]
        );
        // The same for every other list field.
        assert_eq!(
            m("!kind:image;code").len(),
            1,
            "!kind:image;code came apart"
        );
        // What it must not break: the un-negated form, and `;` between words.
        assert_eq!(
            m("ext:rs;toml"),
            vec![(false, Match::Ext(vec!["rs".into(), "toml".into()]))]
        );
        assert_eq!(parse_at("a;b", 0).groups.len(), 2);
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
        assert_eq!(m("kind:kod"), vec![(false, Match::Kind(vec![Kind::Code]))]);
        assert_eq!(
            m("kind:KLASÖR"),
            vec![(false, Match::Kind(vec![Kind::Dir]))]
        );
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
        // The only two field spellings that are not ASCII.
        assert_eq!(m("tür:kod"), vec![(false, Match::Kind(vec![Kind::Code]))]);
        assert_eq!(m("TÜR:kod"), vec![(false, Match::Kind(vec![Kind::Code]))]);
        assert_eq!(
            m("içerik:x"),
            vec![(false, Match::ContentContains("x".into()))]
        );
        // `C:` is still not a field, and is a path term because of the slash.
        assert_eq!(
            m("C:/Users"),
            vec![(false, Match::PathContains("c:/users".into()))]
        );
    }

    /// A term with a separator in it asks about the path: no name holds a slash.
    #[test]
    fn a_term_with_a_slash_in_it_is_about_the_path() {
        assert_eq!(
            m("Projeler/Scour"),
            vec![(false, Match::PathContains("projeler/scour".into()))]
        );
        // Without one it is a name, as before.
        assert_eq!(
            m("Scour"),
            vec![(false, Match::NameContains("scour".into()))]
        );
        // Quoted, it is still a path term: quotes only turn off wildcards.
        assert_eq!(
            m("\"Projeler/Scour\""),
            vec![(false, Match::PathContains("projeler/scour".into()))]
        );
    }

    #[test]
    fn a_field_that_cannot_be_read_becomes_plain_text() {
        // The rule that keeps a half-typed query usable, and keeps Windows
        // paths and URLs from being read as fields.
        assert_eq!(
            m("http://x"),
            vec![(false, Match::PathContains("http://x".into()))]
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
            vec![(false, Match::PathContains("c:/users".into()))]
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

#[cfg(test)]
mod list_separator_tests {
    use super::*;

    fn one(q: &str) -> Vec<(bool, Match)> {
        parse_at(q, 0)
            .groups
            .into_iter()
            .flat_map(|g| g.alts)
            .collect()
    }

    fn exts(q: &str) -> Vec<String> {
        match one(q).into_iter().next() {
            Some((_, Match::Ext(e))) => e,
            other => panic!("{q} parsed to {other:?}"),
        }
    }

    #[test]
    fn a_separator_may_be_written_with_spaces_around_it() {
        // The three spellings a person actually types, all one filter.
        let want = vec!["rs".to_owned(), "toml".to_owned()];
        assert_eq!(exts("ext:rs;toml"), want);
        assert_eq!(exts("ext:rs ; toml"), want);
        assert_eq!(exts("ext:rs; toml"), want);
        assert_eq!(exts("ext:rs ;toml"), want);
        assert_eq!(exts("ext:rs  ;  toml"), want, "and more than one space");
    }

    #[test]
    fn a_longer_list_survives_the_same_treatment() {
        assert_eq!(
            exts("ext:rs ; toml ; md"),
            vec!["rs".to_owned(), "toml".to_owned(), "md".to_owned()]
        );
    }

    #[test]
    fn the_rest_of_the_query_is_untouched() {
        let got = parse_at("rapor ext:rs ; toml dm:7d", 0);
        assert_eq!(got.groups.len(), 3, "name, extension, date");
    }

    #[test]
    fn a_semicolon_between_words_is_and() {
        // Between terms `;` is a space typed without pressing space: both.
        // Inside a field's value it stays "any of these".
        let want = vec![
            vec![(false, Match::NameContains("rapor".into()))],
            vec![(false, Match::NameContains("pdf".into()))],
        ];
        for q in ["rapor ; pdf", "rapor;pdf", "rapor pdf"] {
            assert_eq!(
                parse_at(q, 0)
                    .groups
                    .iter()
                    .map(|g| g.alts.clone())
                    .collect::<Vec<_>>(),
                want,
                "{q:?}"
            );
        }
    }

    /// `|` is "either of these"; `;` is "and also".
    #[test]
    fn a_pipe_is_either_and_a_semicolon_is_both() {
        let either = |q: &str| {
            let ast = parse_at(q, 0);
            (ast.groups.len(), ast.groups[0].alts.len())
        };
        assert_eq!(either("hasan|genel"), (1, 2), "`|` is either of them");
        assert_eq!(either("hasan;genel"), (2, 1), "`;` is both of them");
        assert_eq!(either("hasan genel"), (2, 1), "and a space is the same");
        // Written with spaces around it, or without, or several at once.
        for q in ["a;b;c", "a ; b ; c", "a b c"] {
            assert_eq!(parse_at(q, 0).groups.len(), 3, "{q:?}");
        }
        // A field's own list is untouched: two values are not two terms.
        assert_eq!(
            one("ext:rs;toml"),
            vec![(false, Match::Ext(vec!["rs".into(), "toml".into()]))]
        );
        assert_eq!(parse_at("ext:rs;toml", 0).groups.len(), 1);
        // Beside a `|` it says nothing more: the operator decides how the
        // terms combine, the separator only where one ends.
        assert_eq!(either("a|;c"), (1, 2));
        assert_eq!(either("a | ;c"), (1, 2));
    }

    #[test]
    fn a_separator_inside_quotes_is_an_ordinary_character() {
        assert_eq!(
            one("\"a ; b\""),
            vec![(false, Match::NameContains("a ; b".into()))]
        );
    }
}
