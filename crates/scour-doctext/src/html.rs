//! HTML — a page, an e-book's chapter, a message's body — as flowing blocks.
//! Read leniently, as pages are written: tags left open, attributes
//! unquoted, entities by name or by number. What runs or styles a page —
//! scripts, style sheets, forms — is left out; a preview runs nothing, and
//! what it draws is Scour's own paragraphs, never the page's markup.

use scour_flow::Flow;

use crate::build::Builder;

/// The page in `bytes`, read as UTF-8 (a stray byte becomes U+FFFD).
pub fn flow(bytes: &[u8]) -> Result<Flow, String> {
    let text = String::from_utf8_lossy(bytes);
    let mut b = Builder::default();
    read(&text, &mut b, &mut |_| None);
    Ok(b.finish())
}

/// Elements whose content is not words to show: skipped to their end tag.
const SKIPPED: &[&str] = &[
    "script", "style", "head", "title", "noscript", "template", "svg", "math", "iframe", "object",
    "canvas", "select", "textarea", "button", "nav",
];

/// Elements that never hold anything, so never end.
const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];

/// Elements that are blocks: a paragraph ends where one starts or ends.
const BLOCKS: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "header",
    "footer",
    "main",
    "aside",
    "figure",
    "figcaption",
    "address",
    "dl",
    "dt",
    "dd",
    "details",
    "summary",
    "center",
    "form",
    "fieldset",
    "legend",
    "body",
    "html",
    "caption",
    "hgroup",
    "picture",
];

/// Read `html` into `b`. `picture` gives a picture's bytes for its `src`, if
/// the caller has them — an e-book holds its own.
pub(crate) fn read(html: &str, b: &mut Builder, picture: &mut dyn FnMut(&str) -> Option<Vec<u8>>) {
    let bytes = html.as_bytes();
    let mut at = 0;
    // How deep inside a table that is not the one being read.
    let mut inner_tables = 0usize;
    let mut head_rows = false;
    while at < bytes.len() && !b.full() {
        let Some(lt) = memchr::memchr(b'<', &bytes[at..]).map(|i| at + i) else {
            b.folded(&decode(&html[at..]));
            break;
        };
        if lt > at {
            b.folded(&decode(&html[at..lt]));
        }
        at = lt;
        let rest = &html[at..];
        if rest.starts_with("<!--") {
            at += rest.find("-->").map_or(rest.len(), |i| i + 3);
            continue;
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            at += rest.find('>').map_or(rest.len(), |i| i + 1);
            continue;
        }
        let Some((tag, len)) = Tag::parse(rest) else {
            // A `<` that opens no tag is a word.
            b.folded("<");
            at += 1;
            continue;
        };
        at += len;
        let name = tag.name.as_str();
        if !tag.end && !tag.closed && SKIPPED.contains(&name) {
            at += skip_to_end(&html[at..], name);
            continue;
        }
        // What the page hides is not shown: a help box, a menu's copy.
        if !tag.end && tag.hidden() {
            if !tag.closed && !VOID.contains(&name) {
                at += skip_element(&html[at..], name);
            }
            continue;
        }
        if !tag.end && name == "pre" {
            let len = skip_to_end(&html[at..], "pre");
            let inner = &html[at..at + len];
            let inner = inner.rfind("</").map_or(inner, |end| &inner[..end]);
            b.code_block(&decode(&strip_tags(inner)));
            at += len;
            continue;
        }
        if tag.end {
            close(b, name, &mut inner_tables, &mut head_rows);
        } else {
            open(b, &tag, &mut inner_tables, &mut head_rows, picture);
        }
    }
}

