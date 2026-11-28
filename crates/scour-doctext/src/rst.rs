//! reStructuredText as a flow, read as its specification and pandoc's reader
//! read it: titles by their adornment (levels in the order adornments first
//! appear), paragraphs, bullet and enumerated lists, `::` literal blocks,
//! block quotes, field and definition lists, grid and simple tables, line
//! blocks, footnotes, transitions, and the directives a document is made of
//! — code, admonitions, images, math — with the rest passed over.

use std::collections::HashSet;

use scour_flow::{Align, Flow};

use crate::build::Builder;
use crate::inline::{
    Escape, MOST_DEPTH, Mark, Rules, dedent, flat, indent, inline, joined, least_indent,
};

/// The document in `bytes`.
pub fn flow(bytes: &[u8]) -> Result<Flow, String> {
    let text = String::from_utf8_lossy(bytes).replace("\r\n", "\n");
    let lines: Vec<&str> = text.lines().collect();
    let mut b = Builder::default();
    let mut st = State::default();
    blocks(&lines, &mut b, &mut st);
    Ok(b.finish())
}

#[derive(Default)]
struct State {
    /// Adornments in the order they first appear: a title's level.
    styles: Vec<(char, bool)>,
    /// Blocks inside blocks, now.
    depth: usize,
}

const PUNCT: &str = "=-`:'\"~^_*+#<>.!$%&,/;?@[\\]{|}";

/// A line of one punctuation mark repeated: an adornment, or a transition.
fn adornment(line: &str) -> Option<char> {
    let t = line.trim_end();
    let c = t.chars().next()?;
    (t.chars().count() >= 2 && PUNCT.contains(c) && t.chars().all(|x| x == c)).then_some(c)
}

fn rules() -> Rules<'static> {
    Rules {
        pairs: &[
            ("**", "**", Mark::Bold),
            ("``", "``", Mark::Code),
            ("*", "*", Mark::Italic),
        ],
        loose: &[],
        special: &|text, edge| if edge { special(text) } else { None },
        escape: Escape::Any,
    }
}

/// A role (`:code:`x``), interpreted text (`` `x`:math: ``), a hyperlink
/// (`` `text <url>`_ ``), a reference (`` `text`_ ``, `name_`), a footnote
/// reference (`[1]_`), a substitution (`|name|`) or a bare URL.
fn special(text: &str) -> Option<(usize, String, bool)> {
    if text.starts_with(':')
        && let Some(close) = text[1..].find(":`").map(|i| i + 1)
        && text[1..close]
            .chars()
            .all(|c| c.is_alphanumeric() || "-_.:+".contains(c))
        && let Some(end) = text[close + 2..].find('`').map(|i| i + close + 2)
    {
        return Some((end + 1, role_text(&text[close + 2..end]), false));
    }
    if text.starts_with('`') && !text.starts_with("``") {
        let end = text[1..].find('`')? + 1;
        let inner = &text[1..end];
        let mut n = end + 1;
        let link = text[n..].starts_with('_');
        while text[n..].starts_with('_') {
            n += 1;
        }
        // A role after it: `` `E=mc^2`:math: ``.
        if !link
            && let Some(role) = text[n..].strip_prefix(':')
            && let Some(close) = role.find(':')
            && close > 0
            && role[..close]
                .chars()
                .all(|c| c.is_alphanumeric() || "-_.+".contains(c))
        {
            n += close + 2;
        }
        let shown = match inner.rfind('<') {
            Some(0) if inner.ends_with('>') => &inner[1..inner.len() - 1],
            Some(i) if inner.ends_with('>') => inner[..i].trim_end(),
            _ => inner,
        };
        return Some((n, shown.to_owned(), link));
    }
    if let Some(rest) = text.strip_prefix('[')
        && let Some(end) = rest.find("]_")
        && end > 0
        && !rest[..end].contains(char::is_whitespace)
    {
        return Some((end + 3, format!("[{}]", &rest[..end]), false));
    }
    if let Some(rest) = text.strip_prefix('|')
        && let Some(end) = rest.find('|')
        && end > 0
        && !rest.starts_with(' ')
        && !rest[..end].ends_with(' ')
    {
        let mut n = end + 2;
        while text[n..].starts_with('_') {
            n += 1;
        }
        return Some((n, rest[..end].to_owned(), false));
    }
    for scheme in ["https://", "http://", "mailto:"] {
        if text.starts_with(scheme) {
            let end = text
                .find(|c: char| c.is_whitespace() || "<>\"".contains(c))
                .unwrap_or(text.len());
            let url = text[..end].trim_end_matches(['.', ',', ')', ';', ':']);
            return Some((url.len(), url.to_owned(), true));
        }
    }
    // `name_` and `name__`: a word that is a reference.
    let word = text
        .find(|c: char| !(c.is_alphanumeric() || "-.+".contains(c)))
        .unwrap_or(text.len());
    if word > 0 && text[word..].starts_with('_') {
        let mut n = word;
        while text[n..].starts_with('_') {
            n += 1;
        }
        if n - word <= 2 && !text[n..].starts_with(char::is_alphanumeric) {
            return Some((n, text[..word].to_owned(), true));
        }
    }
    None
}

