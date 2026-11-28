//! AsciiDoc as a flow, read as Asciidoctor and pandoc's reader read it: the
//! header and its attributes, sections, paragraphs and their styles,
//! delimited blocks (listing, literal, example, sidebar, quote, verse,
//! open, passthrough), admonitions, nested lists by their markers, check
//! lists, callouts, description lists, tables (cells that span, cells of
//! AsciiDoc, CSV and DSV), block macros, and inline: constrained and
//! unconstrained marks, links, cross references, macros, attribute
//! references and footnotes, gathered at the end.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;

use scour_flow::Flow;

use crate::build::Builder;
use crate::inline::{
    Escape, MOST_DEPTH, Mark, Rules, bracketed, broken, dedent, flat, least_indent,
};

/// The document in `bytes`.
pub fn flow(bytes: &[u8]) -> Result<Flow, String> {
    let text = String::from_utf8_lossy(bytes).replace("\r\n", "\n");
    let lines: Vec<&str> = text.lines().collect();
    let mut doc = Doc::default();
    let mut b = Builder::default();
    doc.blocks(&lines, &mut b);
    doc.footnotes(&mut b);
    Ok(b.finish())
}

const PAIRS: &[(&str, &str, Mark)] = &[
    ("*", "*", Mark::Bold),
    ("_", "_", Mark::Italic),
    ("`", "`", Mark::Mono),
    ("#", "#", Mark::Plain),
    ("+", "+", Mark::Verbatim),
];
const LOOSE: &[(&str, &str, Mark)] = &[
    ("+++", "+++", Mark::Verbatim),
    ("++", "++", Mark::Verbatim),
    ("**", "**", Mark::Bold),
    ("__", "__", Mark::Italic),
    ("``", "``", Mark::Mono),
    ("##", "##", Mark::Plain),
];
/// What a backslash makes plain: the marks and the openers of what is markup.
const ESCAPED: &str = "*_`#+^~[]{}<>'\"\\-.:|";

#[derive(Default)]
struct Doc {
    attrs: HashMap<String, String>,
    /// Footnotes as they are met: an id (or none), and the words.
    notes: RefCell<Vec<(String, String)>>,
    /// A block has been drawn: a `= Title` after it is not the header.
    begun: bool,
    /// Blocks inside blocks, now.
    depth: usize,
}

/// What lines of attributes and a title said about the block under them.
#[derive(Default, Clone)]
struct Meta {
    title: Option<String>,
    /// The first positional attribute, lowercase: `source`, `quote`, `NOTE`.
    style: String,
    /// The second and third: a listing's language, a quote's attribution.
    second: String,
    third: String,
    options: Vec<String>,
    named: HashMap<String, String>,
}

impl Meta {
    fn has(&self, option: &str) -> bool {
        self.options.iter().any(|o| o == option)
            || self
                .named
                .get("options")
                .is_some_and(|o| o.split(',').any(|o| o.trim() == option))
    }

    fn admonition(&self) -> Option<&'static str> {
        admonition(&self.style)
    }

    /// An attribute line's list, `[style#id.role%option,second,key=value]`,
    /// over what lines above it said.
    fn add(&mut self, list: &str) {
        let mut positional = 0;
        for item in split_quoted(list, ',') {
            let item = item.trim();
            if let Some((key, value)) = item.split_once('=')
                && !key.is_empty()
                && key
                    .chars()
                    .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
            {
                let value = value.trim().trim_matches('"');
                if key == "options" || key == "opts" {
                    self.options
                        .extend(value.split(',').map(|o| o.trim().to_owned()));
                } else {
                    self.named.insert(key.to_owned(), value.to_owned());
                }
                continue;
            }
            match positional {
                0 => {
                    let end = item.find(['#', '.', '%']).unwrap_or(item.len());
                    if end > 0 {
                        self.style = item[..end].to_ascii_lowercase();
                    }
                    let mut rest = &item[end..];
                    while let Some(c) = rest.chars().next() {
                        let next = rest[1..]
                            .find(['#', '.', '%'])
                            .map_or(rest.len(), |p| p + 1);
                        if c == '%' {
                            self.options.push(rest[1..next].to_owned());
                        }
                        rest = &rest[next..];
                    }
                }
                1 => self.second = item.trim_matches('"').to_owned(),
                2 => self.third = item.trim_matches('"').to_owned(),
                _ => {}
            }
            positional += 1;
        }
    }
}

fn admonition(style: &str) -> Option<&'static str> {
    Some(match style.to_ascii_lowercase().as_str() {
        "note" => "Note",
        "tip" => "Tip",
        "important" => "Important",
        "warning" => "Warning",
        "caution" => "Caution",
        _ => return None,
    })
}

