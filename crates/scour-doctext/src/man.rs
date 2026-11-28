//! A manual page as a flow: roff with the `man` macros, or BSD's `mdoc`,
//! read as `man` lays it out — the title line, sections, paragraphs and
//! their fonts, tagged and indented paragraphs, `.RS` indents, no-fill and
//! example blocks, links, the page's own macros and strings, and `tbl`'s
//! tables; `mdoc`'s lists, displays and inline macros. Text that is not
//! roff is refused, so a `.1` that is a log is shown as text.

use std::collections::HashMap;

use scour_flow::Flow;

use crate::build::Builder;
use crate::inline::MOST_DEPTH;

/// The page in `bytes`.
pub fn flow(bytes: &[u8]) -> Result<Flow, String> {
    if bytes.iter().take(8192).any(|&b| b == 0) {
        return Err("not a manual page".into());
    }
    let text = String::from_utf8_lossy(bytes).replace("\r\n", "\n");
    let lines: Vec<&str> = text.lines().collect();
    if !looks_like_roff(&lines) {
        return Err("not a manual page".into());
    }
    let mut m = Man::default();
    let mut b = Builder::default();
    m.lines(&lines, &mut b);
    m.close(&mut b);
    Ok(b.finish())
}

/// Roff: among the first lines that say something, a request a page opens
/// with.
fn looks_like_roff(lines: &[&str]) -> bool {
    const OPENERS: &[&str] = &[
        ".TH", ".Dd", ".Dt", ".SH", ".Sh", ".\\\"", "'\\\"", ".so ", ".de ", ".PP", ".TS", ".ds ",
        ".nr ", ".ig", ".Os",
    ];
    lines
        .iter()
        .filter(|l| !l.trim().is_empty())
        .take(60)
        .any(|l| OPENERS.iter().any(|o| l.starts_with(o)))
}

/// A font: what `\fB`, `\fI`, `\f[C]` set.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
struct Font {
    bold: bool,
    italic: bool,
    code: bool,
}

impl Font {
    fn named(name: &str) -> Option<Font> {
        let (bold, italic, code) = match name {
            "R" | "1" | "" | "P" => (false, false, false),
            "B" | "3" => (true, false, false),
            "I" | "2" => (false, true, false),
            "BI" | "4" | "IB" => (true, true, false),
            "C" | "CW" | "CR" | "CO" | "V" | "TT" => (false, false, true),
            "CB" | "CWB" => (true, false, true),
            "CI" => (false, true, true),
            "CBI" => (true, true, true),
            _ => return None,
        };
        Some(Font { bold, italic, code })
    }
}

/// The paragraph indented under a tag, open until the next paragraph macro.
#[derive(PartialEq, Eq, Clone, Copy)]
enum Under {
    /// `.IP`/`.TP`: a quote holds the body.
    Tagged,
}

#[derive(Default)]
struct Man {
    font: Font,
    previous: Font,
    fill: bool,
    /// Lines of a no-fill block, as text.
    nofill: Vec<String>,
    /// `.RS` indents open.
    indents: usize,
    under: Option<Under>,
    /// `.TP`: the next line is the tag.
    tag_next: bool,
    /// A font macro with no words: the next line is in its fonts.
    font_next: Option<Vec<Font>>,
    /// `.UR`/`.MT`: the link's address, its words not yet seen.
    link: Option<(String, bool)>,
    macros: HashMap<String, Vec<String>>,
    strings: HashMap<String, String>,
    depth: usize,
    /// `.ie` said no: its `.el` runs.
    else_runs: bool,
    // mdoc.
    mdoc: bool,
    name: String,
    lists: Vec<MdocList>,
    /// An `mdoc` table, `.Bl -column`, being read.
    columns: bool,
    /// `.Bd -literal`: no fill, closed by `.Ed`.
    display: bool,
    /// The `.Dd` date, said under the title.
    date: String,
    /// The last `mdoc` line ended in an opening bracket: no space after it.
    glued: bool,
    /// `.Sm off`: a line's words with no spaces between them.
    unspaced: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MdocList {
    Bullet,
    Number,
    Tag,
    Column,
    Plain,
}

/// A request's arguments: split at spaces, `"` grouping, `""` a quote.
fn arguments(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| *c == ' ' || *c == '\t') {
            chars.next();
        }
        let Some(&first) = chars.peek() else { break };
        let mut arg = String::new();
        if first == '"' {
            chars.next();
            while let Some(c) = chars.next() {
                match c {
                    '"' if chars.peek() == Some(&'"') => {
                        chars.next();
                        arg.push('"');
                    }
                    '"' => break,
                    _ => arg.push(c),
                }
            }
        } else {
            while let Some(&c) = chars.peek() {
                if c == ' ' || c == '\t' {
                    break;
                }
                arg.push(c);
                chars.next();
                // An escape keeps its next character, a space among them.
                if c == '\\'
                    && let Some(n) = chars.next()
                {
                    arg.push(n);
                }
            }
        }
        out.push(arg);
    }
    out
}