/// What a role's text shows: `text <target>` its text, Sphinx's `~a.b.c`
/// its last name, `!name` the name.
fn role_text(inner: &str) -> String {
    let inner = match inner.rfind(" <") {
        Some(i) if inner.ends_with('>') => &inner[..i],
        _ => inner,
    };
    let inner = inner.trim_start_matches('!');
    let inner = match inner.strip_prefix('~') {
        Some(path) => path.rsplit('.').next().unwrap_or(path),
        None => inner,
    };
    inner.replace("\\ ", "")
}

fn words(b: &mut Builder, text: &str) {
    inline(text, b, &rules());
}

/// Where a block's body ends: from `from`, past the lines that are blank
/// or indented more than `base`, up to the last such line that says
/// something.
fn body_end(lines: &[&str], from: usize, base: usize) -> usize {
    let mut end = from;
    let mut last = from;
    while end < lines.len() {
        if lines[end].trim().is_empty() {
            end += 1;
            continue;
        }
        if indent(lines[end]) <= base {
            break;
        }
        end += 1;
        last = end;
    }
    last
}

/// The columns `prefix` takes, a tab to the next multiple of eight.
fn columns(prefix: &str) -> usize {
    prefix
        .chars()
        .fold(0, |n, c| if c == '\t' { (n / 8 + 1) * 8 } else { n + 1 })
}

/// `line` from its column `col` on.
fn from_column(line: &str, col: usize) -> &str {
    let mut n = 0;
    for (i, c) in line.char_indices() {
        if n >= col {
            return &line[i..];
        }
        n = if c == '\t' { (n / 8 + 1) * 8 } else { n + 1 };
    }
    ""
}

/// The column an item's text starts at, its marker ending at byte `marker`:
/// after the white space that must follow it, or, with nothing after it on
/// the line, one past it.
fn text_column(line: &str, marker: usize) -> Option<usize> {
    let after = &line[marker..];
    if after.trim().is_empty() {
        return Some(columns(&line[..marker]) + 1);
    }
    if !after.starts_with([' ', '\t']) {
        return None;
    }
    let space = after.len() - after.trim_start_matches([' ', '\t']).len();
    Some(columns(&line[..marker + space]))
}

/// A bullet item's line: where its text starts.
fn bullet(line: &str) -> Option<usize> {
    let t = line.trim_start();
    let c = t.chars().next().filter(|c| "*-+•‣⁃".contains(*c))?;
    text_column(line, line.len() - t.len() + c.len_utf8())
}

/// An enumerator — `1.`, `#.`, `a)`, `(iv)` — as where the item's text
/// starts and the number it gives.
fn enumerator(line: &str) -> Option<(usize, u64)> {
    let t = line.trim_start();
    let lead = line.len() - t.len();
    let (open, rest) = match t.strip_prefix('(') {
        Some(r) => (1, r),
        None => (0, t),
    };
    let label: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '#')
        .collect();
    if label.is_empty() || label.len() > 4 {
        return None;
    }
    let close = rest[label.len()..].chars().next()?;
    if !(close == ')' || (open == 0 && close == '.')) {
        return None;
    }
    let numeric = label.chars().all(|c| c.is_ascii_digit()) || label == "#";
    // A single letter is a letter, but for `i`, which begins a roman list.
    let letter = label.len() == 1
        && label.chars().all(|c| c.is_ascii_alphabetic())
        && !label.eq_ignore_ascii_case("i");
    let roman = !letter
        && (label.chars().all(|c| "ivxlcdm".contains(c))
            || label.chars().all(|c| "IVXLCDM".contains(c)));
    if !(numeric || letter || roman) {
        return None;
    }
    let n = match label.parse() {
        Ok(n) => n,
        Err(_) if letter => u64::from(label.to_ascii_lowercase().as_bytes()[0] - b'a' + 1),
        Err(_) => roman_value(&label).unwrap_or(1),
    };
    let at = text_column(line, lead + open + label.len() + 1)?;
    Some((at, n))
}

