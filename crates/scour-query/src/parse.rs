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
    for token in join_parens(join_operators(split_bangs(split_semicolons(join_lists(
        tokenize(input),
    )))))
    .into_iter()
    .flat_map(|t| expand(&t))
    {
        // Alternatives split on `|`. Quoted runs are already protected, so a
        // pipe inside quotes is a literal character.
        // **`;` is `|` outside a field's value**, and that is one rule rather
        // than two. `ext:rs;toml` already means "extension is rs *or* toml";
        // a mark that means "any of these" inside a value and something else
        // between words is the inconsistency, not the fix. Reported as
        // `OPUS ; SONNET` finding neither, which is what three AND-ed terms —
        // one of them a literal semicolon — correctly finds.
        //
        // A field's value keeps its own semicolons: `join_lists` has already
        // glued them on, and splitting here would take `ext:rs;toml` apart.
        // A quoted run is literal all the way through — `"a ; b"` is one
        // phrase and the semicolon in it is a character. The same guard every
        // other stage here uses, and forgetting it took the phrase apart.
        // Only `|` makes alternatives. A `;` between words is a separator —
        // `split_semicolons` has already cut the token there — and the only
        // semicolons left are a field's own list and the ones inside quotes.
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
/// already say.
///
/// **Text in, text out, and that is the point.** A term like `size:1mb..2mb`
/// is two comparisons AND-ed, and `empty:` is a file of no bytes; both are
/// sentences this parser already understands, so the honest way to add them
/// is to write those sentences rather than to grow the tree. Everything a
/// rewrite produces can be typed by hand, which is also what makes it
/// explainable — `explain` reads back the expansion, so nothing is happening
/// that the user cannot see.
///
/// The spellings are Everything's, because somebody arriving from it has a
/// decade of muscle memory and no reason to relearn any of this.
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
        // A folder's size is stored as zero and its child count is not stored
        // at all, so "empty" can only honestly mean a file of no bytes. Saying
        // that is better than a folder rule that would match every folder.
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

/// Glue `( a | b )` into one token, so an alternation may be spelled with
/// spaces in it the way every shell and `find` allows.
///
/// **Only when the run contains a `|`.** A parenthesis is an ordinary
/// character in a filename and a very common one — `rapor (1).pdf`, `IMG (2)`
/// — so treating every one of them as syntax would break searching for the
/// files people actually have. Grouping is what parentheses are *for* here;
/// anywhere else they are text, and this is the rule that keeps both true.
///
/// Nesting is not supported and the shape of the AST is why: a query is
/// groups AND-ed together and a group is alternatives OR-ed, which is one
/// level by construction. `(a|b) (c|d)` works and says a great deal;
/// `(a (b|c))` would need a tree, and the day something needs one it should
/// get a tree rather than a parser that pretends.
fn join_parens(tokens: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut i = 0;
    while i < tokens.len() {
        // `<a|b>` as well as `(a|b)`: Everything groups with angle brackets,
        // and the same rule applies to both — they are syntax only when they
        // hold an alternation, because both are ordinary characters in names.
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

/// Cut a token at every `;` that separates two terms.
///
/// **`;` is a space somebody typed without pressing space.** It used to be
/// `|` — "either of these" — and that is what made `hasan;genel` answer with
/// every file called `hasan`: one of the two words was enough. Between terms
/// it now means what a space means, which is *both*.
///
/// Inside a field's value it goes on meaning "any of these", and that is not
/// the same mark used two ways: `ext:rs;toml` is one filter with a list in
/// it, and a list of extensions can only ever be an "any". Two terms are a
/// different thing from two values.
///
/// After `join_lists`, so a value written across a space — `ext:rs ; toml` —
/// is already one token by the time this looks; and before `split_bangs`, so
/// that `a;!b` reaches it as `a` and `!b`.
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

/// Cut a token where a `!` starts a new term inside it.
///
/// **After `join_lists`, so that a value continued across a space is already
/// one token.** Before it, `ext:rs ; f!g` had its last token cut into `f` and
/// `!g` while the list was still open, and the line — which glues first — read
/// the same query as one filter. A `!` inside a field's value is a character,
/// wherever the value happens to have been written.
///
/// **A separator that needs a space in front of it is a separator people get
/// wrong.** `;` and `|` have never needed one — `a;b` is two alternatives —
/// and `!` did: `rapor!tmp` was the literal string, so the exclusion silently
/// did nothing. Nobody types a `!` in the middle of a word by accident.
///
///
/// Three places a `!` is *not* a separator, and each is a real query:
///
/// * **At the end**, `hello!` — there is nothing to exclude, and files are
///   called that. Long-standing, and unchanged.
/// * **Just after `;` or `|`**, `a|!b` — the `!` belongs to the alternative
///   that is starting, not to a new term. Cutting there would break the `or`.
/// * **Inside a field's value or a quoted run**, `path:/home/a!b` and
///   `"a!b"` — both are literal all the way through, and a path may contain
///   anything.
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
/// Close up the spaces around a `;` inside a field's value.
///
/// `;` separates the values of a list field — `ext:rs;toml` — and space
/// separates terms, so `ext:rs ; toml` used to be three terms: an extension
/// filter, a search for the literal text ";", and a search for "toml". Nobody
/// means that, and a search box that puts breathing room around its separators
/// (which is what makes a long query readable) would produce it constantly.
///
/// Only after something that is already a field. `rapor ; pdf` stays two terms
/// and a stray semicolon, because there is no list there to extend and joining
/// them would invent one.
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
        // Only a real field's value can be continued, on either side of the
        // separator. Without this condition on *both*, `rapor ; pdf` became
        // `rapor;` and `pdf` — a list invented where there was none.
        let after_field = out.last().is_some_and(|p| token_field(p).is_some());
        if after_field && (open || t.starts_with(';')) {
            let prev = out.last_mut().expect("checked by after_field");
            // The spaces go; the separator stays exactly once, however many
            // sides of it it was written on.
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

fn join_operators(tokens: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::with_capacity(tokens.len());
    let mut pending_bang = false;
    let mut want_alt = false;
    for t in tokens {
        // A quoted run is literal all the way through.
        let quoted = t.starts_with('"');
        // **Only `|` joins.** `;` used to as well, and that is what made
        // `hasan;genel` find everything called `hasan`: one of the two words
        // was enough. It is a separator now — the same thing a space is —
        // and `split_semicolons` has already cut the token there.
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
        // **Once, not twice.** `! !ext:rs;toml` prepended a second `!` and
        // made `!!ext:rs`, which is not a field term to anything that looks
        // — so the list came apart at the semicolon and the exclusion turned
        // into an `or` again. A second `!` says nothing the first did not.
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
            Some("kind") => Kind::from_name(&folded).map(|k| Match::Kind(k.to_vec())),
            Some("dm") => parse_time(TimeField::Modified, &folded, now),
            Some("dc") => parse_time(TimeField::Created, &folded, now),
            Some("da") => parse_time(TimeField::Accessed, &folded, now),
            // Reserved from the first day so the language does not have to
            // change shape when content indexing arrives. An index built
            // without contents rejects it explicitly rather than silently
            // finding nothing.
            Some("content") => (!folded.is_empty()).then(|| Match::ContentContains(folded.clone())),
            Some("node") => parse_node(&folded),
            Some("perm") => parse_perm(&folded),
            // The four that are one bit each, spelled the way a person says
            // them rather than in octal.
            Some("suid") => Some(bits(0o4000, 0o4000, false)),
            Some("sgid") => Some(bits(0o2000, 0o2000, false)),
            Some("sticky") => Some(bits(0o1000, 0o1000, false)),
            Some("ww") => Some(bits(0o0002, 0, true)),
            Some("user") => resolve_owner(scour_core::NumField::Uid, &raw),
            Some("group") => resolve_owner(scour_core::NumField::Gid, &raw),
            // **A bare number means *equals* here, not "at least".**
            //
            // `size:1mb` meaning "at least" is right — nobody looks for a file
            // of exactly one megabyte. Depth is the opposite: `depth:3` reads
            // as "three deep" to everyone, and taking the size convention made
            // it match almost the whole index while `depth:<=3` matched 77.
            // Both were working as written; one of them was written wrong.
            Some("len") => {
                let (cmp, rest) = split_cmp(&folded);
                rest.trim()
                    .parse::<i64>()
                    .ok()
                    .map(|n| Match::NameLen(cmp, n))
            }
            // The raw text, not the folded one: the whole point is the
            // spelling, so folding it first would be folding the question away.
            Some("case") => (!raw.is_empty()).then(|| Match::NameContainsCased(raw.clone())),
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
            // The pattern is folded, not the raw text: it is matched against a
            // folded name, so `[A-Z]` would never fire and `İ` has to reach
            // the same letter `i` does.
            Some("regex") => (!folded.is_empty()).then(|| Match::Regex(folded.clone())),
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
            // Quotes turn off wildcards; they do not change which field the
            // term is about. `"Projeler/Scour"` is still a path.
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

/// `perm:` — the three shapes `find` has, and for the same reasons.
///
/// `644` is *exactly these bits*, `-200` is *all of these*, `/222` is *any of
/// these*. The last is the one an audit actually wants: "anything a stranger
/// can write to" is `perm:/222`, not a list of the modes that would allow it.
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
        // Exact, and only over the permission bits — the type bits are
        // `type:`'s business and nobody writes `perm:100644`.
        Some(bits(0o7777, n, false))
    }
}

/// `user:` and `group:` — a number, or a name looked up on this machine.
///
/// Resolved here because here is where the index is: the service runs on the
/// machine that owns the files, so `/etc/passwd` is the right answer to "who
/// is `root`". A client on another machine asking by name would be asking
/// about its own users, which is not what it means.
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
    // **A term with a separator in it is about the path.** No name holds a
    // slash, so `Projeler/Scour` typed on its own matched nothing at all —
    // which is the one thing somebody typing a fragment of a path is certain
    // not to mean. Everything has the same rule and for the same reason.
    //
    // A path term costs what a scan of the path column costs — 1.7 s over 2.7
    // million rows, measured — because nothing indexes paths. That is the
    // price of asking about them, and it is paid only when a slash is typed.
    // There is no path glob, so a wildcard in a path term is matched as text:
    // `Projeler/*.rs` asks for paths holding that, which is not what a glob
    // would say but is closer to it than nothing.
    if text.contains('/') {
        return Match::PathContains(text.to_owned());
    }
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
/// Split on `mark`, ignoring any that falls inside a quoted run.
///
/// **A quoted run is literal all the way through**, and the alternative split
/// never saw the quotes: `"a|b"` came out as two alternatives, so a phrase
/// with a pipe in it searched for something else entirely. Guarding by "does
/// the token start with a quote" is not enough either — `join_operators`
/// appends an alternative to whatever came before, so `"x y" ;c` arrives here
/// as `"x y"|c`, which starts with a quote and is two things.
///
/// Shared with the highlighter, so that the line cannot cut a term anywhere
/// the parser does not.
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

/// The field a whole **token** names, with any leading `!` set aside.
///
/// **`!ext:rs` is a field term, and `split_field` says it is not.** It reads
/// the name as `!ext` — which is not all letters, therefore not a field — so
/// every question of the form "is this a list I have to keep whole?" answered
/// no the moment the term was negated. `!ext:rs;toml` was then cut at the
/// semicolon into an exclusion and a bare word, OR-ed together:
///
/// ```text
/// !ext:rs;toml   →  (not extension is .rs or name contains "toml")
/// ```
///
/// which is very nearly "everything", from a term that reads as a narrowing.
/// The same for `!kind:image;code`, and for `!ext:rs; toml` across the space.
///
/// So: ask about the token, not about the string. `split_field` still answers
/// about a value being parsed, where the `!` has already been taken off.
fn token_field(s: &str) -> Option<(String, &str)> {
    split_field(s.strip_prefix('!').unwrap_or(s))
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
        // The case that made the rule: these files exist on every machine.
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

    /// Every alternative in the query, flattened — most assertions here are
    /// about what a token turned into, not about grouping.
    fn m(q: &str) -> Vec<(bool, Match)> {
        parse_at(q, 0)
            .groups
            .into_iter()
            .flat_map(|g| g.alts)
            .collect()
    }

    /// A `!` in front of a list does not take the list apart.
    ///
    /// **The failure was silent and it went the wrong way.** `!ext:rs;toml`
    /// reads as a narrowing and became `(not extension is .rs) or (name
    /// contains "toml")` — a disjunction with a negation in it, which matches
    /// very nearly every file there is. The cause: `split_field` was asked
    /// about the token with its `!` still on, said "not a field", and the
    /// semicolon was therefore read as the alternative separator it is
    /// outside a field's value.
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
        // What it must not break: the un-negated form, and a `;` between
        // words, which separates two terms and has to go on doing that.
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
        // `tür:` and `içerik:` were listed as aliases but could never match,
        // because the field-name rule demanded ASCII. They are the only two
        // fields whose Turkish spelling is not ASCII, so nothing else was hit.
        assert_eq!(m("tür:kod"), vec![(false, Match::Kind(vec![Kind::Code]))]);
        assert_eq!(m("TÜR:kod"), vec![(false, Match::Kind(vec![Kind::Code]))]);
        assert_eq!(
            m("içerik:x"),
            vec![(false, Match::ContentContains("x".into()))]
        );
        // The rule that made it worth having is untouched: `C:` is not read
        // as a field. It is a path term rather than a name term because it
        // has a separator in it — which is what a Windows path is.
        assert_eq!(
            m("C:/Users"),
            vec![(false, Match::PathContains("c:/users".into()))]
        );
    }

    /// **A term with a separator in it asks about the path.**
    ///
    /// No name holds a slash, so this used to be the one shape of query that
    /// could not match anything: somebody typing a piece of a path they could
    /// see on the screen got nothing back.
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
        // Quoted, it is still a path term: the quotes turn off wildcards,
        // not the meaning of a separator.
        assert_eq!(
            m("\"Projeler/Scour\""),
            vec![(false, Match::PathContains("projeler/scour".into()))]
        );
    }

    #[test]
    fn a_field_that_cannot_be_read_becomes_plain_text() {
        // This is the rule that keeps a half-typed query usable, and keeps
        // Windows paths and URLs from being mistaken for fields.
        // Still text rather than a field, and a path term because of the
        // slashes. Neither reading finds anything — no path holds `//` — and
        // what matters here is that it parses at all.
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
        // **This has been all three things, and each time a user found the
        // one before.** First a literal character: `OPUS ; SONNET` found
        // neither, because it was three terms AND-ed and one of them was a
        // semicolon. Then `|`, for consistency with `ext:rs;toml` — and that
        // made `hasan;genel` answer with every file called `hasan`, because
        // one of the two words was enough.
        //
        // It is a separator: the thing a space is, typed without pressing
        // space. Between terms that means **both**. Inside a field's value it
        // goes on meaning "any of these", and that is not the same mark used
        // two ways — a list of extensions can only ever be an "any", and two
        // terms are a different thing from two values.
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

    /// The two marks say different things, and that is the point.
    ///
    /// `|` is "either of these"; `;` is "and also". Reported as
    /// `hasan;genel` answering with every file called `hasan` — one word out
    /// of two being enough — where what was wanted was both.
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
        // A field's own list is untouched: a list of extensions can only be
        // an "any", and two values are not two terms.
        assert_eq!(
            one("ext:rs;toml"),
            vec![(false, Match::Ext(vec!["rs".into(), "toml".into()]))]
        );
        assert_eq!(parse_at("ext:rs;toml", 0).groups.len(), 1);
        // And beside a `|` it says nothing the `|` has not: the operator
        // decides how the terms combine, the separator only where one ends.
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