/// A named character, `\(bu` or `\[em]`.
fn glyph(name: &str) -> Option<&'static str> {
    Some(match name {
        "bu" => "•",
        "em" => "—",
        "en" => "–",
        "hy" | "mi" | "-" => "-",
        "co" => "©",
        "rg" => "®",
        "tm" => "™",
        "aq" => "'",
        "dq" => "\"",
        "lq" => "“",
        "rq" => "”",
        "oq" => "‘",
        "cq" => "’",
        "Fo" => "«",
        "Fc" => "»",
        "fo" => "‹",
        "fc" => "›",
        "de" => "°",
        "mu" => "×",
        "di" => "÷",
        "+-" => "±",
        "<=" => "≤",
        ">=" => "≥",
        "!=" => "≠",
        "==" => "≡",
        "~=" => "≅",
        "ap" => "∼",
        "->" => "→",
        "<-" => "←",
        "<>" => "↔",
        "ua" => "↑",
        "da" => "↓",
        "rA" => "⇒",
        "lA" => "⇐",
        "hA" => "⇔",
        "ga" => "`",
        "aa" => "´",
        "ti" | "a~" => "~",
        "ha" | "a^" => "^",
        "rs" => "\\",
        "sl" => "/",
        "ba" | "or" => "|",
        "br" | "bv" => "│",
        "ul" | "ru" => "_",
        "ci" => "○",
        "sq" => "□",
        "OK" => "✓",
        "sc" => "§",
        "ps" => "¶",
        "dg" => "†",
        "dd" => "‡",
        "ct" => "¢",
        "Po" => "£",
        "Ye" => "¥",
        "Eu" | "eu" => "€",
        "mc" => "µ",
        "fm" => "′",
        "sd" => "″",
        "pc" | "md" => "·",
        "**" => "∗",
        "pl" => "+",
        "eq" => "=",
        "lB" => "[",
        "rB" => "]",
        "lC" => "{",
        "rC" => "}",
        "la" => "⟨",
        "ra" => "⟩",
        "if" => "∞",
        "sr" => "√",
        "pd" => "∂",
        "is" => "∫",
        "sum" => "∑",
        "product" => "∏",
        "fa" => "∀",
        "te" => "∃",
        "mo" => "∈",
        "nm" => "∉",
        "es" => "∅",
        "AN" => "∧",
        "OR" => "∨",
        "no" | "tno" => "¬",
        "*a" => "α",
        "*b" => "β",
        "*g" => "γ",
        "*d" => "δ",
        "*e" => "ε",
        "*l" => "λ",
        "*m" => "μ",
        "*p" => "π",
        "*s" => "σ",
        "*t" => "τ",
        "*f" => "φ",
        "*W" => "Ω",
        "*D" => "Δ",
        "*S" => "Σ",
        ":a" => "ä",
        ":o" => "ö",
        ":u" => "ü",
        ":A" => "Ä",
        ":O" => "Ö",
        ":U" => "Ü",
        ":e" => "ë",
        ":i" => "ï",
        ",c" => "ç",
        ",C" => "Ç",
        "'e" => "é",
        "`e" => "è",
        "^e" => "ê",
        "'a" => "á",
        "`a" => "à",
        "^a" => "â",
        "'o" => "ó",
        "'i" => "í",
        "'u" => "ú",
        "~n" => "ñ",
        "~N" => "Ñ",
        "ss" => "ß",
        "ae" => "æ",
        "AE" => "Æ",
        "o/" => "ø",
        "O/" => "Ø",
        "oa" => "å",
        "oA" => "Å",
        "vs" => "š",
        "vS" => "Š",
        "vz" => "ž",
        "'y" => "ý",
        _ => return None,
    })
}

/// The text between quotes after `\X`: `\X'…'`, or a bracketed `[…]`.
fn delimited_arg(rest: &str) -> (usize, &str) {
    let Some(open) = rest.chars().next() else {
        return (0, "");
    };
    let close = if open == '[' { ']' } else { open };
    match rest[open.len_utf8()..].find(close) {
        Some(end) => (open.len_utf8() * 2 + end, &rest[1..1 + end]),
        None => (rest.len(), ""),
    }
}

/// A name after `\(`, `\[` or a single character: its length and itself.
fn escape_name(rest: &str) -> (usize, &str) {
    if let Some(r) = rest.strip_prefix('(') {
        let end = r.char_indices().nth(2).map_or(r.len(), |(i, _)| i);
        (1 + end, &r[..end])
    } else if let Some(r) = rest.strip_prefix('[') {
        match r.find(']') {
            Some(end) => (end + 2, &r[..end]),
            None => (rest.len(), ""),
        }
    } else {
        let n = rest.chars().next().map_or(0, char::len_utf8);
        (n, &rest[..n])
    }
}

impl Man {
    /// Words with their escapes read, as pieces in their fonts.
    fn pieces(&mut self, text: &str) -> Vec<(String, Font)> {
        let mut out: Vec<(String, Font)> = Vec::new();
        let mut cur = String::new();
        let push = |out: &mut Vec<(String, Font)>, cur: &mut String, font: Font| {
            if !cur.is_empty() {
                out.push((std::mem::take(cur), font));
            }
        };
        let mut i = 0;
        while i < text.len() {
            let rest = &text[i..];
            let Some(after) = rest.strip_prefix('\\') else {
                let c = rest.chars().next().expect("not at the end");
                cur.push(c);
                i += c.len_utf8();
                continue;
            };
            let Some(e) = after.chars().next() else {
                break;
            };
            let after_e = &after[e.len_utf8()..];
            i += 1 + e.len_utf8();
            match e {
                'f' => {
                    let (n, name) = escape_name(after_e);
                    i += n;
                    let font = if name == "P" || name.is_empty() {
                        self.previous
                    } else {
                        Font::named(name).unwrap_or(self.font)
                    };
                    push(&mut out, &mut cur, self.font);
                    self.previous = self.font;
                    self.font = font;
                }
                '(' | '[' => {
                    let (n, name) = escape_name(after);
                    i += n - 1;
                    if let Some(code) = name.strip_prefix('u')
                        && let Ok(c) = u32::from_str_radix(code.split('_').next().unwrap_or(""), 16)
                        && let Some(c) = char::from_u32(c)
                    {
                        cur.push(c);
                    } else if let Some(code) = name.strip_prefix("char")
                        && let Ok(c) = code.parse::<u32>()
                        && let Some(c) = char::from_u32(c)
                    {
                        cur.push(c);
                    } else if let Some(g) = glyph(name) {
                        cur.push_str(g);
                    }
                }
                'C' => {
                    let (n, name) = delimited_arg(after_e);
                    i += n;
                    if let Some(g) = glyph(name) {
                        cur.push_str(g);
                    }
                }
                '*' => {
                    let (n, name) = escape_name(after_e);
                    i += n;
                    let said = match name {
                        "R" => Some("®".to_owned()),
                        "(Tm" | "Tm" => Some("™".to_owned()),
                        "(lq" | "lq" => Some("“".to_owned()),
                        "(rq" | "rq" => Some("”".to_owned()),
                        _ => self.strings.get(name).cloned(),
                    };
                    if let Some(said) = said
                        && self.depth < MOST_DEPTH
                    {
                        self.depth += 1;
                        let inner = self.pieces(&said);
                        self.depth -= 1;
                        push(&mut out, &mut cur, self.font);
                        out.extend(inner);
                    }
                }
                'N' => {
                    let (n, code) = delimited_arg(after_e);
                    i += n;
                    if let Some(c) = code.parse::<u32>().ok().and_then(char::from_u32) {
                        cur.push(c);
                    }
                }
                'e' | 'E' | '\\' => cur.push('\\'),
                '-' => cur.push('-'),
                '.' => cur.push('.'),
                '\'' => cur.push('´'),
                '`' => cur.push('`'),
                ' ' | '0' => cur.push(' '),
                '~' => cur.push('\u{a0}'),
                't' => cur.push('\t'),
                // Comments: the rest of the line.
                '"' | '#' => break,
                // Sizes, colours, registers, motions: nothing on the page.
                's' => {
                    let r = after_e;
                    let mut n = 0;
                    if r.starts_with(['+', '-']) {
                        n += 1;
                    }
                    match r[n..].chars().next() {
                        Some('(') => n += 3,
                        Some('[' | '\'') => n += delimited_arg(&r[n..]).0,
                        Some(c) if c.is_ascii_digit() => {
                            n += 1;
                            // `\s10`: two digits from 1, 2 or 3.
                            if matches!(c, '1'..='3')
                                && r[n..].starts_with(|d: char| d.is_ascii_digit())
                            {
                                n += 1;
                            }
                        }
                        _ => {}
                    }
                    i += n.min(r.len());
                }
                'm' | 'M' | 'n' | 'g' | 'k' | 'F' | 'V' | 'Y' | '$' => {
                    let r = after_e;
                    let r2 = r.strip_prefix(['+', '-']).unwrap_or(r);
                    let sign = r.len() - r2.len();
                    i += sign + escape_name(r2).0;
                }
                'X' | 'D' | 'h' | 'v' | 'w' | 'l' | 'L' | 'o' | 'b' | 'x' | 'Z' | 'S' | 'H'
                | 'R' | 'A' | 'B' => {
                    i += delimited_arg(after_e).0;
                }
                '&' | '|' | '^' | ')' | '%' | ':' | '/' | ',' | 'c' | 'u' | 'd' | 'a' | 'p'
                | '{' | '}' | 'z' | 'r' | '!' | '?' => {}
                other => cur.push(other),
            }
        }
        push(&mut out, &mut cur, self.font);
        out
    }