fn roman_value(label: &str) -> Option<u64> {
    let digit = |c: char| match c.to_ascii_lowercase() {
        'i' => Some(1),
        'v' => Some(5),
        'x' => Some(10),
        'l' => Some(50),
        'c' => Some(100),
        'd' => Some(500),
        'm' => Some(1000),
        _ => None,
    };
    let values: Vec<i64> = label.chars().map(digit).collect::<Option<_>>()?;
    let mut total = 0;
    for (i, v) in values.iter().enumerate() {
        if values.get(i + 1).is_some_and(|next| next > v) {
            total -= v;
        } else {
            total += v;
        }
    }
    u64::try_from(total).ok()
}

fn blocks(lines: &[&str], b: &mut Builder, st: &mut State) {
    if st.depth >= MOST_DEPTH {
        flat(lines, b);
        return;
    }
    st.depth += 1;
    blocks_here(lines, b, st);
    st.depth -= 1;
}

fn blocks_here(lines: &[&str], b: &mut Builder, st: &mut State) {
    let mut i = 0;
    // The paragraph before ended in `::`: the indented block after it is code.
    let mut literal_next = false;
    while i < lines.len() && !b.full() {
        let line = lines[i];
        let t = line.trim();
        if t.is_empty() {
            i += 1;
            continue;
        }
        // An indented block: literal after `::`, a quote otherwise.
        if indent(line) > 0 {
            let end = body_end(lines, i, 0).max(i + 1);
            let body = dedent(&lines[i..end], least_indent(&lines[i..end]));
            if literal_next {
                b.code_block(&body.join("\n"));
            } else {
                b.quote();
                blocks(&body, b, st);
                b.end_quote();
            }
            literal_next = false;
            i = end;
            continue;
        }
        literal_next = false;
        // A title: overlined and underlined, or underlined.
        if let Some(c) = adornment(line)
            && i + 2 < lines.len()
            && !lines[i + 1].trim().is_empty()
            && adornment(lines[i + 1]).is_none()
            && adornment(lines[i + 2]) == Some(c)
        {
            title(b, st, (c, true), lines[i + 1].trim());
            i += 3;
            continue;
        }
        if adornment(line).is_none()
            && let Some(c) = lines.get(i + 1).and_then(|l| adornment(l))
            && lines[i + 1].trim().chars().count() >= t.chars().count().min(4)
        {
            title(b, st, (c, false), t);
            i += 2;
            continue;
        }
        if adornment(line).is_some() && t.chars().count() >= 4 {
            b.rule();
            i += 1;
            continue;
        }
        if t.starts_with(".. ") || t == ".." {
            i = directive(lines, i, b, st);
            continue;
        }
        // An anonymous target: `__ url`.
        if t.starts_with("__ ") {
            i = body_end(lines, i + 1, 0).max(i + 1);
            continue;
        }
        if t.starts_with("+-") && t.ends_with('+') {
            i = grid_table(lines, i, b, st);
            continue;
        }
        if t.starts_with('=') && t.contains(' ') && is_rule(t) {
            i = simple_table(lines, i, b);
            continue;
        }
        if bullet(line).is_some() {
            i = list(lines, i, b, st, false);
            continue;
        }
        if enumerator(line).is_some()
            && lines
                .get(i + 1)
                .is_none_or(|l| l.trim().is_empty() || enumerator(l).is_some() || indent(l) > 0)
        {
            i = list(lines, i, b, st, true);
            continue;
        }
        if t.starts_with("| ") || t == "|" {
            i = line_block(lines, i, b);
            continue;
        }
        // A field: `:Name: value`.
        if let Some(rest) = t.strip_prefix(':')
            && let Some(end) = rest
                .find(": ")
                .or_else(|| rest.strip_suffix(':').map(str::len))
            && end > 0
            && !rest[..end].contains('`')
        {
            b.flush();
            b.look.bold += 1;
            b.words(&format!("{}: ", &rest[..end]), false);
            b.look.bold -= 1;
            let end_body = body_end(lines, i + 1, 0);
            let mut value = rest.get(end + 2..).unwrap_or("").trim().to_owned();
            if end_body > i + 1 {
                value.push(' ');
                value.push_str(&joined(&lines[i + 1..end_body]));
            }
            words(b, value.trim());
            b.flush();
            i = end_body.max(i + 1);
            continue;
        }
        // A paragraph, or a definition list's term over its indented definition.
        let start = i;
        while i < lines.len()
            && !lines[i].trim().is_empty()
            && indent(lines[i]) == 0
            && !(i > start && lines.get(i + 1).and_then(|l| adornment(l)).is_some())
        {
            i += 1;
        }
        if i == start + 1
            && lines
                .get(i)
                .is_some_and(|l| !l.trim().is_empty() && indent(l) > 0)
        {
            b.flush();
            b.look.bold += 1;
            // `term : classifier`
            words(b, t.split(" : ").next().unwrap_or(t));
            b.look.bold -= 1;
            b.flush();
            continue;
        }
        let mut para = joined(&lines[start..i]);
        if para.ends_with("::") {
            literal_next = true;
            para.pop();
            // `Text ::` and `::` alone keep no colon at all.
            if para == ":" || para.ends_with(" :") {
                para.pop();
            }
        }
        b.flush();
        words(b, para.trim_end());
        b.flush();
    }
}