/// `text` split at `sep` outside double quotes.
fn split_quoted(text: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut quoted = false;
    let mut start = 0;
    for (i, c) in text.char_indices() {
        match c {
            '"' => quoted = !quoted,
            _ if c == sep && !quoted => {
                out.push(&text[start..i]);
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(&text[start..]);
    out
}

/// The attributes every document has.
fn builtin(name: &str) -> Option<&'static str> {
    Some(match name {
        "blank" | "empty" => "",
        "sp" => " ",
        "nbsp" => "\u{a0}",
        "zwsp" => "\u{200b}",
        "wj" => "\u{2060}",
        "apos" => "'",
        "quot" => "\"",
        "lsquo" => "‘",
        "rsquo" => "’",
        "ldquo" => "“",
        "rdquo" => "”",
        "deg" => "°",
        "plus" => "+",
        "brvbar" => "¦",
        "vbar" => "|",
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "startsb" => "[",
        "endsb" => "]",
        "caret" => "^",
        "asterisk" => "*",
        "tilde" => "~",
        "backslash" => "\\",
        "backtick" => "`",
        "two-colons" => "::",
        "two-semicolons" => ";;",
        "cpp" => "C++",
        "pp" => "++",
        _ => return None,
    })
}

/// An attribute entry, `:name: value` or `:name!:`, as its name, whether it
/// unsets, and its value.
fn entry(t: &str) -> Option<(String, bool, &str)> {
    let rest = t.strip_prefix(':')?;
    let end = rest.find(':')?;
    let name = &rest[..end];
    let (name, unset) = match (name.strip_prefix('!'), name.strip_suffix('!')) {
        (Some(n), _) | (_, Some(n)) => (n, true),
        _ => (name, false),
    };
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
    {
        return None;
    }
    let value = &rest[end + 1..];
    if !value.is_empty() && !value.starts_with([' ', '\t']) {
        return None;
    }
    Some((name.to_ascii_lowercase(), unset, value.trim()))
}

/// A delimiter line: four or more of one of `-.=*_+/`, or `--`.
fn delimiter(t: &str) -> bool {
    t == "--"
        || (t.len() >= 4
            && t.chars()
                .next()
                .is_some_and(|c| "-.=*_+/".contains(c) && t.chars().all(|x| x == c)))
}

fn attribute_line(t: &str) -> bool {
    t.starts_with('[') && t.ends_with(']') && !t.starts_with("[[")
}

/// A section title, `== Title` or `## Title`, as its level and words.
fn section(t: &str) -> Option<(u8, &str)> {
    let c = t.chars().next().filter(|c| *c == '=' || *c == '#')?;
    let n = t.chars().take_while(|x| *x == c).count();
    let rest = t[n..].strip_prefix(' ')?;
    (n <= 6 && !rest.trim().is_empty()).then(|| {
        let words = rest.trim();
        // A closing run, `== Title ==`, is not the title's.
        let words = words.trim_end_matches(c).trim_end();
        (n as u8, if words.is_empty() { rest.trim() } else { words })
    })
}

/// A two-line title, its words over a line of `=`, `-`, `~`, `^` or `+`
/// as long as they are, give or take one: its level and words.
fn underlined<'a>(t: &'a str, under: Option<&&str>) -> Option<(u8, &'a str)> {
    let under = under?.trim_end();
    let c = under.chars().next()?;
    let level = match c {
        '=' => 1,
        '-' => 2,
        '~' => 3,
        '^' => 4,
        '+' => 5,
        _ => return None,
    };
    let words = t.trim();
    let (n, m) = (under.chars().count(), words.chars().count());
    (n >= 2
        && under.chars().all(|x| x == c)
        && n.abs_diff(m) <= 1
        && marker(t).is_none()
        && !delimiter(words)
        && !attribute_line(words)
        && !words.starts_with(['.', '/', '[', ':']))
    .then_some((level, words))
}

fn thematic(t: &str) -> bool {
    (t.len() >= 3 && t.chars().all(|c| c == '\''))
        || matches!(t, "---" | "- - -" | "***" | "* * *" | "<<<")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Star,
    Dash,
    Dot,
    Number,
    Letter,
    Callout,
}

/// A list item's line: its kind, its depth, its words, and the number it
/// gives.
fn marker(line: &str) -> Option<(Kind, usize, &str, Option<u64>)> {
    let t = line.trim_start();
    let run = |c: char| t.chars().take_while(|x| *x == c).count();
    let stars = run('*');
    if (1..=6).contains(&stars) && t[stars..].starts_with([' ', '\t']) {
        return Some((Kind::Star, stars, t[stars..].trim_start(), None));
    }
    if let Some(rest) = t.strip_prefix("- ") {
        return Some((Kind::Dash, 1, rest.trim_start(), None));
    }
    let dots = run('.');
    if (1..=5).contains(&dots) && t[dots..].starts_with([' ', '\t']) {
        return Some((Kind::Dot, dots, t[dots..].trim_start(), None));
    }
    let digits = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
    if digits > 0
        && let Some(rest) = t[digits..].strip_prefix(". ")
    {
        return Some((Kind::Number, 1, rest.trim_start(), t[..digits].parse().ok()));
    }
    let mut chars = t.chars();
    if let (Some(c), Some('.'), Some(' ')) = (chars.next(), chars.next(), chars.next())
        && c.is_ascii_lowercase()
    {
        return Some((
            Kind::Letter,
            1,
            t[3..].trim_start(),
            Some(u64::from(c as u8 - b'a' + 1)),
        ));
    }
    if let Some(rest) = t.strip_prefix('<')
        && let Some(end) = rest.find("> ")
        && end > 0
        && rest[..end].chars().all(|c| c.is_ascii_digit() || c == '.')
    {
        return Some((
            Kind::Callout,
            1,
            rest[end + 2..].trim_start(),
            rest[..end].parse().ok(),
        ));
    }
    None
}

/// A description list's term, `term:: definition`: the term, its depth,
/// and what follows it on the line.
fn term(t: &str) -> Option<(&str, usize, &str)> {
    if t.starts_with(':') || t.starts_with("//") {
        return None;
    }
    let bytes = t.as_bytes();
    let mut i = 0;
    while i + 1 < bytes.len() {
        let c = bytes[i];
        if (c == b':' || c == b';') && bytes[i + 1] == c {
            let mut j = i;
            while j < bytes.len() && bytes[j] == c {
                j += 1;
            }
            let run = j - i;
            let fits = if c == b';' { run == 2 } else { run <= 4 };
            if i > 0 && fits && (j == bytes.len() || bytes[j] == b' ' || bytes[j] == b'\t') {
                let depth = if c == b';' { 2 } else { run - 1 };
                return Some((t[..i].trim(), depth, t[j..].trim()));
            }
            i = j;
            continue;
        }
        i += 1;
    }
    None
}

/// A block macro, `name::target[attributes]`.
fn block_macro(t: &str) -> Option<(&str, &str, &str)> {
    let (name, rest) = t.split_once("::")?;
    if !name.chars().all(|c| c.is_ascii_alphanumeric()) || name.is_empty() {
        return None;
    }
    let open = rest.find('[')?;
    let (attrs, n) = bracketed(&rest[open..])?;
    (open + n == rest.len()).then_some((name, &rest[..open], attrs))
}

/// What an attribute list shows as words: its first item, quoted or not;
/// commas stay the words' unless named attributes follow.
fn shown(attrs: &str, split: bool) -> String {
    let a = attrs.trim();
    // Named attributes only, `alt=Sunset,width=300`: the `alt` one.
    if let Some(first) = split_quoted(a, ',').first()
        && first.contains('=')
    {
        return split_quoted(a, ',')
            .iter()
            .find_map(|item| item.trim().strip_prefix("alt="))
            .unwrap_or("")
            .trim_matches('"')
            .to_owned();
    }
    if let Some(q) = a.strip_prefix('"')
        && let Some(end) = q.find('"')
    {
        return q[..end].to_owned();
    }
    let text = match a.split_once(',') {
        Some((first, rest)) if split || rest.contains('=') => first,
        _ => a,
    };
    text.trim().trim_end_matches('^').to_owned()
}

/// An inline macro, `name:target[attributes]`, as its name, target,
/// attributes and length.
fn inline_macro(text: &str) -> Option<(&str, String, &str, usize)> {
    let colon = text.find(':')?;
    let name = &text[..colon];
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric()) {
        return None;
    }
    let rest = &text[colon + 1..];
    let (target, after) = if let Some(p) = rest.strip_prefix("pass:") {
        let (t, n) = bracketed(p)?;
        (t.to_owned(), 5 + n)
    } else if let Some(p) = rest.strip_prefix("++") {
        let end = p.find("++")?;
        (p[..end].to_owned(), end + 4)
    } else {
        let end = rest
            .find(|c: char| c == '[' || c.is_whitespace())
            .unwrap_or(rest.len());
        (rest[..end].to_owned(), end)
    };
    let (attrs, n) = bracketed(&rest[after..])?;
    Some((name, target, attrs, colon + 1 + after + n))
}