    /// Words into the open paragraph, in their fonts.
    fn words(&mut self, b: &mut Builder, text: &str) {
        let pieces = self.pieces(text);
        self.emit(b, &pieces);
    }

    fn emit(&mut self, b: &mut Builder, pieces: &[(String, Font)]) {
        let link = self.link.is_some();
        if link {
            b.look.link += 1;
            if let Some(l) = self.link.as_mut() {
                l.1 = true;
            }
        }
        for (words, font) in pieces {
            b.look.bold += u8::from(font.bold);
            b.look.italic += u8::from(font.italic);
            b.look.code += u8::from(font.code);
            b.folded(words);
            b.look.bold -= u8::from(font.bold);
            b.look.italic -= u8::from(font.italic);
            b.look.code -= u8::from(font.code);
        }
        if link {
            b.look.link -= 1;
        }
    }

    /// A paragraph macro: what was open closes.
    fn paragraph(&mut self, b: &mut Builder) {
        self.end_nofill(b);
        b.flush();
        if self.under.take() == Some(Under::Tagged) {
            b.end_quote();
        }
    }

    fn end_nofill(&mut self, b: &mut Builder) {
        if !self.nofill.is_empty() {
            let code = std::mem::take(&mut self.nofill).join("\n");
            b.code_block(code.trim_matches('\n'));
        }
    }

    fn close(&mut self, b: &mut Builder) {
        self.paragraph(b);
        while self.indents > 0 {
            b.end_quote();
            self.indents -= 1;
        }
        while self.lists.pop().is_some() {
            b.end_list();
        }
        if self.columns {
            b.end_table();
            self.columns = false;
        }
    }

    fn lines(&mut self, lines: &[&str], b: &mut Builder) {
        if self.depth >= MOST_DEPTH {
            return;
        }
        self.depth += 1;
        let mut i = 0;
        while i < lines.len() && !b.full() {
            i = self.line(lines, i, b);
        }
        self.depth -= 1;
    }

    /// The line at `i`, and where the next to read is.
    fn line(&mut self, lines: &[&str], i: usize, b: &mut Builder) -> usize {
        let line = lines[i];
        let control = line.starts_with('.') || line.starts_with('\'');
        if !control {
            self.text_line(line, b);
            return i + 1;
        }
        let body = line[1..].trim_start();
        let (name, rest) = match body.find([' ', '\t']) {
            Some(at) => (&body[..at], body[at..].trim_start()),
            None => (body, ""),
        };
        if name.starts_with("\\\"") || name.is_empty() {
            return i + 1;
        }
        match name {
            "de" | "de1" | "am" | "ig" => {
                let args = arguments(rest);
                let end = args.get(1).map_or("..".to_owned(), |e| format!(".{e}"));
                let close = lines[i + 1..]
                    .iter()
                    .position(|l| l.trim_end() == end || (end == ".." && l.trim_end() == ".."))
                    .map_or(lines.len(), |k| i + 1 + k);
                if name != "ig"
                    && let Some(macro_name) = args.first()
                {
                    let body: Vec<String> = lines[i + 1..close]
                        .iter()
                        .map(|l| (*l).to_owned())
                        .collect();
                    if name == "am" {
                        self.macros
                            .entry(macro_name.clone())
                            .or_default()
                            .extend(body);
                    } else {
                        self.macros.insert(macro_name.clone(), body);
                    }
                }
                return close + 1;
            }
            "TS" => return self.tbl(lines, i + 1, b),
            "if" | "ie" | "el" => return self.condition(name, rest, lines, i, b),
            _ => {}
        }
        if let Some(body) = self.macros.get(name).cloned() {
            let args = arguments(rest);
            let expanded: Vec<String> = body
                .iter()
                .map(|l| {
                    let mut l = l.replace("\\\\", "\\");
                    for (n, a) in args.iter().enumerate().take(9).rev() {
                        l = l.replace(&format!("\\${}", n + 1), a);
                    }
                    l = l
                        .replace("\\$*", &args.join(" "))
                        .replace("\\$@", &args.join(" "));
                    // Arguments not given are empty.
                    for n in (1..=9).rev() {
                        l = l.replace(&format!("\\${n}"), "");
                    }
                    l
                })
                .collect();
            let refs: Vec<&str> = expanded.iter().map(String::as_str).collect();
            self.lines(&refs, b);
            return i + 1;
        }
        if self.mdoc || matches!(name, "Dd" | "Dt" | "Sh") {
            self.mdoc = true;
            if self.mdoc_request(name, rest, b) {
                return i + 1;
            }
        }
        self.request(name, rest, b);
        i + 1
    }