fn title(b: &mut Builder, st: &mut State, style: (char, bool), text: &str) {
    let level = match st.styles.iter().position(|s| *s == style) {
        Some(n) => n + 1,
        None => {
            st.styles.push(style);
            st.styles.len()
        }
    };
    b.heading(level.min(6) as u8);
    words(b, text);
    b.end_heading();
}

/// A list from `i`, numbered or bulleted; each item's text and its indented
/// body read as blocks of their own.
fn list(lines: &[&str], mut i: usize, b: &mut Builder, st: &mut State, numbered: bool) -> usize {
    let at = |l: &str| {
        if numbered {
            enumerator(l).map(|(at, _)| at)
        } else {
            bullet(l)
        }
    };
    b.list(if numbered {
        enumerator(lines[i]).map(|(_, n)| n)
    } else {
        None
    });
    while i < lines.len() && !b.full() {
        let line = lines[i];
        let Some(start) = at(line).filter(|_| indent(line) == 0) else {
            break;
        };
        let end = body_end(lines, i + 1, start.saturating_sub(1)).max(i + 1);
        let mut item = vec![from_column(line, start)];
        item.extend(dedent(&lines[i + 1..end], start));
        b.item();
        blocks(&item, b, st);
        i = end;
        // Past blank lines, an item of the same list, or the list has ended.
        let next = lines[i..]
            .iter()
            .position(|l| !l.trim().is_empty())
            .map(|n| i + n);
        match next {
            Some(n) if indent(lines[n]) == 0 && at(lines[n]).is_some() => i = n,
            _ => break,
        }
    }
    b.end_list();
    i
}

/// A line block from `i`: `| ` lines kept as lines, an indented line
/// carrying the one before on, a bare `|` between stanzas.
fn line_block(lines: &[&str], mut i: usize, b: &mut Builder) -> usize {
    let mut stanzas: Vec<Vec<String>> = vec![Vec::new()];
    while i < lines.len() {
        let line = lines[i];
        let t = line.trim_start();
        if let Some(said) = t
            .strip_prefix('|')
            .filter(|s| s.is_empty() || s.starts_with(' '))
        {
            let said = said.get(1..).unwrap_or("");
            if said.trim().is_empty() {
                stanzas.push(Vec::new());
            } else {
                // Its indent past the bar's one space is kept.
                let lead = said.len() - said.trim_start().len();
                let stanza = stanzas.last_mut().expect("one at least");
                stanza.push(format!("{}{}", "\u{a0}".repeat(lead), said.trim()));
            }
        } else if indent(line) > 0 && !t.is_empty() {
            match stanzas.last_mut().and_then(|s| s.last_mut()) {
                Some(last) => {
                    last.push(' ');
                    last.push_str(t);
                }
                None => break,
            }
        } else {
            break;
        }
        i += 1;
    }
    for stanza in stanzas.iter().filter(|s| !s.is_empty()) {
        b.flush();
        for (n, line) in stanza.iter().enumerate() {
            if n > 0 {
                b.words("\n", false);
            }
            words(b, line);
        }
        b.flush();
    }
    i
}