fn open(
    b: &mut Builder,
    tag: &Tag,
    inner_tables: &mut usize,
    head_rows: &mut bool,
    picture: &mut dyn FnMut(&str) -> Option<Vec<u8>>,
) {
    let name = tag.name.as_str();
    match name {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => b.heading(name.as_bytes()[1] - b'0'),
        "br" => b.words("\n", false),
        "hr" => b.rule(),
        "ul" | "menu" => b.list(None),
        "ol" => b.list(Some(
            tag.attr("start")
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(1),
        )),
        "li" => b.item(),
        "blockquote" => b.quote(),
        "b" | "strong" => b.look.bold += 1,
        "i" | "em" | "cite" | "var" | "dfn" => b.look.italic += 1,
        "u" | "ins" => b.look.underline += 1,
        "s" | "del" | "strike" => b.look.strike += 1,
        "a" if tag.attr("href").is_some() => b.look.link += 1,
        "code" | "kbd" | "samp" | "tt" => b.look.code += 1,
        "table" => {
            if !b.table(Vec::new()) {
                *inner_tables += 1;
            }
        }
        "thead" if *inner_tables == 0 => *head_rows = true,
        "tbody" | "tfoot" if *inner_tables == 0 => *head_rows = false,
        "tr" if *inner_tables == 0 => b.row(*head_rows),
        "td" | "th" if *inner_tables == 0 => {
            b.cell();
            if name == "th" {
                b.look.bold += 1;
            }
        }
        "img" => {
            let shown = tag
                .attr("src")
                .and_then(&mut *picture)
                .is_some_and(|data| b.picture(&data, size(tag)));
            if !shown && let Some(alt) = tag.attr("alt").filter(|a| !a.trim().is_empty()) {
                b.look.quiet += 1;
                b.words(&format!("🖼 {} ", decode(alt).trim()), false);
                b.look.quiet -= 1;
            }
        }
        _ if BLOCKS.contains(&name) => b.flush(),
        _ => {}
    }
}

fn close(b: &mut Builder, name: &str, inner_tables: &mut usize, head_rows: &mut bool) {
    match name {
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => b.end_heading(),
        "ul" | "ol" | "menu" => b.end_list(),
        "li" => b.flush(),
        "blockquote" => b.end_quote(),
        "b" | "strong" => b.look.bold = b.look.bold.saturating_sub(1),
        "i" | "em" | "cite" | "var" | "dfn" => b.look.italic = b.look.italic.saturating_sub(1),
        "u" | "ins" => b.look.underline = b.look.underline.saturating_sub(1),
        "s" | "del" | "strike" => b.look.strike = b.look.strike.saturating_sub(1),
        "a" => b.look.link = b.look.link.saturating_sub(1),
        "code" | "kbd" | "samp" | "tt" => b.look.code = b.look.code.saturating_sub(1),
        "table" => {
            if *inner_tables > 0 {
                *inner_tables -= 1;
            } else {
                b.end_table();
                *head_rows = false;
            }
        }
        "thead" if *inner_tables == 0 => *head_rows = false,
        "tr" if *inner_tables == 0 => b.end_row(),
        "td" | "th" if *inner_tables == 0 => {
            if name == "th" {
                b.look.bold = b.look.bold.saturating_sub(1);
            }
            b.end_cell();
        }
        _ if BLOCKS.contains(&name) => b.flush(),
        _ => {}
    }
}

/// A picture's size in ems, from its attributes in pixels: a face shrinks it
/// to fit the panel.
fn size(tag: &Tag) -> Option<(f32, f32)> {
    let px = |n: &str| {
        tag.attr(n)?
            .trim()
            .trim_end_matches("px")
            .parse::<f32>()
            .ok()
    };
    let (w, h) = (px("width")?, px("height")?);
    (w > 0.0 && h > 0.0).then(|| (w / 13.0, h / 13.0))
}

/// One tag: its name in lowercase, whether it ends an element or closes
/// itself, and its attributes.
struct Tag {
    name: String,
    end: bool,
    closed: bool,
    attrs: Vec<(String, String)>,
}