/// AsciiDoc's own inline constructs; `notes` gathers the footnotes.
fn special(
    text: &str,
    edge: bool,
    notes: &RefCell<Vec<(String, String)>>,
) -> Option<(usize, String, bool)> {
    // Anywhere, inside a word too.
    if let Some(rest) = text.strip_prefix("footnote:") {
        let open = rest.find('[')?;
        let id = &rest[..open];
        if id.contains(char::is_whitespace) {
            return None;
        }
        let (said, n) = bracketed(&rest[open..])?;
        let mut notes = notes.borrow_mut();
        let number = match notes.iter().position(|(k, _)| !id.is_empty() && k == id) {
            Some(at) => at + 1,
            None => {
                notes.push((id.to_owned(), said.to_owned()));
                notes.len()
            }
        };
        return Some(("footnote:".len() + open + n, format!("[{number}]"), false));
    }
    // A span's attributes before its `#`: `[.red]#Bonjour#`.
    if text.starts_with('[')
        && !text.starts_with("[[")
        && let Some((_, n)) = bracketed(text)
        && text[n..].starts_with('#')
    {
        return Some((n, String::new(), false));
    }
    // Sub- and superscript: `H~2~O`, `E=mc^2^`.
    if let Some(c) = text.chars().next().filter(|c| *c == '~' || *c == '^') {
        let rest = &text[1..];
        if let Some(end) = rest.find(c)
            && end > 0
            && !rest[..end].contains(char::is_whitespace)
        {
            return Some((end + 2, rest[..end].to_owned(), false));
        }
    }
    if !edge {
        return None;
    }
    for (open, close, left, right) in [("\"`", "`\"", '“', '”'), ("'`", "`'", '‘', '’')] {
        if let Some(rest) = text.strip_prefix(open)
            && let Some(end) = rest.find(close)
        {
            let len = open.len() + end + close.len();
            return Some((len, format!("{left}{}{right}", &rest[..end]), false));
        }
    }
    if let Some(rest) = text.strip_prefix("<<")
        && let Some(end) = rest.find(">>")
    {
        let inner = &rest[..end];
        let words = inner.split_once(',').map_or(inner, |(_, t)| t.trim());
        return Some((end + 4, words.to_owned(), true));
    }
    if let Some(rest) = text.strip_prefix("[[[")
        && let Some(end) = rest.find("]]]")
    {
        return Some((end + 6, format!("[{}]", &rest[..end]), false));
    }
    if let Some(rest) = text.strip_prefix("[[")
        && let Some(end) = rest.find("]]")
    {
        return Some((end + 4, String::new(), false));
    }
    if let Some(rest) = text.strip_prefix("anchor:") {
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '[')
            .unwrap_or(rest.len());
        let n = bracketed(&rest[end..]).map_or(0, |(_, n)| n);
        return Some((7 + end + n, String::new(), false));
    }
    if let Some((name, target, attrs, n)) = inline_macro(text) {
        let said = match name {
            "kbd" | "stem" | "latexmath" | "asciimath" | "indexterm2" => (attrs.to_owned(), false),
            "pass" => (attrs.to_owned(), false),
            "btn" => (format!("[{attrs}]"), false),
            "menu" => {
                let mut path = vec![target.clone()];
                path.extend(
                    attrs
                        .split('>')
                        .map(|s| s.trim().to_owned())
                        .filter(|s| !s.is_empty()),
                );
                (path.join(" › "), false)
            }
            "icon" | "indexterm" => (String::new(), false),
            "image" => {
                let alt = shown(attrs, true);
                (
                    format!("🖼 {}", if alt.is_empty() { &target } else { &alt }),
                    false,
                )
            }
            "link" | "mailto" | "xref" => {
                let words = shown(attrs, false);
                (if words.is_empty() { target } else { words }, true)
            }
            "https" | "http" | "ftp" | "irc" | "file" => {
                let words = shown(attrs, false);
                let url = format!("{name}:{target}");
                (if words.is_empty() { url } else { words }, true)
            }
            _ => return None,
        };
        return Some((n, said.0, said.1));
    }
    for scheme in ["https://", "http://", "ftp://", "file://"] {
        if text.starts_with(scheme) {
            let end = text
                .find(|c: char| c.is_whitespace() || "<>\"[".contains(c))
                .unwrap_or(text.len());
            let url = text[..end].trim_end_matches(['.', ',', ')', ';', ':', '!', '?']);
            return Some((url.len(), url.to_owned(), true));
        }
    }
    if let Some(rest) = text.strip_prefix('<')
        && let Some(end) = rest.find('>')
        && (rest.starts_with("http") || rest[..end].contains('@'))
        && !rest[..end].contains(char::is_whitespace)
    {
        return Some((end + 2, rest[..end].to_owned(), true));
    }
    // A bare address: `me@example.com`.
    let end = text
        .find(|c: char| !(c.is_alphanumeric() || "._%+-@".contains(c)))
        .unwrap_or(text.len());
    let word = text[..end].trim_end_matches('.');
    if let Some(at) = word.find('@')
        && at > 0
        && word[at + 1..].contains('.')
        && !word[at + 1..].contains('@')
    {
        return Some((word.len(), word.to_owned(), true));
    }
    None
}

/// A cell's specifier, `2+^.>s` before its `|`.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Spec {
    columns: usize,
    rows: usize,
    copies: usize,
    style: char,
}

impl Default for Spec {
    fn default() -> Self {
        Spec {
            columns: 1,
            rows: 1,
            copies: 1,
            style: 'd',
        }
    }
}