/// A directive, a footnote or a comment from `i`, and where the line after
/// its body is.
fn directive(lines: &[&str], i: usize, b: &mut Builder, st: &mut State) -> usize {
    let t = lines[i].trim();
    let end = body_end(lines, i + 1, 0).max(i + 1);
    let body: Vec<&str> = dedent(&lines[i + 1..end], least_indent(&lines[i + 1..end]));
    // A footnote or a citation: shown where it is written, quietly.
    if let Some(rest) = t.strip_prefix(".. [")
        && let Some(close) = rest.find(']')
        && close > 0
    {
        let first = format!("[{}] {}", &rest[..close], rest[close + 1..].trim());
        let mut note = vec![first.as_str()];
        note.extend(body.iter().copied());
        b.look.quiet += 1;
        blocks(&note, b, st);
        b.look.quiet -= 1;
        return end;
    }
    let Some((name, arg)) = t[3.min(t.len())..].split_once("::") else {
        // A comment or a target: nothing shown.
        return end;
    };
    let name = name.trim().to_ascii_lowercase();
    let arg = arg.trim();
    // A directive's options (`:linenos:`) come first in its body.
    let content: Vec<&str> = body
        .iter()
        .skip_while(|l| l.trim_start().starts_with(':'))
        .copied()
        .collect();
    let said = |name: &str| {
        let mut s = name.replace('-', " ");
        if let Some(c) = s.get_mut(..1) {
            c.make_ascii_uppercase();
        }
        s
    };
    match name.as_str() {
        "code" | "code-block" | "sourcecode" => {
            if !arg.is_empty() {
                b.caption(arg);
            }
            b.code_block(content.join("\n").trim_matches('\n'));
        }
        "literalinclude" | "include" | "availability" => {
            b.caption(&format!("{}: {arg}", said(&name)));
        }
        // A body of its own after a title in its argument.
        "admonition" | "topic" | "sidebar" | "versionadded" | "versionchanged" | "deprecated"
        | "seealso" | "rubric" => {
            b.quote();
            b.caption(&match name.as_str() {
                "admonition" | "topic" | "sidebar" | "rubric" => arg.to_owned(),
                "versionadded" => format!("Added in version {arg}"),
                "versionchanged" => format!("Changed in version {arg}"),
                "deprecated" => format!("Deprecated since version {arg}"),
                _ if arg.is_empty() => said(&name),
                _ => format!("{} {arg}", said(&name)),
            });
            blocks(&content, b, st);
            b.end_quote();
        }
        // The argument is the body's first words.
        "note" | "warning" | "tip" | "important" | "caution" | "danger" | "attention" | "hint"
        | "error" => {
            b.quote();
            b.caption(&said(&name));
            let mut all = vec![arg];
            all.extend(body.iter().copied());
            blocks(&all, b, st);
            b.end_quote();
        }
        "image" | "figure" => {
            b.flush();
            b.look.quiet += 1;
            b.words(&format!("🖼 {arg}"), false);
            b.look.quiet -= 1;
            b.flush();
            if name == "figure" {
                blocks(&content, b, st);
            }
        }
        "math" => {
            let math = format!("{arg}\n{}", content.join("\n"));
            b.code_block(math.trim());
        }
        "epigraph" | "pull-quote" | "highlights" | "container" | "only" | "rst-class"
        | "compound" | "class" => {
            b.quote();
            blocks(&content, b, st);
            b.end_quote();
        }
        "csv-table" | "list-table" | "table" => {
            if !arg.is_empty() {
                b.caption(arg);
            }
            blocks(&content, b, st);
        }
        "parsed-literal" | "line-block" | "doctest" | "testcode" | "testoutput"
        | "productionlist" => b.code_block(content.join("\n").trim_matches('\n')),
        // What says nothing on a page of its own.
        "raw" | "highlight" | "default-role" | "toctree" | "index" | "meta" | "contents"
        | "sectnum" | "role" | "testsetup" | "testcleanup" | "module" | "currentmodule"
        | "tabularcolumns" | "sectionauthor" | "moduleauthor" | "codeauthor" | "title"
        | "header" | "footer" | "target-notes" | "cssclass" => {}
        _ if name.starts_with("auto") => {}
        // Any other — Sphinx's `function`, `class`, `option` and the rest —
        // is its argument, a signature, over its body.
        _ => {
            // A signature goes on over the lines up to a blank one or an option.
            let more = body
                .iter()
                .take_while(|l| !l.trim().is_empty() && !l.trim_start().starts_with(':'))
                .count();
            for line in std::iter::once(arg).chain(body[..more].iter().map(|l| l.trim())) {
                if !line.is_empty() {
                    b.flush();
                    b.look.bold += 1;
                    b.words(line, true);
                    b.look.bold -= 1;
                    b.flush();
                }
            }
            let content: Vec<&str> = body[more..]
                .iter()
                .skip_while(|l| l.trim_start().starts_with(':'))
                .copied()
                .collect();
            b.quote();
            blocks(&content, b, st);
            b.end_quote();
        }
    }
    end
}