    /// `.if`, `.ie` and `.el`: a condition about nroff holds, one about troff
    /// does not, and any other is taken as false.
    fn condition(
        &mut self,
        name: &str,
        rest: &str,
        lines: &[&str],
        i: usize,
        b: &mut Builder,
    ) -> usize {
        let (holds, body) = if name == "el" {
            (self.else_runs, rest)
        } else {
            let negated = rest.starts_with('!');
            let r = rest.trim_start_matches('!');
            let (cond, body) = match r.find([' ', '\t']) {
                Some(at) => (&r[..at], r[at..].trim_start()),
                None => (r, ""),
            };
            let truth = match cond {
                "n" => true,
                "t" | "v" | "o" | "e" => false,
                _ => false,
            } != negated;
            if name == "ie" {
                self.else_runs = !truth;
            }
            (truth, body)
        };
        // A block in `\{ … \}`.
        if let Some(first) = body.strip_prefix("\\{") {
            let mut block = vec![first.trim_start()];
            let mut end = i;
            if !first.contains("\\}") {
                end = lines[i + 1..]
                    .iter()
                    .position(|l| l.contains("\\}"))
                    .map_or(lines.len() - 1, |k| i + 1 + k);
                block.extend(&lines[i + 1..=end.min(lines.len() - 1)]);
            }
            if holds {
                let cleaned: Vec<String> = block
                    .iter()
                    .map(|l| l.replace("\\}", ""))
                    .filter(|l| !l.trim().is_empty())
                    .collect();
                let refs: Vec<&str> = cleaned.iter().map(String::as_str).collect();
                self.lines(&refs, b);
            }
            return end + 1;
        }
        if holds && !body.is_empty() {
            self.lines(&[body], b);
        }
        i + 1
    }

    fn text_line(&mut self, line: &str, b: &mut Builder) {
        if !self.fill || self.display {
            let pieces = self.pieces(line);
            self.nofill
                .push(pieces.into_iter().map(|(t, _)| t).collect());
            return;
        }
        if self.tag_next {
            self.tag_next = false;
            self.tag(b, line);
            return;
        }
        if let Some(fonts) = self.font_next.take() {
            self.alternating(b, &fonts, &[line.to_owned()]);
            return;
        }
        if line.trim().is_empty() {
            b.flush();
            return;
        }
        // A line that starts with a space starts a line of its own.
        if line.starts_with([' ', '\t']) && b.has_words() {
            b.words("\n", false);
        }
        b.folded(" ");
        self.words(b, line);
    }

    /// A `.TP` tag: its own line, the body indented under it.
    fn tag(&mut self, b: &mut Builder, tag: &str) {
        b.flush();
        if tag.starts_with('.') || tag.starts_with('\'') {
            self.lines(&[tag], b);
        } else {
            self.words(b, tag);
        }
        b.flush();
        b.quote();
        self.under = Some(Under::Tagged);
    }

    /// `.B`, `.IR` and the rest: each argument in the next font of `fonts`,
    /// with no space between.
    fn alternating(&mut self, b: &mut Builder, fonts: &[Font], args: &[String]) {
        if self.tag_next {
            self.tag_next = false;
            b.flush();
            let saved = self.font;
            for (n, a) in args.iter().enumerate() {
                self.font = fonts[n % fonts.len()];
                self.words(b, a);
            }
            self.font = saved;
            b.flush();
            b.quote();
            self.under = Some(Under::Tagged);
            return;
        }
        let saved = self.font;
        b.folded(" ");
        for (n, a) in args.iter().enumerate() {
            self.font = fonts[n % fonts.len()];
            self.words(b, a);
        }
        self.font = saved;
    }

    fn request(&mut self, name: &str, rest: &str, b: &mut Builder) {
        let font = |n: &str| Font::named(n).unwrap_or_default();
        let fonts: Option<Vec<Font>> = match name {
            "B" | "SB" => Some(vec![font("B")]),
            "I" => Some(vec![font("I")]),
            "SM" => Some(vec![font("R")]),
            "BI" => Some(vec![font("B"), font("I")]),
            "IB" => Some(vec![font("I"), font("B")]),
            "BR" => Some(vec![font("B"), font("R")]),
            "RB" => Some(vec![font("R"), font("B")]),
            "IR" => Some(vec![font("I"), font("R")]),
            "RI" => Some(vec![font("R"), font("I")]),
            "CW" => Some(vec![font("C")]),
            _ => None,
        };
        if let Some(fonts) = fonts {
            let args = arguments(rest);
            if args.is_empty() {
                self.font_next = Some(fonts);
            } else if matches!(name, "B" | "I" | "SB" | "SM" | "CW") {
                // One font for all its words, with their spaces.
                self.alternating(b, &fonts, &[args.join(" ")]);
            } else {
                self.alternating(b, &fonts, &args);
            }
            return;
        }
        match name {
            "TH" => {
                let a = arguments(rest);
                let title = match (a.first(), a.get(1)) {
                    (Some(t), Some(s)) => format!("{t}({s})"),
                    (Some(t), None) => t.clone(),
                    _ => String::new(),
                };
                self.fill = true;
                b.heading(1);
                self.words(b, &title);
                b.end_heading();
                let said: Vec<String> = a
                    .iter()
                    .skip(2)
                    .filter(|s| !s.is_empty())
                    .cloned()
                    .collect();
                if !said.is_empty() {
                    let said = said.join(" · ");
                    let words: String = self.pieces(&said).into_iter().map(|(t, _)| t).collect();
                    b.caption(&words);
                }
            }
            "SH" | "SS" => {
                self.close(b);
                b.heading(if name == "SH" { 2 } else { 3 });
                if rest.is_empty() {
                    self.tag_next = false;
                    self.font_next = None;
                }
                let words = arguments(rest).join(" ");
                self.words(b, &words);
                b.end_heading();
            }
            "PP" | "LP" | "P" | "HP" | "sp" => self.paragraph(b),
            "br" => {
                if self.fill {
                    b.words("\n", false);
                } else {
                    self.nofill.push(String::new());
                }
            }
            "TP" | "TQ" => {
                self.paragraph(b);
                self.tag_next = true;
            }
            "IP" => {
                self.paragraph(b);
                let a = arguments(rest);
                let tag = a.first().cloned().unwrap_or_default();
                let shown: String = self.pieces(&tag).into_iter().map(|(t, _)| t).collect();
                let short = shown.chars().count() <= 4;
                b.quote();
                self.under = Some(Under::Tagged);
                if shown.is_empty() {
                } else if short {
                    let mark = match shown.trim() {
                        "*" | "o" | "-" | "+" | "·" => "•".to_owned(),
                        m => m.to_owned(),
                    };
                    b.mark(&mark);
                } else {
                    // A long tag, an option: its own line over the body.
                    b.end_quote();
                    b.flush();
                    self.words(b, &tag);
                    b.flush();
                    b.quote();
                }
            }
            "RS" => {
                self.paragraph(b);
                b.quote();
                self.indents += 1;
            }
            "RE" => {
                self.paragraph(b);
                if self.indents > 0 {
                    b.end_quote();
                    self.indents -= 1;
                }
            }
            "nf" | "EX" => {
                b.flush();
                self.fill = false;
            }
            "fi" | "EE" => {
                self.end_nofill(b);
                self.fill = true;
            }
            "ft" => {
                let f = arguments(rest).first().cloned().unwrap_or_default();
                self.previous = self.font;
                self.font = Font::named(&f).unwrap_or_default();
            }
            "UR" | "MT" => {
                let a = arguments(rest);
                self.link = Some((
                    a.first().cloned().unwrap_or_default().replace("\\:", ""),
                    false,
                ));
            }
            "UE" | "ME" => {
                if let Some((url, said)) = self.link.take()
                    && !said
                {
                    b.folded(" ");
                    b.look.link += 1;
                    b.folded(&url);
                    b.look.link -= 1;
                }
                let tail = arguments(rest).join(" ");
                if !tail.is_empty() {
                    self.words(b, &tail);
                }
            }
            "ds" | "ds1" => {
                let (key, value) = rest.split_once([' ', '\t']).unwrap_or((rest, ""));
                let value = value.trim_start();
                let value = value.strip_prefix('"').unwrap_or(value);
                self.strings.insert(key.to_owned(), value.to_owned());
            }
            "SY" => {
                self.paragraph(b);
                let a = arguments(rest);
                let saved = self.font;
                self.font = font("B");
                self.words(b, &a.join(" "));
                self.font = saved;
            }
            "OP" => {
                let a = arguments(rest);
                b.folded(" [");
                let saved = self.font;
                self.font = font("B");
                self.words(b, a.first().map_or("", String::as_str));
                self.font = font("I");
                if let Some(arg) = a.get(1) {
                    b.folded(" ");
                    self.words(b, arg);
                }
                self.font = saved;
                b.folded("]");
            }
            "YS" => self.paragraph(b),
            "so" | "mso" => {
                b.flush();
                b.caption(&format!("see {rest}"));
            }
            _ => {}
        }
    }