fn spec(s: &str) -> Option<Spec> {
    let mut sp = Spec::default();
    let number = |r: &str| {
        let d = r.find(|c: char| !c.is_ascii_digit()).unwrap_or(r.len());
        (r[..d].parse::<usize>().ok(), d)
    };
    let mut rest = s;
    let (a, n) = number(rest);
    if let Some(after) = rest[n..].strip_prefix('*') {
        sp.copies = a?.clamp(1, 64);
        rest = after;
    } else {
        let mut r = &rest[n..];
        let mut down = None;
        if let Some(after) = r.strip_prefix('.')
            && let (Some(m), k) = number(after)
        {
            down = Some(m);
            r = &after[k..];
        }
        match r.strip_prefix('+') {
            Some(after) => {
                sp.columns = a.unwrap_or(1).clamp(1, 64);
                sp.rows = down.unwrap_or(1).clamp(1, 1000);
                rest = after;
            }
            None if a.is_some() || down.is_some() => return None,
            None => rest = r,
        }
    }
    rest = rest.strip_prefix(['<', '^', '>']).unwrap_or(rest);
    if let Some(after) = rest.strip_prefix('.') {
        rest = after.strip_prefix(['<', '^', '>'])?;
    }
    if let Some(c) = rest.chars().next()
        && rest.len() == 1
        && "adehlmsv".contains(c)
    {
        sp.style = c;
        rest = "";
    }
    rest.is_empty().then_some(sp)
}

/// A prefix-separated table's cells: each with its specifier, its words,
/// and the line it starts on.
fn cells(body: &str, sep: char) -> Vec<(Spec, String, usize)> {
    let mut out = Vec::new();
    let mut open: Option<(Spec, usize, usize)> = None;
    let mut line = 0;
    let mut prev = '\n';
    for (pos, c) in body.char_indices() {
        let escaped = prev == '\\';
        prev = c;
        if c == '\n' {
            line += 1;
            continue;
        }
        if c != sep || escaped {
            continue;
        }
        // A specifier just before it, at a line's start or after a space.
        let mut s = pos;
        while let Some(p) = body[..s].chars().next_back()
            && (p.is_ascii_alphanumeric() || ".+*<^>".contains(p))
        {
            s -= p.len_utf8();
        }
        let edge = body[..s]
            .chars()
            .next_back()
            .is_none_or(char::is_whitespace);
        let (from, sp) = match spec(&body[s..pos]) {
            Some(sp) if edge => (s, sp),
            _ => (pos, Spec::default()),
        };
        if let Some((sp, start, at)) = open.take() {
            out.push((sp, body[start..from].to_owned(), at));
        }
        open = Some((sp, pos + c.len_utf8(), line));
    }
    if let Some((sp, start, at)) = open {
        out.push((sp, body[start..].to_owned(), at));
    }
    out
}

/// Columns a `cols` attribute gives: `3,2,3` three, `2*` two.
fn columns_of(cols: &str) -> usize {
    cols.split([',', ';'])
        .map(|c| {
            c.split_once('*')
                .and_then(|(n, _)| n.trim().parse().ok())
                .unwrap_or(1)
        })
        .sum::<usize>()
        .max(1)
}

/// A delimiter-separated line's fields, `"` quoting as CSV has it.
fn fields(line: &str, sep: char) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        let field = out.last_mut().expect("one at least");
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                chars.next();
                field.push('"');
            }
            '"' => quoted = !quoted,
            _ if c == sep && !quoted => out.push(String::new()),
            _ => field.push(c),
        }
    }
    out.into_iter().map(|f| f.trim().to_owned()).collect()
}

