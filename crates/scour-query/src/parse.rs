//! Text to [`Ast`].
//!
//! The parser never fails: a query halfway through being typed — `size:>`, a
//! lone opening quote — must still read as something. An unknown field, or one
//! whose value will not parse, becomes a search for the raw text.

use scour_core::text::DefaultFolder;
use scour_core::{Ast, Cmp, Group, Match, TimeField};

use crate::time::{now_secs, parse_time};

/// Parse query text against the current clock, with `~` in `under:` and
/// `parent:` standing for the home directory.
pub fn parse(input: &str) -> Ast {
    parse_in(input, now_secs(), home().as_deref())
}

/// Parse query text as though `now` were the current unix time; relative
/// windows (`dm:7d`) resolve against it, which is what makes them testable.
/// No home is known here, so `~` stays literal.
pub fn parse_at(input: &str, now: i64) -> Ast {
    parse_in(input, now, None)
}

/// The full form: the clock, and the directory `~` expands to, if any.
pub fn parse_in(input: &str, now: i64, home: Option<&str>) -> Ast {
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
            .filter_map(|a| parse_alt(a, now, home))
            .collect();
        if !alts.is_empty() {
            groups.push(Group { alts });
        }
    }
    Ast {
        groups,
        ..Ast::default()
    }
}

/// The query with every plain word long enough to be forgiven written `fuzzy:` —
/// what [`Ast::forgiving`] makes of it, as text a person can read and change.
/// `None` when no word is.
pub fn forgiving(input: &str) -> Option<String> {
    let mut changed = false;
    let mut out: Vec<String> = Vec::new();
    for token in join_parens(join_operators(split_bangs(split_semicolons(join_lists(
        tokenize(input),
    )))))
    .into_iter()
    .flat_map(|t| expand(&t))
    {
        let alts: Vec<String> = split_outside_quotes(&token, '|')
            .into_iter()
            .filter(|a| !a.is_empty())
            .map(|a| match parse_alt(a, 0, None) {
                Some((false, Match::NameContains(t))) if scour_core::mistakes_allowed(&t) > 0 => {
                    changed = true;
                    format!("fuzzy:{a}")
                }
                _ => a.to_owned(),
            })
            .collect();
        if !alts.is_empty() {
            out.push(alts.join("|"));
        }
    }
    changed.then(|| out.join(" "))
}

/// The home directory as the index spells paths: forward slashes, no trailing one.
fn home() -> Option<String> {
    let h = std::env::home_dir()?;
    let h = h.to_string_lossy().replace('\\', "/");
    let h = h.trim_end_matches('/');
    (!h.is_empty()).then(|| h.to_owned())
}