    /// `tbl`: options, formats, then rows until `.TE`; `T{ … T}` a cell of
    /// many lines.
    fn tbl(&mut self, lines: &[&str], mut i: usize, b: &mut Builder) -> usize {
        self.paragraph(b);
        let mut tab = '\t';
        // Options end in `;`.
        if let Some(first) = lines.get(i)
            && first.trim_end().ends_with(';')
        {
            if let Some(at) = first.find("tab(")
                && let Some(c) = first[at + 4..].chars().next()
            {
                tab = c;
            }
            i += 1;
        }
        // Formats end in `.`.
        while let Some(l) = lines.get(i) {
            i += 1;
            if l.trim_end().ends_with('.') {
                break;
            }
        }
        let table = b.table(Vec::new());
        let mut first_row = true;
        let mut rows = 0;
        while let Some(&line) = lines.get(i) {
            if line.starts_with(".TE") {
                i += 1;
                break;
            }
            if line.starts_with(".T&") {
                i += 1;
                while let Some(l) = lines.get(i) {
                    i += 1;
                    if l.trim_end().ends_with('.') {
                        break;
                    }
                }
                continue;
            }
            let t = line.trim();
            if t == "_" || t == "=" {
                i += 1;
                continue;
            }
            // A row: its cells, a `T{` cell running on to its `T}`.
            let mut cells: Vec<Vec<String>> = Vec::new();
            let mut rest = line.to_owned();
            i += 1;
            loop {
                let (cell, more) = match rest.find(tab) {
                    Some(at) => (
                        rest[..at].to_owned(),
                        Some(rest[at + tab.len_utf8()..].to_owned()),
                    ),
                    None => (rest.clone(), None),
                };
                if cell.trim_end() == "T{" || cell.starts_with("T{") && cell.trim_end().len() == 2 {
                    let mut block = Vec::new();
                    let mut after = None;
                    while let Some(&l) = lines.get(i) {
                        i += 1;
                        if let Some(tail) = l.strip_prefix("T}") {
                            after = Some(tail.to_owned());
                            break;
                        }
                        block.push(l.to_owned());
                    }
                    cells.push(block);
                    match after.as_deref().and_then(|t| t.strip_prefix(tab)) {
                        Some(next) => {
                            rest = next.to_owned();
                            continue;
                        }
                        None => break,
                    }
                }
                cells.push(vec![cell]);
                match more {
                    Some(m) => rest = m,
                    None => break,
                }
            }
            if !table {
                continue;
            }
            // The first row is the heading when a rule is under it.
            let ruled = lines.get(i).is_some_and(|l| matches!(l.trim(), "_" | "="));
            b.row(first_row && ruled);
            first_row = false;
            rows += 1;
            for cell in cells {
                b.cell();
                let saved = (self.fill, self.under, self.indents);
                self.fill = true;
                self.under = None;
                self.indents = 0;
                let refs: Vec<&str> = cell.iter().map(String::as_str).collect();
                self.lines(&refs, b);
                self.end_nofill(b);
                if self.under.take().is_some() {
                    b.end_quote();
                }
                while self.indents > 0 {
                    b.end_quote();
                    self.indents -= 1;
                }
                (self.fill, self.under, self.indents) = saved;
            }
            if rows > scour_flow::MOST_ROWS {
                break;
            }
        }
        if table {
            b.end_table();
        }
        i
    }