impl Doc {
    /// `{name}` replaced by its value, entities by their characters.
    fn substitute<'a>(&self, text: &'a str) -> Cow<'a, str> {
        if !text.contains('{') {
            return crate::html::decode(text);
        }
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(open) = rest.find('{') {
            out.push_str(&rest[..open]);
            let after = &rest[open + 1..];
            match after.find('}') {
                Some(close)
                    if close > 0
                        && after[..close]
                            .chars()
                            .all(|c| c.is_alphanumeric() || c == '-' || c == '_') =>
                {
                    let name = &after[..close];
                    if out.ends_with('\\') {
                        out.pop();
                        out.push_str(&rest[open..open + close + 2]);
                    } else if let Some(v) = self.attrs.get(&name.to_ascii_lowercase()) {
                        out.push_str(v);
                    } else if let Some(v) = builtin(name) {
                        out.push_str(v);
                    } else {
                        out.push_str(&rest[open..open + close + 2]);
                    }
                    rest = &after[close + 1..];
                }
                _ => {
                    out.push('{');
                    rest = after;
                }
            }
        }
        out.push_str(rest);
        Cow::Owned(crate::html::decode(&out).into_owned())
    }

    /// Lines of words, a hard break between each, into `b`.
    fn read(&self, b: &mut Builder, lines: &[String]) {
        let notes = &self.notes;
        let special = move |t: &str, edge: bool| special(t, edge, notes);
        let rules = Rules {
            pairs: PAIRS,
            loose: LOOSE,
            special: &special,
            escape: Escape::Only(ESCAPED),
        };
        let lines: Vec<String> = lines
            .iter()
            .map(|l| self.substitute(l).into_owned())
            .collect();
        broken(&lines, b, &rules);
    }

    /// A paragraph of `lines`: a line ending ` +` breaks there, every line
    /// does when `hard`.
    fn paragraph(&self, b: &mut Builder, lines: &[&str], hard: bool) {
        let mut parts = vec![String::new()];
        for line in lines {
            let (words, breaks) = match line.trim_end().strip_suffix(" +") {
                Some(w) => (w, true),
                None => (line.trim_end(), hard),
            };
            let part = parts.last_mut().expect("one at least");
            if !part.is_empty() {
                part.push(' ');
            }
            part.push_str(words.trim());
            if breaks {
                parts.push(String::new());
            }
        }
        if parts.last().is_some_and(String::is_empty) {
            parts.pop();
        }
        b.flush();
        self.read(b, &parts);
        b.flush();
    }

    fn caption(&self, b: &mut Builder, text: &str) {
        b.flush();
        b.look.quiet += 1;
        b.look.bold += 1;
        self.read(b, &[text.to_owned()]);
        b.look.bold -= 1;
        b.look.quiet -= 1;
        b.flush();
    }

    fn footnotes(&self, b: &mut Builder) {
        let notes = self.notes.borrow().clone();
        if notes.is_empty() {
            return;
        }
        b.rule();
        b.look.quiet += 1;
        for (n, (_, words)) in notes.iter().enumerate() {
            b.flush();
            self.read(b, &[format!("[{}] {words}", n + 1)]);
            b.flush();
        }
        b.look.quiet -= 1;
    }

    fn blocks(&mut self, lines: &[&str], b: &mut Builder) {
        if self.depth >= MOST_DEPTH {
            flat(lines, b);
            return;
        }
        self.depth += 1;
        self.blocks_here(lines, b);
        self.depth -= 1;
    }

    fn blocks_here(&mut self, lines: &[&str], b: &mut Builder) {
        let mut meta = Meta::default();
        let mut i = 0;
        while i < lines.len() && !b.full() {
            let t = lines[i].trim_end();
            let trimmed = t.trim();
            if trimmed.is_empty() {
                i += 1;
                continue;
            }
            if t.starts_with("////") && t.chars().all(|c| c == '/') {
                let close = lines[i + 1..].iter().position(|l| l.trim_end() == t);
                i = close.map_or(lines.len(), |c| i + c + 2);
                continue;
            }
            if t.starts_with("//") {
                i += 1;
                continue;
            }
            if let Some((name, unset, value)) = entry(t) {
                let mut value = value.to_owned();
                i += 1;
                while value.ends_with(" \\") && i < lines.len() {
                    value.truncate(value.len() - 2);
                    value.push(' ');
                    value.push_str(lines[i].trim());
                    i += 1;
                }
                if unset {
                    self.attrs.remove(&name);
                } else {
                    let value = self.substitute(&value).into_owned();
                    self.attrs.insert(name, value);
                }
                continue;
            }
            if trimmed.starts_with("[[") && trimmed.ends_with("]]") {
                i += 1;
                continue;
            }
            if attribute_line(trimmed) {
                meta.add(&trimmed[1..trimmed.len() - 1]);
                i += 1;
                continue;
            }
            if let Some(title) = t.strip_prefix('.')
                && !title.is_empty()
                && !title.starts_with(['.', ' ', '\t'])
            {
                meta.title = Some(title.to_owned());
                i += 1;
                continue;
            }
            let title = section(t)
                .map(|(level, words)| (level, words, 1, t.starts_with('=')))
                .or_else(|| underlined(t, lines.get(i + 1)).map(|(l, w)| (l, w, 2, true)));
            if let Some((level, words, taken, document)) = title {
                let header = level == 1 && document && !self.begun;
                self.begun = true;
                meta = Meta::default();
                b.heading(level);
                self.read(b, &[words.to_owned()]);
                b.end_heading();
                i += taken;
                // The header: authors, a revision, attribute entries.
                if header {
                    while i < lines.len() && !lines[i].trim().is_empty() {
                        let line = lines[i].trim();
                        i += 1;
                        if let Some((name, unset, value)) = entry(line) {
                            let mut value = value.to_owned();
                            while value.ends_with(" \\") && i < lines.len() {
                                value.truncate(value.len() - 2);
                                value.push(' ');
                                value.push_str(lines[i].trim());
                                i += 1;
                            }
                            if unset {
                                self.attrs.remove(&name);
                            } else {
                                let value = self.substitute(&value).into_owned();
                                self.attrs.insert(name, value);
                            }
                        } else if !line.starts_with("//") {
                            b.caption(line);
                        }
                    }
                }
                continue;
            }
            self.begun = true;
            let m = std::mem::take(&mut meta);
            if delimiter(t) {
                let close = lines[i + 1..]
                    .iter()
                    .position(|l| l.trim_end() == t)
                    .map_or(lines.len(), |c| i + 1 + c);
                self.delimited(t, &lines[i + 1..close], b, &m);
                i = close + 1;
                continue;
            }
            if let Some(lang) = t.strip_prefix("```") {
                let close = lines[i + 1..]
                    .iter()
                    .position(|l| l.trim_end() == "```")
                    .map_or(lines.len(), |c| i + 1 + c);
                self.title(b, &m);
                if !lang.trim().is_empty() && m.title.is_none() {
                    b.caption(lang.trim());
                }
                b.code_block(&lines[i + 1..close].join("\n"));
                i = close + 1;
                continue;
            }
            if let Some(c) = ['|', ',', ':', '!']
                .into_iter()
                .find(|c| t.len() == 4 && t.starts_with(*c) && t.ends_with("==="))
            {
                let close = lines[i + 1..]
                    .iter()
                    .position(|l| l.trim_end() == t)
                    .map_or(lines.len(), |k| i + 1 + k);
                self.table(c, &lines[i + 1..close], b, &m);
                i = close + 1;
                continue;
            }
            if thematic(trimmed) {
                b.rule();
                i += 1;
                continue;
            }
            if let Some((name, target, attrs)) = block_macro(trimmed) {
                self.title(b, &m);
                let said = match name {
                    "image" => {
                        let alt = shown(attrs, true);
                        Some(format!("🖼 {}", if alt.is_empty() { target } else { &alt }))
                    }
                    "video" => Some(format!("🎞 {target}")),
                    "audio" => Some(format!("🔊 {target}")),
                    "include" => Some(format!("include: {target}")),
                    _ => None,
                };
                if let Some(said) = said {
                    b.flush();
                    b.look.quiet += 1;
                    b.words(&said, false);
                    b.look.quiet -= 1;
                    b.flush();
                }
                i += 1;
                continue;
            }
            if marker(t).is_some() {
                self.title(b, &m);
                i = self.list(lines, i, b, &m);
                continue;
            }
            if term(trimmed).is_some() {
                self.title(b, &m);
                i = self.descriptions(lines, i, b);
                continue;
            }
            // A literal paragraph: indented.
            if t.starts_with([' ', '\t']) {
                let end = lines[i..]
                    .iter()
                    .position(|l| l.trim().is_empty())
                    .map_or(lines.len(), |k| i + k);
                let body = dedent(&lines[i..end], least_indent(&lines[i..end]));
                self.title(b, &m);
                b.code_block(&body.join("\n"));
                i = end;
                continue;
            }
            let end = lines[i..]
                .iter()
                .position(|l| {
                    let t = l.trim();
                    t.is_empty() || delimiter(t) || attribute_line(t) || t.starts_with("//")
                })
                .map_or(lines.len(), |k| i + k)
                .max(i + 1);
            self.styled(&lines[i..end], b, &m);
            i = end;
        }
    }

    fn title(&self, b: &mut Builder, m: &Meta) {
        if let Some(title) = &m.title {
            self.caption(b, title);
        }
    }

    /// A paragraph in the style its attributes give it.
    fn styled(&mut self, lines: &[&str], b: &mut Builder, m: &Meta) {
        let first = lines[0].trim();
        // `NOTE: words`
        if let Some((label, rest)) = first.split_once(": ")
            && let Some(said) = admonition(label)
            && label.chars().all(|c| c.is_ascii_uppercase())
        {
            let mut body = vec![rest];
            body.extend(&lines[1..]);
            b.quote();
            b.caption(said);
            self.title(b, m);
            self.paragraph(b, &body, false);
            b.end_quote();
            return;
        }
        if let Some(said) = m.admonition() {
            b.quote();
            b.caption(said);
            self.title(b, m);
            self.paragraph(b, lines, false);
            b.end_quote();
            return;
        }
        self.title(b, m);
        match m.style.as_str() {
            "source" | "listing" | "literal" | "stem" | "latexmath" | "asciimath" => {
                b.code_block(&lines.join("\n"));
            }
            "verse" | "quote" => {
                b.quote();
                self.paragraph(b, lines, m.style == "verse");
                self.attribution(b, m);
                b.end_quote();
            }
            "sidebar" | "example" | "abstract" | "partintro" => {
                b.quote();
                self.paragraph(b, lines, false);
                b.end_quote();
            }
            _ => self.paragraph(b, lines, m.has("hardbreaks")),
        }
    }

    fn attribution(&self, b: &mut Builder, m: &Meta) {
        let who: Vec<&str> = [m.second.as_str(), m.third.as_str()]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
        if !who.is_empty() {
            b.caption(&format!("— {}", who.join(", ")));
        }
    }

    /// A delimited block: `delim` opened it, `inner` is between.
    fn delimited(&mut self, delim: &str, inner: &[&str], b: &mut Builder, m: &Meta) {
        let kind = delim.chars().next().unwrap_or('-');
        let style = m.style.as_str();
        if let Some(said) = m.admonition() {
            b.quote();
            b.caption(said);
            self.title(b, m);
            self.blocks(inner, b);
            b.end_quote();
            return;
        }
        self.title(b, m);
        match (kind, delim == "--") {
            ('/', _) => {}
            ('-', false) | ('.', _) => {
                if m.title.is_none() && !m.second.is_empty() {
                    b.caption(&m.second);
                }
                b.code_block(&inner.join("\n"));
            }
            ('+', _) if matches!(style, "stem" | "latexmath" | "asciimath") => {
                b.code_block(&inner.join("\n"));
            }
            ('+', _) => crate::html::read(&inner.join("\n"), b, &mut |_| None),
            _ if matches!(style, "source" | "listing" | "literal") => {
                b.code_block(&inner.join("\n"));
            }
            _ if style == "verse" => {
                b.quote();
                for stanza in inner
                    .split(|l| l.trim().is_empty())
                    .filter(|s| !s.is_empty())
                {
                    self.paragraph(b, stanza, true);
                }
                self.attribution(b, m);
                b.end_quote();
            }
            ('-', true) if style != "quote" => self.blocks(inner, b),
            _ => {
                b.quote();
                self.blocks(inner, b);
                self.attribution(b, m);
                b.end_quote();
            }
        }
    }

    /// A list from `i`: items nest by their markers, `+` attaches a block
    /// to an item, and blank lines between items do not end it.
    fn list(&mut self, lines: &[&str], mut i: usize, b: &mut Builder, m: &Meta) -> usize {
        let mut stack: Vec<(Kind, usize)> = Vec::new();
        let start = m.named.get("start").and_then(|s| s.parse().ok());
        loop {
            let next = lines[i..]
                .iter()
                .position(|l| !l.trim().is_empty())
                .map_or(lines.len(), |k| i + k);
            let Some((kind, depth, words, number)) = lines.get(next).and_then(|l| marker(l)) else {
                break;
            };
            let known = stack.iter().position(|s| *s == (kind, depth));
            // Past a blank line, a marker of another kind starts a list of its own.
            if b.full() || (next > i && known.is_none() && !stack.is_empty()) {
                break;
            }
            i = next + 1;
            match known {
                Some(at) => {
                    while stack.len() > at + 1 {
                        stack.pop();
                        b.end_list();
                    }
                }
                None => {
                    let first = match kind {
                        Kind::Star | Kind::Dash => None,
                        _ => Some(
                            number
                                .or(if stack.is_empty() { start } else { None })
                                .unwrap_or(1),
                        ),
                    };
                    b.list(first);
                    stack.push((kind, depth));
                }
            }
            b.item();
            let mut words = words;
            if matches!(kind, Kind::Star | Kind::Dash) {
                for (tick, done) in [("[x] ", true), ("[*] ", true), ("[ ] ", false)] {
                    if let Some(rest) = words.strip_prefix(tick) {
                        b.task(done);
                        words = rest;
                    }
                }
            }
            let mut text = vec![words];
            while let Some(line) = lines.get(i) {
                let t = line.trim();
                if t.is_empty()
                    || t == "+"
                    || marker(line).is_some()
                    || term(t).is_some()
                    || delimiter(t)
                    || attribute_line(t)
                    || t.starts_with("//")
                {
                    break;
                }
                text.push(t);
                i += 1;
            }
            self.paragraph(b, &text, false);
            // `+`, then a block that belongs to the item; before the next
            // item, it joins nothing.
            while lines.get(i).is_some_and(|l| l.trim() == "+") {
                if lines.get(i + 1).is_none_or(|l| marker(l).is_some()) {
                    i += 1;
                    break;
                }
                let end = attached_end(lines, i + 1);
                self.blocks(&lines[i + 1..end], b);
                i = end;
            }
        }
        while stack.pop().is_some() {
            b.end_list();
        }
        i
    }

    /// A description list from `i`: each term bold, what it says under it.
    fn descriptions(&mut self, lines: &[&str], mut i: usize, b: &mut Builder) -> usize {
        loop {
            let next = lines[i..]
                .iter()
                .position(|l| !l.trim().is_empty())
                .map_or(lines.len(), |k| i + k);
            let Some((said, depth, rest)) = lines.get(next).and_then(|l| term(l.trim())) else {
                break;
            };
            if b.full() {
                break;
            }
            i = next + 1;
            for _ in 1..depth {
                b.quote();
            }
            b.flush();
            b.look.bold += 1;
            self.read(b, &[said.to_owned()]);
            b.look.bold -= 1;
            b.flush();
            b.quote();
            let mut text = vec![rest];
            while let Some(line) = lines.get(i) {
                let t = line.trim();
                if t.is_empty()
                    || t == "+"
                    || marker(line).is_some()
                    || term(t).is_some()
                    || delimiter(t)
                    || attribute_line(t)
                {
                    break;
                }
                text.push(t);
                i += 1;
            }
            if text.iter().any(|t| !t.is_empty()) {
                self.paragraph(b, &text, false);
            } else {
                // The definition below the term, indented, past blank lines.
                loop {
                    let at = lines[i..]
                        .iter()
                        .position(|l| !l.trim().is_empty())
                        .map_or(lines.len(), |k| i + k);
                    let Some(line) = lines.get(at) else { break };
                    if !line.starts_with([' ', '\t'])
                        || marker(line).is_some()
                        || term(line.trim()).is_some()
                    {
                        break;
                    }
                    let end = lines[at..]
                        .iter()
                        .position(|l| l.trim().is_empty())
                        .map_or(lines.len(), |k| at + k);
                    self.paragraph(b, &lines[at..end], false);
                    i = end;
                }
            }
            let after = lines[i..]
                .iter()
                .position(|l| !l.trim().is_empty())
                .map_or(lines.len(), |k| i + k);
            if lines.get(after).is_some_and(|l| marker(l).is_some()) {
                i = self.list(lines, after, b, &Meta::default());
            }
            while lines.get(i).is_some_and(|l| l.trim() == "+") {
                let end = attached_end(lines, i + 1);
                self.blocks(&lines[i + 1..end], b);
                i = end;
            }
            b.end_quote();
            for _ in 1..depth {
                b.end_quote();
            }
        }
        i
    }

    /// A table: `delim` the first character of its `|===`, `inner` its lines.
    fn table(&mut self, delim: char, inner: &[&str], b: &mut Builder, m: &Meta) {
        self.title(b, m);
        let format = m
            .named
            .get("format")
            .map(String::as_str)
            .unwrap_or(match delim {
                ',' => "csv",
                ':' => "dsv",
                _ => "psv",
            });
        let sep = m
            .named
            .get("separator")
            .and_then(|s| s.chars().next())
            .unwrap_or(match format {
                "csv" => ',',
                "dsv" => ':',
                "tsv" => '\t',
                _ => delim,
            });
        let head = m.has("header");
        let no_head = m.has("noheader");
        if format != "psv" {
            if !b.table(Vec::new()) {
                return;
            }
            let rows = inner.iter().filter(|l| !l.trim().is_empty());
            for (n, line) in rows.enumerate() {
                b.row(head && n == 0);
                for field in fields(line, sep) {
                    b.cell();
                    self.read(b, &[field]);
                }
            }
            b.end_table();
            return;
        }
        let body = inner.join("\n");
        let found = cells(&body, sep);
        let first = inner.iter().position(|l| !l.trim().is_empty()).unwrap_or(0);
        let on_first: Vec<&(Spec, String, usize)> = found.iter().filter(|c| c.2 == first).collect();
        let columns = m.named.get("cols").map_or_else(
            || {
                on_first
                    .iter()
                    .map(|c| c.0.columns * c.0.copies)
                    .sum::<usize>()
                    .max(1)
            },
            |c| columns_of(c),
        );
        // The first line is the heading when a blank line follows it.
        let implicit = first == 0
            && inner.get(1).is_some_and(|l| l.trim().is_empty())
            && on_first
                .iter()
                .map(|c| c.0.columns * c.0.copies)
                .sum::<usize>()
                == columns;
        let head = !no_head && (head || implicit);
        let mut rows: Vec<Vec<Option<(Spec, String, usize)>>> = Vec::new();
        let mut row = Vec::new();
        let mut col = 0;
        let mut busy = vec![0usize; columns];
        let mut below = vec![0usize; columns];
        let next_row = |busy: &mut Vec<usize>, below: &mut Vec<usize>| {
            for (b, n) in busy.iter_mut().zip(below.iter_mut()) {
                *b = if *b > 0 { *b - 1 } else { *n };
                *n = 0;
            }
        };
        for (sp, words, _) in found {
            for _ in 0..sp.copies {
                loop {
                    while col < columns && busy[col] > 0 {
                        row.push(None);
                        col += 1;
                    }
                    if col < columns {
                        break;
                    }
                    rows.push(std::mem::take(&mut row));
                    col = 0;
                    next_row(&mut busy, &mut below);
                }
                let span = sp.columns.clamp(1, columns - col);
                for n in &mut below[col..col + span] {
                    *n = sp.rows - 1;
                }
                row.push(Some((sp, words.clone(), span)));
                col += span;
                if col >= columns {
                    rows.push(std::mem::take(&mut row));
                    col = 0;
                    next_row(&mut busy, &mut below);
                }
            }
            if rows.len() > scour_flow::MOST_BLOCKS {
                break;
            }
        }
        if row.iter().any(Option::is_some) {
            rows.push(row);
        }
        if !b.table(Vec::new()) {
            return;
        }
        for (n, row) in rows.into_iter().enumerate() {
            b.row(head && n == 0);
            for slot in row {
                b.cell();
                if let Some((sp, words, span)) = slot {
                    b.span(span as u32);
                    self.cell(b, sp, &words);
                }
            }
        }
        b.end_table();
    }

    fn cell(&mut self, b: &mut Builder, sp: Spec, words: &str) {
        let lines: Vec<&str> = words.lines().collect();
        match sp.style {
            'a' => self.blocks(&dedent(&lines, least_indent(&lines)), b),
            'l' => b.code_block(words.trim_matches('\n')),
            style => {
                let look = match style {
                    's' | 'h' => &mut b.look.bold,
                    'e' => &mut b.look.italic,
                    'm' => &mut b.look.code,
                    _ => &mut b.look.quiet,
                };
                let quiet = !matches!(style, 's' | 'h' | 'e' | 'm');
                if !quiet {
                    *look += 1;
                }
                for part in lines
                    .split(|l| l.trim().is_empty())
                    .filter(|p| !p.is_empty())
                {
                    self.paragraph(b, part, false);
                }
                if !quiet {
                    match style {
                        's' | 'h' => b.look.bold -= 1,
                        'e' => b.look.italic -= 1,
                        _ => b.look.code -= 1,
                    }
                }
            }
        }
    }
}