/// A grid table from `i`: each cell found as the rectangle its corners
/// close, so a cell may span columns and rows; what is in one is read as
/// blocks.
fn grid_table(lines: &[&str], mut i: usize, b: &mut Builder, st: &mut State) -> usize {
    let start = i;
    while i < lines.len() && matches!(lines[i].trim_start().chars().next(), Some('+' | '|')) {
        i += 1;
    }
    let lead = indent(lines[start]);
    let grid: Vec<Vec<char>> = lines[start..i]
        .iter()
        .map(|l| from_column(l, lead).trim_end().chars().collect())
        .collect();
    let at = |r: usize, c: usize| {
        grid.get(r)
            .and_then(|row| row.get(c))
            .copied()
            .unwrap_or(' ')
    };
    let height = grid.len();
    let width = grid.iter().map(Vec::len).max().unwrap_or(0);
    // The heading ends at a border of `=`.
    let head_end = grid
        .iter()
        .position(|r| r.first() == Some(&'+') && r.contains(&'='));
    let mut cells = Vec::new();
    let mut corners = vec![(0usize, 0usize)];
    let mut seen = HashSet::new();
    while let Some((top, left)) = corners.pop() {
        if cells.len() > 20_000 || !seen.insert((top, left)) || top + 1 >= height {
            continue;
        }
        if let Some((bottom, right)) = cell_at(&at, top, left, height, width) {
            cells.push((top, left, bottom, right));
            corners.push((top, right));
            corners.push((bottom, left));
        }
    }
    cells.sort_unstable();
    // The columns' edges, for how many a cell spans.
    let mut edges: Vec<usize> = cells.iter().flat_map(|c| [c.1, c.3]).collect();
    edges.sort_unstable();
    edges.dedup();
    let mut tops: Vec<usize> = cells.iter().map(|c| c.0).collect();
    tops.dedup();
    b.table(Vec::new());
    for &row in &tops {
        // The row's own cells, and an empty one where a cell from a row
        // above runs down through it: the model spans columns, not rows.
        let mut here: Vec<(usize, usize, usize, bool)> = cells
            .iter()
            .filter(|c| c.0 == row || (c.0 < row && row < c.2))
            .map(|&(top, left, bottom, right)| (left, right, bottom, top == row))
            .collect();
        here.sort_unstable();
        b.row(head_end.is_some_and(|h| here.iter().all(|c| c.2 <= h)));
        for (left, right, bottom, own) in here {
            b.cell();
            b.span(edges.iter().filter(|&&e| e > left && e <= right).count() as u32);
            if !own {
                continue;
            }
            let said: Vec<String> = (row + 1..bottom)
                .map(|r| (left + 1..right).map(|c| at(r, c)).collect())
                .collect();
            let said: Vec<&str> = said.iter().map(String::as_str).collect();
            blocks(&dedent(&said, least_indent(&said)), b, st);
        }
    }
    b.end_table();
    i
}