    /// An `mdoc` request; false for one `man` reads too.
    fn mdoc_request(&mut self, name: &str, rest: &str, b: &mut Builder) -> bool {
        let args = arguments(rest);
        match name {
            "Dd" => {
                let date = args.join(" ");
                // A date version control keeps: `$Mdocdate: May 5 2026 $`.
                let date = date.strip_prefix("$Mdocdate:").unwrap_or(&date);
                self.date = date.trim().trim_end_matches('$').trim().to_owned();
            }
            "Dt" => {
                self.fill = true;
                let title = match (args.first(), args.get(1)) {
                    (Some(t), Some(s)) => format!("{t}({s})"),
                    (Some(t), None) => t.clone(),
                    _ => String::new(),
                };
                b.heading(1);
                self.words(b, &title);
                b.end_heading();
                if !self.date.is_empty() {
                    b.caption(&self.date.clone());
                }
            }
            "Os" => {}
            "Sm" => {
                self.unspaced = match args.first().map(String::as_str) {
                    Some("off") => true,
                    Some("on") => false,
                    _ => !self.unspaced,
                };
            }
            "Sh" | "Ss" => {
                self.close(b);
                b.heading(if name == "Sh" { 2 } else { 3 });
                self.inline(b, &args);
                b.end_heading();
            }
            "Pp" | "Lp" => self.mdoc_paragraph(b),
            "Nd" => {
                b.folded(" — ");
                self.inline(b, &args);
            }
            "Bl" => {
                self.mdoc_paragraph(b);
                let kind = match args.first().map(String::as_str) {
                    Some("-bullet" | "-dash" | "-hyphen") => MdocList::Bullet,
                    Some("-enum") => MdocList::Number,
                    Some("-column") => MdocList::Column,
                    Some("-item") => MdocList::Plain,
                    _ => MdocList::Tag,
                };
                match kind {
                    MdocList::Bullet => b.list(None),
                    MdocList::Number => b.list(Some(1)),
                    MdocList::Column => {
                        self.columns = b.table(Vec::new());
                    }
                    _ => b.quote(),
                }
                self.lists.push(kind);
            }
            "El" => {
                self.mdoc_paragraph(b);
                match self.lists.pop() {
                    Some(MdocList::Bullet | MdocList::Number) => b.end_list(),
                    Some(MdocList::Column) => {
                        if self.columns {
                            b.end_table();
                            self.columns = false;
                        }
                    }
                    Some(_) => b.end_quote(),
                    None => {}
                }
            }
            "It" => {
                self.mdoc_paragraph(b);
                match self.lists.last() {
                    Some(MdocList::Bullet | MdocList::Number) => {
                        b.item();
                        self.inline(b, &args);
                    }
                    Some(MdocList::Column) if self.columns => {
                        b.row(false);
                        b.cell();
                        for a in args.split(|a| a == "Ta") {
                            self.inline(b, a);
                            b.cell();
                        }
                    }
                    Some(MdocList::Tag) => {
                        b.end_quote();
                        b.flush();
                        b.look.bold += 1;
                        self.inline(b, &args);
                        b.look.bold -= 1;
                        b.flush();
                        b.quote();
                    }
                    _ => self.inline(b, &args),
                }
            }
            "Bd" => {
                self.mdoc_paragraph(b);
                if args.iter().any(|a| a == "-literal" || a == "-unfilled") {
                    self.display = true;
                } else {
                    b.quote();
                    self.indents += 1;
                }
            }
            "Ed" => {
                if self.display {
                    self.display = false;
                    self.end_nofill(b);
                } else {
                    b.flush();
                    if self.indents > 0 {
                        b.end_quote();
                        self.indents -= 1;
                    }
                }
            }
            "Dl" => {
                b.flush();
                let pieces = self.mdoc_pieces(&args);
                b.code_block(&pieces.into_iter().map(|(t, _)| t).collect::<String>());
            }
            "D1" => {
                b.flush();
                b.quote();
                self.inline(b, &args);
                b.end_quote();
            }
            "Nm" if self.name.is_empty() => {
                self.name = args.first().cloned().unwrap_or_default();
                self.inline(b, &[&["Nm".to_owned()][..], &args].concat());
            }
            "Rs" | "Re" | "Ex" | "Rv" | "Bk" | "Ek" | "Bf" | "Ef" | "Bc" | "Xc" => {}
            _ if is_callable(name) => {
                self.inline(b, &[&[name.to_owned()][..], &args].concat());
            }
            _ => return false,
        }
        true
    }

    fn mdoc_paragraph(&mut self, b: &mut Builder) {
        self.end_nofill(b);
        b.flush();
    }

    /// An `mdoc` line's words: callable macros among them each change what
    /// follows, punctuation closes up to the word before it.
    fn inline(&mut self, b: &mut Builder, args: &[String]) {
        let pieces = self.mdoc_pieces(args);
        // Lines are words apart, but not after an opening bracket or
        // before a closing one.
        let closes = pieces.first().is_some_and(|(w, _)| {
            w.starts_with([']', ')', '”', '’', '⟩', '}', '.', ',', ':', ';', '?', '!'])
        });
        if !closes && !self.glued {
            b.folded(" ");
        }
        self.glued = pieces
            .last()
            .is_some_and(|(w, _)| w.ends_with(['[', '(', '“', '‘', '⟨', '{']));
        for (words, font) in pieces {
            b.look.bold += u8::from(font.bold);
            b.look.italic += u8::from(font.italic);
            b.look.code += u8::from(font.code);
            if font
                == (Font {
                    bold: false,
                    italic: false,
                    code: false,
                })
                && words.starts_with("\u{1}")
            {
                b.look.link += 1;
                b.folded(&words[1..]);
                b.look.link -= 1;
            } else {
                b.folded(&words);
            }
            b.look.bold -= u8::from(font.bold);
            b.look.italic -= u8::from(font.italic);
            b.look.code -= u8::from(font.code);
        }
    }