/// Where a block attached with `+` from `i` ends: its attribute and title
/// lines, then a delimited block to its close, or lines to a blank one.
fn attached_end(lines: &[&str], mut i: usize) -> usize {
    while lines.get(i).is_some_and(|l| {
        let t = l.trim();
        attribute_line(t) || (t.starts_with('.') && !t.starts_with(". ") && t.len() > 1)
    }) {
        i += 1;
    }
    let Some(first) = lines.get(i).map(|l| l.trim_end()) else {
        return lines.len();
    };
    if delimiter(first) || first.starts_with("```") {
        let close = if first.starts_with("```") {
            "```"
        } else {
            first
        };
        return lines[i + 1..]
            .iter()
            .position(|l| l.trim_end() == close)
            .map_or(lines.len(), |k| i + k + 2);
    }
    lines[i..]
        .iter()
        .position(|l| {
            let t = l.trim();
            t.is_empty() || t == "+" || marker(l).is_some()
        })
        .map_or(lines.len(), |k| i + k)
        .max(i + 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use scour_flow::{Block, Para, Table};

    fn paras(flow: &Flow) -> Vec<&Para> {
        flow.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Para(p) => Some(p),
                _ => None,
            })
            .collect()
    }

    fn texts(flow: &Flow) -> Vec<String> {
        paras(flow).iter().map(|p| p.text()).collect()
    }

    fn tables(flow: &Flow) -> Vec<&Table> {
        flow.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Table(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn header_sections_and_inline_marks() {
        let adoc = "= Başlık\nAda Lovelace <ada@x>\n:ad: Scour\n\n== Bölüm\n\n\
                    *kalın* _eğik_ `kod` iç**kalın**dış {ad} H~2~O \\*düz\\* \
                    https://x.org[bağ] <<yer,orası>> kbd:[Ctrl+C] dipnot.footnote:[Not.]\n\
                    satır +\nkırılır\n";
        let flow = flow(adoc.as_bytes()).unwrap();
        let p = paras(&flow);
        let all = texts(&flow);
        assert_eq!((p[0].heading, all[0].as_str()), (1, "Başlık"));
        assert_eq!(all[1], "Ada Lovelace <ada@x>");
        assert_eq!((p[2].heading, all[2].as_str()), (2, "Bölüm"));
        assert_eq!(
            all[3],
            "kalın eğik kod içkalındış Scour H2O *düz* bağ orası Ctrl+C dipnot.[1] satır\nkırılır"
        );
        let runs = &p[3].runs;
        assert!(runs.iter().any(|r| r.bold && r.text == "kalın"));
        assert!(runs.iter().any(|r| r.italic && r.text == "eğik"));
        assert!(runs.iter().any(|r| r.code && r.text == "kod"));
        assert!(runs.iter().any(|r| r.underline && r.text == "bağ"));
        assert_eq!(all.last().unwrap(), "[1] Not.");
    }

    #[test]
    fn blocks_in_their_styles() {
        let adoc = "[source,rust]\n----\nfn main() {}\n----\n\nNOTE: Dikkat et.\n\n\
                    [quote,Ada]\n____\nSöz.\n____\n\n.Başlıklı\n====\nÖrnek.\n====\n\n\
                    // yorum\n\n  girintili\n  kod\n\n'''\n";
        let flow = flow(adoc.as_bytes()).unwrap();
        let all = texts(&flow);
        assert_eq!(all[0], "rust");
        assert_eq!(all[1], "fn main() {}");
        assert_eq!(all[2], "Note");
        assert_eq!(all[3], "Dikkat et.");
        assert_eq!(all[4], "Söz.");
        assert_eq!(all[5], "— Ada");
        assert_eq!(all[6], "Başlıklı");
        assert_eq!(all[7], "Örnek.");
        assert_eq!(all[8], "girintili\nkod");
        assert!(!all.iter().any(|t| t.contains("yorum")));
        assert!(matches!(flow.blocks.last(), Some(Block::Break)));
    }

    #[test]
    fn lists_nest_by_their_markers() {
        let adoc = ". bir\n* alt\n** daha alt\n. iki\n+\n----\nek\n----\n\n* [x] bitti\n* [ ] değil\n\n\
                    Terim:: Tanım.\n";
        let flow = flow(adoc.as_bytes()).unwrap();
        let p = paras(&flow);
        let marks: Vec<(&str, String)> = p.iter().map(|p| (p.mark.as_str(), p.text())).collect();
        assert_eq!(marks[0], ("1.", "bir".into()));
        assert_eq!(marks[1], ("◦", "alt".into()));
        assert!(p[2].indent > p[1].indent && p[1].indent > p[0].indent);
        assert_eq!(marks[3], ("2.", "iki".into()));
        assert_eq!(marks[4].1, "ek");
        assert_eq!(marks[5], ("☑", "bitti".into()));
        assert_eq!(marks[6], ("☐", "değil".into()));
        assert!(p[7].runs[0].bold && marks[7].1 == "Terim");
        assert_eq!(marks[8].1, "Tanım.");
    }

    #[test]
    fn tables_with_spans_styles_and_csv() {
        let adoc = "|===\n|A |B\n\n2+|geniş\n|1 a|* liste\n|===\n\n\
                    [%header,format=csv]\n|===\nad,\"sayı, ondalık\"\nelma,3\n|===\n";
        let flow = flow(adoc.as_bytes()).unwrap();
        let t = tables(&flow);
        assert!(t[0].rows[0].head);
        assert_eq!(t[0].rows[1].cells[0].span, 2);
        assert_eq!(t[0].rows[1].cells[0].paras[0].text(), "geniş");
        assert_eq!(t[0].rows[2].cells[1].paras[0].mark, "•");
        assert_eq!(t[1].rows[0].cells[1].paras[0].text(), "sayı, ondalık");
        assert!(t[1].rows[0].head && !t[1].rows[1].head);
    }

    #[test]
    fn underlined_titles_and_definitions_below_their_terms() {
        let adoc = "Kılavuz\n=======\n\nGiriş\n-----\n\n*-c*::\n\n    Temizler.\n\n\
                    Örnek:\n----\nkod\n----\n";
        let flow = flow(adoc.as_bytes()).unwrap();
        let p = paras(&flow);
        let all = texts(&flow);
        assert_eq!((p[0].heading, all[0].as_str()), (1, "Kılavuz"));
        assert_eq!((p[1].heading, all[1].as_str()), (2, "Giriş"));
        assert_eq!(all[2], "-c");
        assert!(!p[3].runs[0].code && all[3] == "Temizler.");
        // Too short to underline `Örnek:`: the listing it opens.
        assert_eq!(all[4], "Örnek:");
        assert!(p[5].runs[0].code && all[5] == "kod");
    }

    #[test]
    fn specifiers_as_asciidoctor_reads_them() {
        assert_eq!(spec("2+").map(|s| s.columns), Some(2));
        let tall = spec(".3+^.>s").unwrap();
        assert_eq!((tall.rows, tall.style), (3, 's'));
        assert_eq!(spec("2*^").map(|s| s.copies), Some(2));
        assert_eq!(spec("a").map(|s| s.style), Some('a'));
        assert_eq!(spec("word"), None);
        assert_eq!(spec("2"), None);
    }
}
