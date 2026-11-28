//! Org as a flow, read as Org mode lays it out: the title and the keywords
//! that matter to a reader, headlines with their state and tags, planning
//! lines, paragraphs, lists nested by their indent (check boxes,
//! description items), tables, blocks (source, example, quote, verse and
//! the rest), fixed-width lines, rules, and inline emphasis, links and
//! footnotes, gathered at the end. Drawers, comments and settings say
//! nothing on the page.

use std::cell::RefCell;

use scour_flow::Flow;

use crate::build::Builder;
use crate::inline::{Escape, MOST_DEPTH, Mark, Rules, broken, dedent, flat, indent, least_indent};

/// The document in `bytes`.
pub fn flow(bytes: &[u8]) -> Result<Flow, String> {
    let text = String::from_utf8_lossy(bytes).replace("\r\n", "\n");
    let lines: Vec<&str> = text.lines().collect();
    let mut doc = Doc {
        todo: vec!["TODO".into(), "DONE".into()],
        ..Doc::default()
    };
    // The states a `#+TODO:` line names, before the headlines that use them.
    for line in &lines {
        if let Some(value) = keyword(line)
            .and_then(|(k, v)| matches!(k.as_str(), "todo" | "seq_todo" | "typ_todo").then_some(v))
        {
            doc.todo.extend(
                value
                    .split_whitespace()
                    .filter(|w| *w != "|")
                    .map(|w| w.split('(').next().unwrap_or(w).to_owned()),
            );
        }
    }
    doc.titled = lines
        .iter()
        .any(|l| keyword(l).is_some_and(|(k, _)| k == "title"));
    let mut b = Builder::default();
    doc.blocks(&lines, &mut b);
    doc.footnotes(&mut b);
    Ok(b.finish())
}

const PAIRS: &[(&str, &str, Mark)] = &[
    ("*", "*", Mark::Bold),
    ("/", "/", Mark::Italic),
    ("_", "_", Mark::Underline),
    ("+", "+", Mark::Strike),
    ("=", "=", Mark::Code),
    ("~", "~", Mark::Code),
];

#[derive(Default)]
struct Doc {
    todo: Vec<String>,
    /// A `#+TITLE:` is the first heading, and headlines sit a level under it.
    titled: bool,
    title_drawn: bool,
    /// Footnotes by label, as they are met: their words, once defined.
    notes: RefCell<Vec<(String, Option<String>)>>,
    /// What a `#+CAPTION:` said, for the block under it.
    caption: Option<String>,
    /// Blocks inside blocks, now.
    depth: usize,
}

/// A keyword line, `#+KEY: value`, as its key lowercase and its value.
fn keyword(line: &str) -> Option<(String, &str)> {
    let rest = line.trim_start().strip_prefix("#+")?;
    let (key, value) = rest.split_once(':')?;
    (!key.is_empty() && !key.contains(char::is_whitespace))
        .then(|| (key.to_ascii_lowercase(), value.trim()))
}