    fn mdoc_pieces(&mut self, args: &[String]) -> Vec<(String, Font)> {
        let plain = Font::default();
        let bold = Font {
            bold: true,
            ..plain
        };
        let italic = Font {
            italic: true,
            ..plain
        };
        let code = Font {
            code: true,
            ..plain
        };
        let mut out: Vec<(String, Font)> = Vec::new();
        let mut font = plain;
        // Closers for open enclosures, innermost last.
        // With whether each closes at its own macro (`Oo` … `Oc`) or at
        // the line's end (`Op`).
        let mut open: Vec<(&'static str, bool)> = Vec::new();
        let mut space = false;
        let mut no_space = false;
        let unspaced = self.unspaced;
        let mut prefix = String::new();
        let mut i = 0;
        let put = |out: &mut Vec<(String, Font)>,
                   words: String,
                   f: Font,
                   space: &mut bool,
                   no_space: &mut bool| {
            let closing = matches!(
                words.as_str(),
                "." | "," | ":" | ";" | ")" | "]" | "?" | "!"
            );
            if *space && !closing && !*no_space && !unspaced {
                out.push((" ".to_owned(), plain));
            }
            *no_space = matches!(words.as_str(), "(" | "[");
            out.push((words, f));
            *space = true;
        };
        while i < args.len() {
            let a = args[i].as_str();
            i += 1;
            let enclose = |s: &str| -> Option<(&'static str, &'static str)> {
                Some(match s {
                    "Dq" | "Do" => ("“", "”"),
                    "Sq" | "So" => ("‘", "’"),
                    "Pq" | "Po" => ("(", ")"),
                    "Bq" | "Bo" => ("[", "]"),
                    "Brq" | "Bro" => ("{", "}"),
                    "Aq" | "Ao" => ("⟨", "⟩"),
                    "Qq" | "Qo" => ("\"", "\""),
                    "Op" | "Oo" => ("[", "]"),
                    "Eo" => ("", ""),
                    _ => return None,
                })
            };
            if let Some((left, right)) = enclose(a) {
                let o = format!("{prefix}{left}");
                prefix.clear();
                put(&mut out, o, plain, &mut space, &mut no_space);
                no_space = true;
                // The open forms close at their own macro; the rest at the line's end.
                open.push((right, a.ends_with('o')));
                font = plain;
                continue;
            }
            match a {
                "Dc" | "Sc" | "Pc" | "Bc" | "Brc" | "Ac" | "Qc" | "Oc" | "Ec" => {
                    if let Some((close, _)) = open.pop() {
                        out.push((close.to_owned(), plain));
                    }
                    font = plain;
                    continue;
                }
                "Ns" => {
                    no_space = true;
                    continue;
                }
                // An item's head going on over lines: nothing of its own.
                "Xo" | "Xc" => continue,
                "No" | "Li" => {
                    font = if a == "Li" { code } else { plain };
                    continue;
                }
                "Ta" => {
                    put(&mut out, "\t".to_owned(), plain, &mut space, &mut no_space);
                    continue;
                }
                "Ap" => {
                    out.push(("'".to_owned(), plain));
                    no_space = true;
                    continue;
                }
                "Pf" => {
                    if let Some(p) = args.get(i) {
                        prefix = p.clone();
                        i += 1;
                    }
                    continue;
                }
                "Fl" => {
                    // `-` before each word, and alone with none.
                    let start = i;
                    while i < args.len() && !is_callable(&args[i]) && !is_punct(&args[i]) {
                        i += 1;
                    }
                    if start == i {
                        put(&mut out, "-".to_owned(), bold, &mut space, &mut no_space);
                    }
                    for w in &args[start..i] {
                        put(&mut out, format!("-{w}"), bold, &mut space, &mut no_space);
                    }
                    font = plain;
                    continue;
                }
                "Ar" if args.get(i).is_none_or(|n| is_callable(n) || is_punct(n)) => {
                    put(
                        &mut out,
                        "file ...".to_owned(),
                        italic,
                        &mut space,
                        &mut no_space,
                    );
                    continue;
                }
                "Nm" if args.get(i).is_none_or(|n| is_callable(n) || is_punct(n)) => {
                    let name = self.name.clone();
                    put(&mut out, name, bold, &mut space, &mut no_space);
                    continue;
                }
                "Xr" => {
                    let page = args.get(i).cloned().unwrap_or_default();
                    let section = args
                        .get(i + 1)
                        .filter(|s| !is_callable(s) && !is_punct(s))
                        .cloned();
                    i += 1 + usize::from(section.is_some());
                    let said = match section {
                        Some(s) => format!("{page}({s})"),
                        None => page,
                    };
                    put(&mut out, said, plain, &mut space, &mut no_space);
                    continue;
                }
                "Lk" | "Mt" => {
                    let url = args.get(i).cloned().unwrap_or_default();
                    i += 1;
                    let mut said = Vec::new();
                    while i < args.len() && !is_callable(&args[i]) && !is_punct(&args[i]) {
                        said.push(args[i].clone());
                        i += 1;
                    }
                    let words = if said.is_empty() { url } else { said.join(" ") };
                    put(
                        &mut out,
                        format!("\u{1}{words}"),
                        plain,
                        &mut space,
                        &mut no_space,
                    );
                    continue;
                }
                "Fn" | "Fo" => {
                    let name = args.get(i).cloned().unwrap_or_default();
                    i += 1;
                    let mut fa = Vec::new();
                    while i < args.len() && !is_callable(&args[i]) && !is_punct(&args[i]) {
                        fa.push(args[i].clone());
                        i += 1;
                    }
                    put(
                        &mut out,
                        format!("{name}({})", fa.join(", ")),
                        bold,
                        &mut space,
                        &mut no_space,
                    );
                    continue;
                }
                "Ux" | "Bx" | "Ox" | "Nx" | "Fx" | "Dx" | "Bsx" | "At" | "St" => {
                    let said = match a {
                        "Ux" => "UNIX",
                        "Bx" => "BSD",
                        "Ox" => "OpenBSD",
                        "Nx" => "NetBSD",
                        "Fx" => "FreeBSD",
                        "Dx" => "DragonFly",
                        "Bsx" => "BSD/OS",
                        _ => "",
                    };
                    if !said.is_empty() {
                        put(&mut out, said.to_owned(), plain, &mut space, &mut no_space);
                    }
                    continue;
                }
                _ => {}
            }
            if let Some(f) = callable_font(a) {
                font = f;
                continue;
            }
            // Punctuation that ends the line ends what the line opened first.
            if is_punct(a) && args[i - 1..].iter().all(|w| is_punct(w)) {
                while let Some(&(close, false)) = open.last() {
                    out.push((close.to_owned(), plain));
                    open.pop();
                }
            }
            // A word in the font the last macro set.
            let pieces = self.pieces(a);
            let words: String = pieces.into_iter().map(|(t, _)| t).collect();
            let words = format!("{}{words}", std::mem::take(&mut prefix));
            let f = if is_punct(a) { plain } else { font };
            put(&mut out, words, f, &mut space, &mut no_space);
        }
        while let Some((close, _)) = open.pop() {
            out.push((close.to_owned(), plain));
        }
        out
    }
}

fn is_punct(a: &str) -> bool {
    matches!(
        a,
        "." | "," | ":" | ";" | ")" | "]" | "?" | "!" | "(" | "[" | "|"
    )
}

/// The font an inline `mdoc` macro sets for the words after it.
fn callable_font(name: &str) -> Option<Font> {
    let plain = Font::default();
    Some(match name {
        "Cm" | "Ic" | "Sy" | "Nm" | "Fd" | "In" | "Ft" if name != "Ft" => Font {
            bold: true,
            ..plain
        },
        "Ar" | "Pa" | "Em" | "Va" | "Fa" | "Ft" | "Vt" | "Ev" | "Dv" | "Er" | "Ad" | "An"
        | "Cd" | "Lb" | "Ms" | "Tn" | "Sx" | "Ot" => {
            if matches!(name, "Ar" | "Pa" | "Em" | "Va" | "Fa" | "Ft" | "Vt") {
                Font {
                    italic: true,
                    ..plain
                }
            } else {
                plain
            }
        }
        "Ql" => Font {
            code: true,
            ..plain
        },
        _ => return None,
    })
}

fn is_callable(name: &str) -> bool {
    callable_font(name).is_some()
        || matches!(
            name,
            "Dq" | "Do"
                | "Dc"
                | "Sq"
                | "So"
                | "Sc"
                | "Pq"
                | "Po"
                | "Pc"
                | "Bq"
                | "Bo"
                | "Bc"
                | "Brq"
                | "Bro"
                | "Brc"
                | "Aq"
                | "Ao"
                | "Ac"
                | "Qq"
                | "Qo"
                | "Qc"
                | "Op"
                | "Oo"
                | "Oc"
                | "Eo"
                | "Ec"
                | "Ns"
                | "No"
                | "Li"
                | "Ta"
                | "Ap"
                | "Pf"
                | "Fl"
                | "Xr"
                | "Lk"
                | "Mt"
                | "Fn"
                | "Fo"
                | "Ux"
                | "Bx"
                | "Ox"
                | "Nx"
                | "Fx"
                | "Dx"
                | "Bsx"
                | "At"
                | "St"
                | "Xo"
                | "Xc"
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use scour_flow::{Block, Para};

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

    #[test]
    fn a_man_page_in_its_fonts() {
        let page = ".\\\" yorum\n.TH LS 1 \"2026-11-28\" \"GNU\"\n.SH AD\nls \\- dizini listeler\n.SH KULLANIM\n\
                    .B ls\n[\\fIseçenek\\fR]... \\fB\\-a\\fP sonra\n.TP\n.BR \\-l \", \" \\-\\-long\n\
                    Uzun biçim.\n.IP \\(bu 2\nmadde\n.PP\n.nf\nkod  satırı\n.fi\n\
                    .de AU\n.B \\\\$1\n..\n.AU Ada\n.ds ad Scour\nAdı \\*(ad\\(em tamam.\n";
        let flow = flow(page.as_bytes()).unwrap();
        let p = paras(&flow);
        let all = texts(&flow);
        assert_eq!((p[0].heading, all[0].as_str()), (1, "LS(1)"));
        assert_eq!(all[1], "2026-11-28 · GNU");
        assert_eq!((p[2].heading, all[2].as_str()), (2, "AD"));
        assert_eq!(all[3], "ls - dizini listeler");
        assert_eq!(all[5], "ls [seçenek]... -a sonra");
        assert!(p[5].runs.iter().any(|r| r.bold && r.text == "ls"));
        assert!(p[5].runs.iter().any(|r| r.italic && r.text == "seçenek"));
        assert!(p[5].runs.iter().any(|r| r.bold && r.text == "-a"));
        assert_eq!(all[6], "-l, --long");
        assert!(p[7].indent > p[6].indent && all[7] == "Uzun biçim.");
        assert_eq!((p[8].mark.as_str(), all[8].as_str()), ("•", "madde"));
        assert!(p[9].runs[0].code && all[9] == "kod  satırı");
        assert_eq!(all[10], "Ada Adı Scour— tamam.");
        assert!(p[10].runs[0].bold);
    }

    #[test]
    fn tbl_tables() {
        let page = ".TH T 1\n.TS\ntab(@);\nl l.\nAd@Sayı\n_\nT{\nuzun\nhücre\nT}@3\n.TE\n";
        let flow = flow(page.as_bytes()).unwrap();
        let table = flow
            .blocks
            .iter()
            .find_map(|b| match b {
                Block::Table(t) => Some(t),
                _ => None,
            })
            .unwrap();
        assert!(table.rows[0].head);
        assert_eq!(table.rows[1].cells[0].paras[0].text(), "uzun hücre");
        assert_eq!(table.rows[1].cells[1].paras[0].text(), "3");
    }

    #[test]
    fn an_mdoc_page() {
        let page = ".Dd November 28, 2026\n.Dt CAT 1\n.Os\n.Sh NAME\n.Nm cat\n.Nd birleştirir\n\
                   .Sh SYNOPSIS\n.Nm\n.Op Fl benstuv\n.Op Ar\n.Sh DESCRIPTION\n.Bl -tag -width Ds\n\
                   .It Fl b\nBoş olmayanları numaralar.\n.El\nBkz.\n.Xr ls 1 .\n";
        let flow = flow(page.as_bytes()).unwrap();
        let p = paras(&flow);
        let all = texts(&flow);
        assert_eq!((p[0].heading, all[0].as_str()), (1, "CAT(1)"));
        assert_eq!(all[1], "November 28, 2026");
        assert_eq!(all[3], "cat — birleştirir");
        assert_eq!(all[5], "cat [-benstuv] [file ...]");
        assert_eq!(all[7], "-b");
        assert!(p[7].runs[0].bold);
        assert_eq!(all[8], "Boş olmayanları numaralar.");
        assert_eq!(all[9], "Bkz. ls(1).");
        // Punctuation after an enclosure goes outside it; `.Sm off` joins.
        let page = ".Dd x\n.Dt X 1\n.Sh A\n.Sm off\n.Oo user @ Oc host Op : port .\n.Sm on\n";
        let all = texts(&super::flow(page.as_bytes()).unwrap());
        assert_eq!(all.last().unwrap(), "[user@]host[:port].");
    }

    #[test]
    fn what_is_not_roff_is_refused() {
        assert!(flow(b"2026-11-28 10:00 started\n2026-11-28 10:01 stopped\n").is_err());
        assert!(flow(b"\x7fELF\x02\x01\x01\0\0\0").is_err());
    }
}