/// The bottom-right corner of the cell whose top-left corner is at
/// (`top`, `left`): along the top edge to a `+` whose right edge leads down
/// to a `+` whose bottom and left edges lead back.
fn cell_at(
    at: &dyn Fn(usize, usize) -> char,
    top: usize,
    left: usize,
    height: usize,
    width: usize,
) -> Option<(usize, usize)> {
    if at(top, left) != '+' {
        return None;
    }
    for right in left + 1..width {
        match at(top, right) {
            '+' => {
                for bottom in top + 1..height {
                    match at(bottom, right) {
                        '+' if at(bottom, left) == '+'
                            && (left + 1..right)
                                .all(|c| matches!(at(bottom, c), '-' | '=' | '+'))
                            && (top + 1..bottom).all(|r| matches!(at(r, left), '|' | '+')) =>
                        {
                            return Some((bottom, right));
                        }
                        '+' | '|' => {}
                        _ => break,
                    }
                }
            }
            '-' | '=' => {}
            _ => return None,
        }
    }
    None
}

/// The characters of `line` from column `from` up to column `to`: a table
/// is laid out in characters, not bytes.
fn between(line: &str, from: usize, to: usize) -> String {
    line.chars()
        .skip(from)
        .take(to.saturating_sub(from))
        .collect()
}

fn is_rule(line: &str) -> bool {
    let t = line.trim();
    t.starts_with('=') && t.chars().all(|c| c == '=' || c == ' ')
}