/// Rewrite the spellings that are shorthand for something the language can
/// already say. Text in, text out, so `explain` reads back what actually ran.
fn expand(token: &str) -> Vec<String> {
    // A quoted run is literal all the way through.
    if token.starts_with('"') {
        return vec![token.to_owned()];
    }
    let (negated, body) = match token.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, token),
    };
    // Negated as a whole: not (a and b) is `!a|!b`, one group.
    let with = |parts: &[&str]| -> Vec<String> {
        if negated {
            let alts: Vec<String> = parts.iter().map(|p| format!("!{p}")).collect();
            vec![alts.join("|")]
        } else {
            parts.iter().map(|p| (*p).to_owned()).collect()
        }
    };
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
/// what a space means, both, while inside a list field's value (`ext:`, `kind:`)
/// it stays "any of these". After `join_lists`, which glues `ext:rs ; toml`,
/// and before `split_bangs`, so `a;!b` reaches it as `a` and `!b`.
fn split_semicolons(tokens: Vec<String>) -> Vec<String> {
    let mut out = Vec::with_capacity(tokens.len());
    for t in tokens {
        for piece in cut_semicolons(&t) {
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

/// Close up the spaces around a `;` inside a list field's value (`ext:`, `kind:`),
/// so `ext:rs ; toml` is one filter while `size:>1mb ; dm:7d` stays two.
fn join_lists(tokens: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut open = false;
    for t in tokens {
        let after_list = out.last().is_some_and(|p| is_openable_list(p));
        if after_list && can_join_list(&t) && (open || t.starts_with(';')) {
            let prev = out.last_mut().expect("checked by after_list");
            if !prev.ends_with(';') {
                prev.push(';');
            }
            prev.push_str(t.trim_start_matches(';'));
        } else {
            out.push(t);
        }
        let last = out.last().map(String::as_str).unwrap_or("");
        open = last.ends_with(';') && is_openable_list(last);
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
            want_alt |= trailing_alt;
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

fn parse_alt(raw: &str, now: i64, home: Option<&str>) -> Option<(bool, Match)> {
    let (negated, rest) = match raw.strip_prefix('!') {
        Some(r) => (true, r),
        None => (false, raw),
    };
    if rest.is_empty() {
        return None;
    }
    // **A `:` starting a filter is a filter not chosen yet**, and is left out:
    // the bar's `+` and its filter button write one and open the list of
    // fields, and the letters after it narrow that list (`:d`, `:kon`). Searched
    // for as text it found nothing, and the list went empty under the menu.
    // `":"` in quotes is still the character.
    if is_launcher(rest) {
        return None;
    }

    if let Some((field, value)) = split_field(rest) {
        let raw = unquote(value);
        let folded = DefaultFolder::of(&raw);
        // **A filter with nothing after its colon is not written yet**, and
        // is left out rather than searched for as text: the bar writes `dm:`
        // or `content:""` and opens a lens under it, and the list has to go on
        // answering for the rest of the query until a value is chosen there.
        // `file:` and `folder:` take nothing and are complete as written.
        if folded.trim().is_empty()
            && crate::fields::lookup(&field)
                .is_some_and(|f| f.takes != crate::fields::Takes::Nothing)
        {
            return None;
        }
        // The field table is the only place a field name is written down.
        let m: Option<Match> = match crate::fields::lookup(&field).map(|f| f.name) {
            Some("ext") => {
                let exts: Vec<String> = folded
                    .split(';')
                    .map(|e| e.trim_start_matches('.'))
                    .filter(|e| !e.is_empty())
                    .map(str::to_owned)
                    .collect();
                (!exts.is_empty()).then_some(Match::Ext(exts))
            }
            Some("path") => Some(Match::PathContains(folded)),
            // Paths are compared as the filesystem stores them: folding would
            // make `under:` disagree with the tokens the index holds.
            Some("under") => (!raw.is_empty()).then(|| Match::Under(trim_dir(&raw, home))),
            Some("parent") => (!raw.is_empty()).then(|| Match::ParentIs(trim_dir(&raw, home))),
            Some("file") => Some(Match::IsDir(false)),
            Some("folder") => Some(Match::IsDir(true)),
            Some("size") => parse_size(&folded),
            // Turkish spellings are aliases on purpose; see `Kind::from_name`.
            Some("kind") => crate::fields::parse_kinds(&folded).map(Match::Kind),
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
            // A term too short to be forgiven anything is an ordinary one.
            Some("fuzzy") => {
                (!folded.is_empty()).then(|| match scour_core::mistakes_allowed(&folded) {
                    0 => Match::NameContains(folded.clone()),
                    edits => Match::NameNear(folded.clone(), edits),
                })
            }
            Some("letters") => (!folded.is_empty()).then(|| Match::NameLetters(folded.clone())),
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

/// The list field (`ext:` or `kind:`) a token opens, if any.
pub(crate) fn list_field(s: &str) -> Option<&'static crate::fields::Field> {
    if s.starts_with('"') {
        return None;
    }
    let (name, _) = token_field(s)?;
    crate::fields::lookup(&name).filter(|f| f.takes.is_list())
}

/// Whether `token` is a pure list-field run that has not yet crossed `|` or `;!`.
fn is_openable_list(token: &str) -> bool {
    list_field(token).is_some()
        && split_outside_quotes(token, '|').len() == 1
        && split_list_token(token).1.is_none()
}

/// Whether one `;`-separated piece (before any `|`) can stay inside an `ext:`
/// or `kind:` value list rather than starting a new term.
pub(crate) fn is_list_piece(piece: &str) -> bool {
    if piece.is_empty() {
        return true;
    }
    if piece.starts_with('"') {
        return false;
    }
    let head = split_outside_quotes(piece, '|')
        .into_iter()
        .next()
        .unwrap_or("");
    !head.is_empty()
        && !head.starts_with(['!', '"', '(', '<'])
        && !head.contains([':', '/', '*', '?', '!'])
}

/// Whether `token` can continue an open `ext:` or `kind:` list across a space.
pub(crate) fn can_join_list(token: &str) -> bool {
    let trimmed = token.trim_start_matches(';');
    if trimmed.is_empty() {
        return true;
    }
    split_outside_quotes(trimmed, ';')
        .into_iter()
        .next()
        .is_some_and(is_list_piece)
}

/// Split a list run at the first unquoted `;` (before any `|`) after its first
/// piece whose following piece starts a new term.
pub(crate) fn split_list_token(token: &str) -> (&str, Option<&str>) {
    let mut quoted = false;
    let mut from = 0;
    let mut skip_first = true;
    for (i, c) in token.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '|' if !quoted => break,
            ';' if !quoted => {
                if !skip_first && !is_list_piece(&token[from..i]) {
                    return (&token[..from - 1], Some(&token[from..]));
                }
                skip_first = false;
                from = i + 1;
            }
            _ => {}
        }
    }
    if !skip_first && !is_list_piece(&token[from..]) {
        return (&token[..from - 1], Some(&token[from..]));
    }
    (token, None)
}

/// Cut `s` at every `;` that separates two terms while keeping `;` inside
/// quotes and inside `ext:` / `kind:` value lists. Joins back with `;` to `s`.
pub(crate) fn cut_semicolons(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut cur = s;
    loop {
        if list_field(cur).is_some() {
            let (head, rest) = split_list_token(cur);
            out.push(head);
            match rest {
                Some(r) => cur = r,
                None => break,
            }
            continue;
        }
        let mut quoted = false;
        let mut cut = None;
        for (i, c) in cur.char_indices() {
            if c == '"' {
                quoted = !quoted;
            } else if c == ';' && !quoted {
                cut = Some(i);
                break;
            }
        }
        match cut {
            Some(i) => {
                out.push(&cur[..i]);
                cur = &cur[i + 1..];
            }
            None => {
                out.push(cur);
                break;
            }
        }
    }
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
/// A filter being started: `:`, and perhaps the letters choosing one, with no
/// second `:` — the same term the bar's list of fields opens on.
pub(crate) fn is_launcher(term: &str) -> bool {
    term.strip_prefix(':')
        .is_some_and(|rest| !rest.contains([':', '"']))
}

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
/// A directory as typed, spelt the index's way; a lone `~` or a leading `~/`
/// becomes `home` when one is known. `~x` is a folder named `~x`.
fn trim_dir(s: &str, home: Option<&str>) -> String {
    let t = s.replace('\\', "/");
    let trimmed = t.trim_end_matches('/');
    if trimmed.is_empty() {
        return "/".to_owned();
    }
    match (home, trimmed.strip_prefix("~/")) {
        (Some(h), _) if trimmed == "~" => h.to_owned(),
        (Some(h), Some(rest)) => format!("{h}/{rest}"),
        _ => trimmed.to_owned(),
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
mod tests;

#[cfg(test)]
mod list_separator_tests;