/// A block's opening line, `#+BEGIN_NAME args`, as its name lowercase and
/// its arguments.
fn begin(line: &str) -> Option<(String, &str)> {
    let rest = line.trim_start();
    let head = rest.get(..8)?;
    if !head.eq_ignore_ascii_case("#+begin_") {
        return None;
    }
    let rest = &rest[8..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    Some((rest[..end].to_ascii_lowercase(), rest[end..].trim()))
}

fn headline(line: &str) -> Option<(usize, &str)> {
    let stars = line.chars().take_while(|c| *c == '*').count();
    let rest = &line[stars..];
    (stars > 0 && (rest.is_empty() || rest.starts_with([' ', '\t']))).then(|| (stars, rest.trim()))
}

/// A list item's line: the column of its bullet, whether it is numbered,
/// its words, and its number.
fn item(line: &str) -> Option<(usize, bool, &str, Option<u64>)> {
    let at = indent(line);
    let t = line.trim_start();
    let words = |n: usize| {
        let rest = &t[n..];
        (rest.is_empty() || rest.starts_with([' ', '\t'])).then(|| rest.trim_start())
    };
    if let Some(c) = t.chars().next()
        && (c == '-' || c == '+' || (c == '*' && at > 0))
        && let Some(rest) = words(1)
    {
        // A rule of dashes is not an item.
        if !(c == '-' && t.trim_end().len() >= 5 && t.trim_end().chars().all(|x| x == '-')) {
            return Some((at, false, rest, None));
        }
    }
    let digits = t.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
    if digits > 0
        && t[digits..].starts_with(['.', ')'])
        && let Some(rest) = words(digits + 1)
    {
        return Some((at, true, rest, t[..digits].parse().ok()));
    }
    let mut chars = t.chars();
    if let (Some(c), Some('.' | ')')) = (chars.next(), chars.next())
        && c.is_ascii_alphabetic()
        && let Some(rest) = words(2)
    {
        return Some((
            at,
            true,
            rest,
            Some(u64::from(c.to_ascii_lowercase() as u8 - b'a' + 1)),
        ));
    }
    None
}

fn drawer(line: &str) -> bool {
    let t = line.trim();
    t.len() > 2
        && t.starts_with(':')
        && t.ends_with(':')
        && t[1..t.len() - 1]
            .chars()
            .all(|c| c.is_alphanumeric() || c == '-' || c == '_')
}

fn rule(line: &str) -> bool {
    let t = line.trim();
    t.len() >= 5 && t.chars().all(|c| c == '-')
}

fn fixed(line: &str) -> Option<&str> {
    let t = line.trim_start();
    if t == ":" {
        return Some("");
    }
    t.strip_prefix(": ")
}

fn table_line(line: &str) -> bool {
    line.trim_start().starts_with('|')
}

fn comment(line: &str) -> bool {
    let t = line.trim_start();
    t == "#" || t.starts_with("# ")
}

fn footnote_definition(line: &str) -> Option<(&str, &str)> {
    let rest = line.strip_prefix("[fn:")?;
    let end = rest.find(']')?;
    let label = &rest[..end];
    (!label.is_empty() && !label.contains(':')).then(|| (label, rest[end + 1..].trim()))
}

/// The line that closes the block `name` opened at `i`: one of the same
/// name inside it closes first.
fn block_end(lines: &[&str], i: usize, name: &str) -> usize {
    let close = format!("#+end_{name}");
    let mut depth = 0;
    for (k, line) in lines.iter().enumerate().skip(i + 1) {
        if begin(line).is_some_and(|(n, _)| n == name) {
            depth += 1;
        } else if line.trim().eq_ignore_ascii_case(&close) {
            if depth == 0 {
                return k;
            }
            depth -= 1;
        }
    }
    lines.len()
}

/// A line that starts a block of its own, ending a paragraph before it.
fn starts_block(line: &str) -> bool {
    let t = line.trim_start();
    headline(line).is_some()
        || item(line).is_some()
        || table_line(line)
        || t.starts_with("#+")
        || comment(line)
        || drawer(line)
        || rule(line)
        || fixed(line).is_some()
        || footnote_definition(line).is_some()
}

/// Org's own inline constructs: links, footnotes, targets, macros.
fn special(text: &str, edge: bool, doc: &Doc) -> Option<(usize, String, bool)> {
    if let Some(rest) = text.strip_prefix("[fn:") {
        let end = rest.find(']')?;
        let inner = &rest[..end];
        let (label, words) = match inner.split_once(':') {
            Some((label, words)) => (label, Some(words.trim().to_owned())),
            None => (inner, None),
        };
        let mut notes = doc.notes.borrow_mut();
        let at = match notes
            .iter()
            .position(|(l, _)| !label.is_empty() && l == label)
        {
            Some(at) => {
                if words.is_some() {
                    notes[at].1 = words;
                }
                at
            }
            None => {
                notes.push((label.to_owned(), words));
                notes.len() - 1
            }
        };
        return Some((end + 5, format!("[{}]", at + 1), false));
    }
    // A sub- or superscript in braces, `H_{2}O`, `a^{bc}`.
    if let Some(rest) = text.strip_prefix("_{").or_else(|| text.strip_prefix("^{"))
        && let Some(end) = rest.find('}')
    {
        return Some((end + 3, rest[..end].to_owned(), false));
    }
    if !edge {
        return None;
    }
    if let Some(rest) = text.strip_prefix("[[")
        && let Some(end) = rest.find("]]")
    {
        let inner = &rest[..end];
        let (target, words) = match inner.split_once("][") {
            Some((t, w)) => (t, Some(w)),
            None => (inner, None),
        };
        let target = target.strip_prefix("file:").unwrap_or(target);
        let picture = [".png", ".jpg", ".jpeg", ".gif", ".svg", ".webp"]
            .iter()
            .any(|e| target.to_ascii_lowercase().ends_with(e));
        let shown = match words {
            Some(w) => w.to_owned(),
            None if picture => format!("🖼 {target}"),
            None => target.trim_start_matches(['*', '#']).to_owned(),
        };
        return Some((end + 4, shown, !picture || words.is_some()));
    }
    if let Some(rest) = text.strip_prefix("<<<")
        && let Some(end) = rest.find(">>>")
    {
        return Some((end + 6, rest[..end].to_owned(), false));
    }
    if let Some(rest) = text.strip_prefix("<<")
        && let Some(end) = rest.find(">>")
    {
        return Some((end + 4, String::new(), false));
    }
    if let Some(rest) = text.strip_prefix("{{{")
        && let Some(end) = rest.find("}}}")
    {
        return Some((end + 6, String::new(), false));
    }
    if let Some(rest) = text.strip_prefix("@@")
        && let Some(end) = rest.find("@@")
    {
        return Some((end + 4, String::new(), false));
    }
    for scheme in ["https://", "http://", "mailto:", "ftp://"] {
        if text.starts_with(scheme) {
            let end = text
                .find(|c: char| c.is_whitespace() || "<>\"[]".contains(c))
                .unwrap_or(text.len());
            let url = text[..end].trim_end_matches(['.', ',', ')', ';', ':', '!', '?']);
            return Some((url.len(), url.to_owned(), true));
        }
    }
    None
}

impl Doc {
    /// Lines of words, a hard break between each, into `b`.
    fn read(&self, b: &mut Builder, lines: &[String]) {
        let special = |t: &str, edge: bool| special(t, edge, self);
        let rules = Rules {
            pairs: PAIRS,
            loose: &[],
            special: &special,
            escape: Escape::None,
        };
        broken(lines, b, &rules);
    }

    /// A paragraph of `lines`: one ending `\\` breaks there.
    fn paragraph(&self, b: &mut Builder, lines: &[&str]) {
        let mut parts = vec![String::new()];
        for line in lines {
            let (words, breaks) = match line.trim_end().strip_suffix("\\\\") {
                Some(w) => (w, true),
                None => (line.trim_end(), false),
            };
            let part = parts.last_mut().expect("one at least");
            if !part.is_empty() && !words.trim().is_empty() {
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

    fn quietly(&self, b: &mut Builder, words: &str) {
        b.flush();
        b.look.quiet += 1;
        self.read(b, &[words.to_owned()]);
        b.look.quiet -= 1;
        b.flush();
    }

    fn take_caption(&mut self, b: &mut Builder) {
        if let Some(c) = self.caption.take() {
            b.caption(&c);
        }
    }

    fn footnotes(&self, b: &mut Builder) {
        let notes = self.notes.borrow().clone();
        if notes.iter().all(|(_, w)| w.is_none()) {
            return;
        }
        b.rule();
        for (n, (_, words)) in notes.iter().enumerate() {
            if let Some(words) = words {
                self.quietly(b, &format!("[{}] {words}", n + 1));
            }
        }
    }

    fn headline(&self, b: &mut Builder, stars: usize, rest: &str) {
        let mut words = rest;
        let mut state = None;
        if let Some(first) = words.split_whitespace().next()
            && self.todo.iter().any(|k| k == first)
        {
            state = Some(first);
            words = words[first.len()..].trim_start();
        }
        // A priority cookie, `[#A]`.
        if words.starts_with("[#") && words.get(3..4) == Some("]") {
            words = words[4..].trim_start();
        }
        // Tags, `:work:home:`, at the end.
        let mut tags = "";
        if let Some(at) = words.rfind([' ', '\t'])
            && let tail = words[at..].trim()
            && tail.len() > 2
            && tail.starts_with(':')
            && tail.ends_with(':')
            && !tail.contains(' ')
        {
            tags = tail;
            words = words[..at].trim_end();
        }
        let level = (stars + usize::from(self.titled)).min(6) as u8;
        b.heading(level);
        if let Some(state) = state {
            b.look.quiet += 1;
            b.words(&format!("{state} "), false);
            b.look.quiet -= 1;
        }
        self.read(b, &[words.to_owned()]);
        if !tags.is_empty() {
            b.look.quiet += 1;
            b.words(
                &format!("  {}", tags.trim_matches(':').replace(':', " · ")),
                false,
            );
            b.look.quiet -= 1;
        }
        b.end_heading();
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
        let mut i = 0;
        while i < lines.len() && !b.full() {
            let line = lines[i];
            let t = line.trim();
            if t.is_empty() || comment(line) {
                i += 1;
                continue;
            }
            if let Some((stars, rest)) = headline(line) {
                self.headline(b, stars, rest);
                i += 1;
                // Planning: deadlines and schedules under it.
                while let Some(l) = lines.get(i)
                    && ["SCHEDULED:", "DEADLINE:", "CLOSED:"]
                        .iter()
                        .any(|p| l.trim_start().starts_with(p))
                {
                    self.quietly(b, l.trim());
                    i += 1;
                }
                continue;
            }
            if drawer(line) && !t.eq_ignore_ascii_case(":end:") {
                let end = lines[i + 1..]
                    .iter()
                    .position(|l| l.trim().eq_ignore_ascii_case(":end:"));
                if let Some(end) = end {
                    i += end + 2;
                    continue;
                }
            }
            if let Some((name, args)) = begin(line) {
                let end = block_end(lines, i, &name);
                self.block(&name, args, &lines[i + 1..end], b);
                i = end + 1;
                continue;
            }
            // A dynamic block, `#+BEGIN: clocktable`, is made by Emacs.
            if t.get(..8)
                .is_some_and(|h| h.eq_ignore_ascii_case("#+begin:"))
            {
                let end = lines[i + 1..]
                    .iter()
                    .position(|l| l.trim().eq_ignore_ascii_case("#+end:"))
                    .map_or(lines.len(), |k| i + 1 + k);
                i = end + 1;
                continue;
            }
            if let Some((key, value)) = keyword(line) {
                match key.as_str() {
                    "title" if !self.title_drawn => {
                        self.title_drawn = true;
                        b.heading(1);
                        self.read(b, &[value.to_owned()]);
                        b.end_heading();
                    }
                    "subtitle" | "author" | "date" | "email" => self.quietly(b, value),
                    "caption" => self.caption = Some(value.to_owned()),
                    _ => {}
                }
                i += 1;
                continue;
            }
            if rule(line) {
                b.rule();
                i += 1;
                continue;
            }
            if fixed(line).is_some() {
                let mut code = Vec::new();
                while let Some(words) = lines.get(i).and_then(|l| fixed(l)) {
                    code.push(words);
                    i += 1;
                }
                self.take_caption(b);
                b.code_block(&code.join("\n"));
                continue;
            }
            if table_line(line) {
                let end = lines[i..]
                    .iter()
                    .position(|l| !table_line(l))
                    .map_or(lines.len(), |k| i + k);
                self.take_caption(b);
                self.table(&lines[i..end], b);
                i = end;
                // Its formulas say nothing.
                while lines
                    .get(i)
                    .is_some_and(|l| keyword(l).is_some_and(|(k, _)| k == "tblfm"))
                {
                    i += 1;
                }
                continue;
            }
            if item(line).is_some() {
                i = self.list(lines, i, b);
                continue;
            }
            if let Some((label, words)) = footnote_definition(line) {
                let mut said = vec![words];
                i += 1;
                while let Some(l) = lines.get(i)
                    && !l.trim().is_empty()
                    && footnote_definition(l).is_none()
                    && headline(l).is_none()
                {
                    said.push(l.trim());
                    i += 1;
                }
                let said = said.join(" ");
                let mut notes = self.notes.borrow_mut();
                match notes.iter().position(|(l, _)| l == label) {
                    Some(at) => notes[at].1 = Some(said),
                    None => notes.push((label.to_owned(), Some(said))),
                }
                continue;
            }
            let end = lines[i + 1..]
                .iter()
                .position(|l| l.trim().is_empty() || starts_block(l))
                .map_or(lines.len(), |k| i + 1 + k);
            self.take_caption(b);
            self.paragraph(b, &lines[i..end]);
            i = end;
        }
    }

    fn block(&mut self, name: &str, args: &str, inner: &[&str], b: &mut Builder) {
        let body = dedent(inner, least_indent(inner));
        // A comma before `*` or `#+` keeps a line from being Org.
        let code = || {
            body.iter()
                .map(|l| {
                    l.strip_prefix(",*")
                        .map(|r| format!("*{r}"))
                        .or_else(|| l.strip_prefix(",#+").map(|r| format!("#+{r}")))
                        .unwrap_or_else(|| (*l).to_owned())
                })
                .collect::<Vec<_>>()
                .join("\n")
        };
        match name {
            "src" => {
                let lang = args.split_whitespace().next().unwrap_or("");
                match self.caption.take() {
                    Some(c) => b.caption(&c),
                    None if !lang.is_empty() => b.caption(lang),
                    None => {}
                }
                b.code_block(&code());
            }
            "example" => {
                self.take_caption(b);
                b.code_block(&code());
            }
            "comment" => {}
            "export" if args.eq_ignore_ascii_case("html") => {
                crate::html::read(&inner.join("\n"), b, &mut |_| None);
            }
            "export" => {}
            "verse" => {
                b.quote();
                for stanza in body
                    .split(|l| l.trim().is_empty())
                    .filter(|s| !s.is_empty())
                {
                    let lines: Vec<String> = stanza.iter().map(|l| l.trim().to_owned()).collect();
                    b.flush();
                    self.read(b, &lines);
                    b.flush();
                }
                b.end_quote();
            }
            "center" => self.blocks(&body, b),
            "quote" => {
                b.quote();
                self.blocks(&body, b);
                b.end_quote();
            }
            // A special block, `#+BEGIN_NOTE`: a quote under its name.
            other => {
                let mut said = other.replace(['_', '-'], " ");
                if let Some(c) = said.get_mut(..1) {
                    c.make_ascii_uppercase();
                }
                b.quote();
                b.caption(&said);
                self.blocks(&body, b);
                b.end_quote();
            }
        }
    }

    /// A list from `i`: items at one indent, each with what is indented
    /// under it; two blank lines end it.
    fn list(&mut self, lines: &[&str], mut i: usize, b: &mut Builder) -> usize {
        let Some((base, numbered, _, first)) = item(lines[i]) else {
            return i + 1;
        };
        b.list(numbered.then(|| first.unwrap_or(1)));
        while let Some((at, ordered, words, _)) = lines.get(i).and_then(|l| item(l)) {
            if at != base || ordered != numbered || b.full() {
                break;
            }
            i += 1;
            // Its body: what is indented past its bullet, blank lines among it.
            let start = i;
            let mut end = i;
            let mut blanks = 0;
            while let Some(l) = lines.get(i) {
                if l.trim().is_empty() {
                    blanks += 1;
                    if blanks >= 2 {
                        break;
                    }
                } else if indent(l) <= base {
                    break;
                } else {
                    blanks = 0;
                    end = i + 1;
                }
                i += 1;
            }
            i = end;
            let body = dedent(&lines[start..end], least_indent(&lines[start..end]));
            b.item();
            let mut words = words;
            if let Some(rest) = words.strip_prefix("[@")
                && let Some(close) = rest.find(']')
            {
                words = rest[close + 1..].trim_start();
            }
            for (tick, done) in [
                ("[X] ", true),
                ("[x] ", true),
                ("[ ] ", false),
                ("[-] ", false),
            ] {
                if let Some(rest) = words.strip_prefix(tick) {
                    b.task(done);
                    words = rest;
                }
            }
            // Its first paragraph runs on over the body's first lines.
            let more = body
                .iter()
                .position(|l| l.trim().is_empty() || starts_block(l))
                .unwrap_or(body.len());
            let mut first: Vec<&str> = vec![words];
            first.extend(&body[..more]);
            match (!numbered).then(|| words.split_once(" :: ")).flatten() {
                Some((term, rest)) => {
                    b.flush();
                    b.look.bold += 1;
                    self.read(b, &[term.trim().to_owned()]);
                    b.look.bold -= 1;
                    b.words(" — ", false);
                    let mut rest_lines = vec![rest];
                    rest_lines.extend(&body[..more]);
                    let joined: Vec<String> = vec![
                        rest_lines
                            .iter()
                            .map(|l| l.trim())
                            .collect::<Vec<_>>()
                            .join(" "),
                    ];
                    self.read(b, &joined);
                    b.flush();
                }
                None => self.paragraph(b, &first),
            }
            self.blocks(&body[more..], b);
            // One blank line between items keeps the list going.
            let next = lines[i..]
                .iter()
                .take(2)
                .position(|l| !l.trim().is_empty())
                .map(|k| i + k);
            match next {
                Some(n)
                    if item(lines[n]).is_some_and(|(a, o, _, _)| a == base && o == numbered) =>
                {
                    i = n
                }
                _ => break,
            }
        }
        b.end_list();
        i
    }

    fn table(&mut self, lines: &[&str], b: &mut Builder) {
        let mut rows: Vec<Vec<String>> = Vec::new();
        let mut head = 0;
        for line in lines {
            let t = line.trim();
            if t.starts_with("|-") {
                if head == 0 && !rows.is_empty() {
                    head = rows.len();
                }
                continue;
            }
            let t = t.strip_prefix('|').unwrap_or(t);
            let t = t.strip_suffix('|').unwrap_or(t);
            let cells: Vec<String> = t.split('|').map(|c| c.trim().to_owned()).collect();
            // A row of widths and alignments, `<l>` `<10>`, is for Emacs.
            if cells.iter().all(|c| {
                c.is_empty()
                    || (c.starts_with('<')
                        && c.ends_with('>')
                        && c[1..c.len() - 1]
                            .chars()
                            .all(|x| x.is_ascii_digit() || "lrc".contains(x)))
            }) {
                continue;
            }
            rows.push(cells);
        }
        // A rule under the last row only is no heading.
        if head == rows.len() {
            head = 0;
        }
        if rows.is_empty() || !b.table(Vec::new()) {
            return;
        }
        for (n, row) in rows.into_iter().enumerate() {
            b.row(n < head);
            for cell in row {
                b.cell();
                self.read(b, &[cell]);
            }
        }
        b.end_table();
    }
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
    fn title_headlines_and_inline() {
        let org = "#+TITLE: Notlar\n#+AUTHOR: Ada\n#+STARTUP: overview\n\n\
                   * TODO [#A] Görev :iş:ev:\n  SCHEDULED: <2026-11-28 Cmt>\n  :PROPERTIES:\n  :ID: x\n  :END:\n\
                   Bir *kalın* /eğik/ _altı_ +çizik+ =kod= ~kod2~ [[https://x.org][bağ]] [[resim.png]]\n\
                   ve /usr/bin/ yolu.[fn:1] satır\\\\\nkırılır\n\n# yorum\n\n[fn:1] Dipnot.\n";
        let flow = flow(org.as_bytes()).unwrap();
        let p = paras(&flow);
        let all = texts(&flow);
        assert_eq!((p[0].heading, all[0].as_str()), (1, "Notlar"));
        assert_eq!(all[1], "Ada");
        assert_eq!(p[2].heading, 2);
        assert_eq!(all[2], "TODO Görev  iş · ev");
        assert_eq!(all[3], "SCHEDULED: <2026-11-28 Cmt>");
        assert_eq!(
            all[4],
            "Bir kalın eğik altı çizik kod kod2 bağ 🖼 resim.png ve usr/bin yolu.[1] satır\nkırılır"
        );
        let runs = &p[4].runs;
        assert!(runs.iter().any(|r| r.bold && r.text == "kalın"));
        assert!(runs.iter().any(|r| r.italic && r.text == "eğik"));
        assert!(
            runs.iter()
                .any(|r| r.underline && r.text == "altı" && !r.strike)
        );
        assert!(runs.iter().any(|r| r.strike && r.text == "çizik"));
        assert!(runs.iter().any(|r| r.code && r.text == "kod2"));
        assert!(!all.iter().any(|t| t.contains("yorum") || t.contains("ID")));
        assert_eq!(all.last().unwrap(), "[1] Dipnot.");
    }

    #[test]
    fn lists_tables_and_blocks() {
        let org = "- bir\n  devam\n  - alt\n- [X] iki\n- terim :: tanım\n\n1. ilk\n2. ikinci\n\n\
                   | ad | sayı |\n|----+------|\n| elma | 3 |\n#+TBLFM: $2=1\n\n\
                   #+begin_src rust\nfn main() {}\n#+end_src\n\n: sabit\n: satırlar\n\n\
                   #+BEGIN_QUOTE\nSöz.\n#+END_QUOTE\n\n-----\n";
        let flow = flow(org.as_bytes()).unwrap();
        let p = paras(&flow);
        let marks: Vec<(&str, String)> = p.iter().map(|p| (p.mark.as_str(), p.text())).collect();
        assert_eq!(marks[0], ("•", "bir devam".into()));
        assert_eq!(marks[1], ("◦", "alt".into()));
        assert!(p[1].indent > p[0].indent);
        assert_eq!(marks[2], ("☑", "iki".into()));
        assert_eq!(marks[3], ("•", "terim — tanım".into()));
        assert!(p[3].runs[0].bold);
        assert_eq!(marks[4], ("1.", "ilk".into()));
        let table = flow
            .blocks
            .iter()
            .find_map(|b| match b {
                Block::Table(t) => Some(t),
                _ => None,
            })
            .unwrap();
        assert!(table.rows[0].head && !table.rows[1].head);
        assert_eq!(table.rows.len(), 2);
        let all = texts(&flow);
        assert!(all.iter().any(|t| t == "rust"));
        assert!(all.iter().any(|t| t == "fn main() {}"));
        assert!(all.iter().any(|t| t == "sabit\nsatırlar"));
        assert!(all.iter().any(|t| t == "Söz."));
        // A quote in a quote closes first.
        let nested =
            "#+begin_quote\ndış\n#+begin_quote\niç\n#+end_quote\nsonra\n#+end_quote\nH_{2}O\n";
        let nested = super::flow(nested.as_bytes()).unwrap();
        let p = paras(&nested);
        assert_eq!(
            p.iter().map(|p| p.text()).collect::<Vec<_>>(),
            ["dış", "iç", "sonra", "H2O"]
        );
        assert!(p[1].indent > p[0].indent && p[2].indent == p[0].indent && p[3].indent == 0.0);
        assert!(matches!(flow.blocks.last(), Some(Block::Break)));
    }
}