/// A simple table from `i`: columns under runs of `=`, a row a line, a line
/// with its first column empty carrying the row above on. Rows between the
/// first two of three rules are the heading.
fn simple_table(lines: &[&str], mut i: usize, b: &mut Builder) -> usize {
    let rule = lines[i];
    let mut spans = Vec::new();
    let mut open = None;
    for (n, c) in rule.chars().chain(std::iter::once(' ')).enumerate() {
        match (c, open) {
            ('=', None) => open = Some(n),
            (' ', Some(s)) => {
                spans.push((s, n));
                open = None;
            }
            _ => {}
        }
    }
    i += 1;
    let mut parts: Vec<Vec<&str>> = vec![Vec::new()];
    while i < lines.len() {
        let line = lines[i];
        i += 1;
        if is_rule(line) {
            // A rule with a blank line or the end after it closes the table.
            if parts.len() == 2 || lines.get(i).is_none_or(|l| l.trim().is_empty()) {
                break;
            }
            parts.push(Vec::new());
            continue;
        }
        parts.last_mut().expect("one at least").push(line);
    }
    let head = parts.len() == 2;
    b.table(vec![Align::Left; spans.len()]);
    for (n, part) in parts.iter().enumerate() {
        let mut rows: Vec<Vec<String>> = Vec::new();
        for line in part {
            let t = line.trim();
            if t.is_empty() || t.chars().all(|c| c == '-' || c == ' ') {
                continue;
            }
            let cells: Vec<String> = spans
                .iter()
                .enumerate()
                .map(|(k, &(s, e))| {
                    // The last column runs to the line's end.
                    let e = if k + 1 == spans.len() { usize::MAX } else { e };
                    between(line, s, e).trim().to_owned()
                })
                .collect();
            match rows.last_mut() {
                Some(last) if cells.first().is_some_and(String::is_empty) => {
                    for (into, more) in last.iter_mut().zip(cells) {
                        if !more.is_empty() {
                            into.push(' ');
                            into.push_str(&more);
                        }
                    }
                }
                _ => rows.push(cells),
            }
        }
        for row in rows {
            b.row(head && n == 0);
            for cell in row {
                b.cell();
                words(b, &cell);
            }
        }
    }
    b.end_table();
    i
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
    fn titles_lists_code_and_directives() {
        let rst = "=====\nBaşlık\n=====\n\nAlt\n---\n\nBir **kalın** ve *eğik* ``kod`` \
                   `bağ <https://x>`_ ve :code:`rol` ve ref_ ve\\ birleşik.\n\n\
                   -\tbir\n-\tiki\n\n  devamı\n\n#. ilk\n#. ikinci\n\nÖrnek::\n\n    fn main() {}\n\
                   \n        içeride\n\n.. note:: Dikkat\n\n   içinde\n\n.. code-block:: rust\n\n   \
                   let x = 1;\n\n.. comment gizli\n\n:Yazar: Ada\n\nterim\n    tanım\n";
        let flow = flow(rst.as_bytes()).unwrap();
        let p = paras(&flow);
        let all: Vec<String> = p.iter().map(|p| p.text()).collect();
        assert_eq!((p[0].heading, all[0].as_str()), (1, "Başlık"));
        assert_eq!((p[1].heading, all[1].as_str()), (2, "Alt"));
        assert_eq!(
            all[2],
            "Bir kalın ve eğik kod bağ ve rol ve ref vebirleşik."
        );
        assert!(p[2].runs.iter().any(|r| r.bold && r.text == "kalın"));
        assert!(p[2].runs.iter().any(|r| r.code && r.text == "kod"));
        assert!(p[2].runs.iter().any(|r| r.underline && r.text == "bağ"));
        assert!(p[2].runs.iter().any(|r| r.underline && r.text == "ref"));
        assert_eq!((p[3].mark.as_str(), all[3].as_str()), ("•", "bir"));
        assert_eq!(all[5], "devamı");
        assert_eq!((p[6].mark.as_str(), all[6].as_str()), ("1.", "ilk"));
        assert_eq!(all[8], "Örnek:");
        assert!(p[9].runs[0].code);
        assert_eq!(all[9], "fn main() {}\n\n    içeride");
        assert!(all.iter().any(|t| t == "Note"));
        assert!(all.iter().any(|t| t == "Dikkat"));
        assert!(all.iter().any(|t| t == "içinde"));
        assert!(all.iter().any(|t| t == "let x = 1;"));
        assert!(!all.iter().any(|t| t.contains("gizli")));
        assert!(all.iter().any(|t| t == "Yazar: Ada"));
        assert!(all.iter().any(|t| t == "terim") && all.iter().any(|t| t == "tanım"));
    }

    #[test]
    fn grid_and_simple_tables() {
        let rst = "+-----+------+\n| a   | b    |\n+=====+======+\n| 1   | iki  |\n|     | satır|\n\
                   +-----+------+\n| geniş tek  |\n+------------+\n\n\
                   =====  =====\nad     sayı\n=====  =====\nelma   3\n       dört\n=====  =====\n\n\
                   Sonra\n\n=====  =====\nx      y\n=====  =====\n\nSon\n";
        let flow = flow(rst.as_bytes()).unwrap();
        let t = tables(&flow);
        assert_eq!(t.len(), 3);
        assert!(t[0].rows[0].head && !t[0].rows[1].head);
        assert_eq!(t[0].rows[1].cells[1].paras[0].text(), "iki satır");
        assert_eq!(t[0].rows[2].cells[0].span, 2);
        // A cell two rows high leaves an empty cell in the second.
        let tall = "+---+---+\n| a | b |\n+   +---+\n|   | c |\n+---+---+\n";
        let tall = super::flow(tall.as_bytes()).unwrap();
        let rows = &tables(&tall)[0].rows;
        assert_eq!(rows[1].cells.len(), 2);
        assert!(rows[1].cells[0].paras.is_empty());
        assert_eq!(rows[1].cells[1].paras[0].text(), "c");
        assert_eq!(t[1].rows.len(), 2);
        assert!(t[1].rows[0].head);
        assert_eq!(t[1].rows[1].cells[1].paras[0].text(), "3 dört");
        // Two rules only: no heading, and what follows is not the table's.
        assert!(!t[2].rows[0].head);
        assert_eq!(t[2].rows.len(), 1);
        assert!(paras(&flow).iter().any(|p| p.text() == "Son"));
    }

    #[test]
    fn line_blocks_and_footnotes() {
        let rst = "| bir\n|   iki\n  sürer\n|\n| üç\n\nMetin [1]_.\n\n.. [1] Not\n   devam.\n";
        let flow = flow(rst.as_bytes()).unwrap();
        let all: Vec<String> = paras(&flow).iter().map(|p| p.text()).collect();
        assert_eq!(all[0], "bir\n\u{a0}\u{a0}iki sürer");
        assert_eq!(all[1], "üç");
        assert_eq!(all[2], "Metin [1].");
        assert_eq!(all[3], "[1] Not devam.");
    }

    #[test]
    fn enumerators_in_their_kinds() {
        assert_eq!(enumerator("3. x").map(|e| e.1), Some(3));
        assert_eq!(enumerator("(c) x").map(|e| e.1), Some(3));
        assert_eq!(enumerator("iv) x").map(|e| e.1), Some(4));
        assert_eq!(enumerator("#.\tx").map(|e| e.0), Some(8));
        assert_eq!(enumerator("Version 8. x"), None);
        assert_eq!(enumerator("e.g. x"), None);
    }
}