impl Tag {
    /// The tag `text` starts with, and its length. None when `<` is not
    /// followed by a name.
    fn parse(text: &str) -> Option<(Tag, usize)> {
        let bytes = text.as_bytes();
        let mut i = 1;
        let end = bytes.get(i) == Some(&b'/');
        if end {
            i += 1;
        }
        let start = i;
        while i < bytes.len()
            && (bytes[i].is_ascii_alphanumeric() || matches!(bytes[i], b'-' | b':'))
        {
            i += 1;
        }
        if i == start || !bytes[start].is_ascii_alphabetic() {
            return None;
        }
        let name = text[start..i].to_ascii_lowercase();
        // The name's namespace prefix, as an e-book's XHTML may write it.
        let name = name.rsplit(':').next().unwrap_or(&name).to_owned();
        let mut attrs = Vec::new();
        let mut closed = false;
        loop {
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            match bytes.get(i) {
                None => {
                    return Some((
                        Tag {
                            name,
                            end,
                            closed,
                            attrs,
                        },
                        i,
                    ));
                }
                Some(b'>') => {
                    return Some((
                        Tag {
                            name,
                            end,
                            closed,
                            attrs,
                        },
                        i + 1,
                    ));
                }
                Some(b'/') => {
                    closed = true;
                    i += 1;
                    continue;
                }
                _ => {}
            }
            let key_start = i;
            while i < bytes.len()
                && !bytes[i].is_ascii_whitespace()
                && !matches!(bytes[i], b'=' | b'>' | b'/')
            {
                i += 1;
            }
            let key = text[key_start..i].to_ascii_lowercase();
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            let mut value = String::new();
            if bytes.get(i) == Some(&b'=') {
                i += 1;
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                match bytes.get(i) {
                    Some(&q @ (b'"' | b'\'')) => {
                        let from = i + 1;
                        let to =
                            memchr::memchr(q, &bytes[from..]).map_or(bytes.len(), |n| from + n);
                        value = text[from..to].to_owned();
                        i = (to + 1).min(bytes.len());
                    }
                    _ => {
                        let from = i;
                        while i < bytes.len() && !bytes[i].is_ascii_whitespace() && bytes[i] != b'>'
                        {
                            i += 1;
                        }
                        value = text[from..i].to_owned();
                    }
                }
            }
            if key.is_empty() {
                i += 1;
            } else {
                attrs.push((key, value));
            }
        }
    }

    /// Hidden by the page: `hidden`, `aria-hidden`, or a style that says so.
    fn hidden(&self) -> bool {
        self.attr("hidden").is_some()
            || self
                .attr("aria-hidden")
                .is_some_and(|v| v.eq_ignore_ascii_case("true"))
            || self.attr("style").is_some_and(|s| {
                let s: String = s
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect::<String>()
                    .to_ascii_lowercase();
                s.contains("display:none") || s.contains("visibility:hidden")
            })
    }

    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }
}

/// How far past `text`'s start the element `name` ends: past its end tag,
/// or the whole of it when it never ends. Looked for in place, any case: a
/// lowercased copy of the rest of a 15 MB page for every script in it was
/// most of the time the page took.
fn skip_to_end(text: &str, name: &str) -> usize {
    let bytes = text.as_bytes();
    for at in memchr::memmem::find_iter(bytes, b"</") {
        let after = &bytes[at + 2..];
        if after.len() >= name.len()
            && after[..name.len()].eq_ignore_ascii_case(name.as_bytes())
            && after
                .get(name.len())
                .is_none_or(|b| *b == b'>' || b.is_ascii_whitespace())
        {
            let close = at + 2 + name.len();
            return memchr::memchr(b'>', &bytes[close..]).map_or(bytes.len(), |i| close + i + 1);
        }
    }
    bytes.len()
}

/// How far past `text`'s start the element `name` ends, counting the same
/// element opened inside it: a hidden `<div>` holds `<div>`s.
fn skip_element(text: &str, name: &str) -> usize {
    let bytes = text.as_bytes();
    let n = name.len();
    let mut depth = 1usize;
    for at in memchr::memchr_iter(b'<', bytes) {
        let after = &bytes[at + 1..];
        let (closing, rest) = match after.first() {
            Some(b'/') => (true, &after[1..]),
            _ => (false, after),
        };
        if rest.len() < n
            || !rest[..n].eq_ignore_ascii_case(name.as_bytes())
            || rest
                .get(n)
                .is_some_and(|b| !(b.is_ascii_whitespace() || matches!(b, b'>' | b'/')))
        {
            continue;
        }
        if closing {
            depth -= 1;
            if depth == 0 {
                return memchr::memchr(b'>', &bytes[at..]).map_or(bytes.len(), |i| at + i + 1);
            }
        } else {
            depth += 1;
        }
    }
    bytes.len()
}

/// Text with its tags taken out: a `<pre>` block's highlighting, say.
fn strip_tags(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut inside = false;
    for c in text.chars() {
        match c {
            '<' => inside = true,
            '>' if inside => inside = false,
            _ if !inside => out.push(c),
            _ => {}
        }
    }
    out
}

/// Entities replaced: by number, and by the names pages use.
pub(crate) fn decode(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains('&') {
        return text.into();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let end = rest[1..]
            .find(|c: char| c == ';' || !c.is_ascii_alphanumeric() && c != '#')
            .map(|i| i + 1);
        let (entity, len) = match end {
            Some(e) if rest.as_bytes()[e] == b';' => (&rest[1..e], e + 1),
            _ => ("", 0),
        };
        match named(entity) {
            Some(c) if len > 0 => {
                out.push(c);
                rest = &rest[len..];
            }
            _ => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out.into()
}

fn named(entity: &str) -> Option<char> {
    if let Some(n) = entity.strip_prefix('#') {
        let code = match n.strip_prefix(['x', 'X']) {
            Some(hex) => u32::from_str_radix(hex, 16).ok()?,
            None => n.parse().ok()?,
        };
        return char::from_u32(code).filter(|c| *c != '\0');
    }
    Some(match entity {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "hellip" => '…',
        "mdash" => '—',
        "ndash" => '–',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "laquo" => '«',
        "raquo" => '»',
        "bull" => '•',
        "middot" => '·',
        "deg" => '°',
        "euro" => '€',
        "pound" => '£',
        "yen" => '¥',
        "cent" => '¢',
        "times" => '×',
        "divide" => '÷',
        "para" => '¶',
        "sect" => '§',
        "shy" => '\u{ad}',
        "larr" => '←',
        "rarr" => '→',
        "uarr" => '↑',
        "darr" => '↓',
        "check" => '✓',
        _ => return None,
    })
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

    #[test]
    fn a_page_is_its_words_not_its_markup() {
        let html = "<!doctype html><html><head><title>t</title><style>p{}</style>\
                    <script>alert(1)</script></head><body><h1>Başlık &amp; alt</h1>\
                    <p>Bir   <b>kalın</b>\n<i>eğik</i> <a href=x>bağ</a> <code>kod</code><br>yeni\
                    <ul><li>bir<li>iki</ul><ol start=3><li>üç</ol>\
                    <pre><span>fn  main()</span>\n{}</pre><hr></body>";
        let flow = flow(html.as_bytes()).unwrap();
        let p = paras(&flow);
        let all: String = p.iter().map(|p| p.text()).collect::<Vec<_>>().join("|");
        assert!(
            !all.contains("alert") && !all.contains("p{}") && !all.contains('<'),
            "{all}"
        );
        assert_eq!((p[0].heading, p[0].text().as_str()), (1, "Başlık & alt"));
        assert_eq!(p[1].text(), "Bir kalın eğik bağ kod\nyeni");
        assert!(p[1].runs.iter().any(|r| r.bold && r.text == "kalın"));
        assert!(p[1].runs.iter().any(|r| r.code && r.text == "kod"));
        assert_eq!((p[2].mark.as_str(), p[3].mark.as_str()), ("•", "•"));
        assert_eq!((p[4].mark.as_str(), p[4].text().as_str()), ("3.", "üç"));
        assert_eq!(p[5].text(), "fn  main()\n{}");
        assert!(p[5].runs[0].code && p[5].shade.is_some());
        assert!(matches!(flow.blocks.last(), Some(Block::Break)));
    }

    #[test]
    fn a_table_and_a_table_inside_it() {
        let html = "<table><thead><tr><th>ad<th>n</thead><tr><td>a<td>1\
                    <tr><td><table><tr><td>iç</td></tr></table><td>2</table>";
        let flow = flow(html.as_bytes()).unwrap();
        let Some(Block::Table(t)) = flow.blocks.first() else {
            panic!("{:?}", flow.blocks);
        };
        assert_eq!(t.rows.len(), 3);
        assert!(t.rows[0].head);
        assert_eq!(t.rows[2].cells[0].paras[0].text(), "iç");
        assert_eq!(t.rows[2].cells[1].paras[0].text(), "2");
    }

    #[test]
    fn what_the_page_hides_is_not_shown() {
        let html = "<div style=\"display: none\"><div>yardım</div><p>gizli</p></div>\
                    <p hidden>yok</p><span aria-hidden=true>süs</span><p>görünen</p>";
        let flow = flow(html.as_bytes()).unwrap();
        let all: Vec<String> = paras(&flow).iter().map(|p| p.text()).collect();
        assert_eq!(all, ["görünen"]);
    }

    #[test]
    fn entities_by_name_and_number() {
        assert_eq!(
            decode("a&nbsp;b &#305;&#x131; &bogus; & x"),
            "a\u{a0}b ıı &bogus; & x"
        );
    }
}
