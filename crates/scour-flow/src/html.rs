//! A flow as a page of its own, for a browser to set: every word escaped, every
//! colour a number, every picture one of [`PICTURE_TYPES`] as a `data:` URL.
//! It runs no script and loads nothing, and is served under a policy that
//! says so.

use std::fmt::Write as _;

use crate::{Align, Block, Flow, Locale, Marks, PICTURE_TYPES, Para, Picture, Run, Sheet, Table};

/// The page: white paper, dark ink, the body at 13 px; a book's sheets as
/// grids, one shown at a time. `locale` punctuates a sheet's numbers.
pub fn html(flow: &Flow, locale: &Locale, marks: &Marks) -> String {
    let mut out = String::with_capacity(16 * 1024);
    out.push_str(HEAD);
    let sheets: Vec<&Sheet> = flow.sheets().collect();
    if !sheets.is_empty() {
        out.push_str("<body class=book>\n");
        book(&sheets, locale, marks, &mut out);
        out.push_str("</body></html>\n");
        return first_mark(out);
    }
    let deck = flow.blocks.iter().any(|b| matches!(b, Block::Slide(_)));
    out.push_str(if deck {
        "<body class=deck>\n"
    } else {
        "<body>\n"
    });
    let mut slides = 0;
    for block in &flow.blocks {
        if let Block::Slide(_) = block {
            slides += 1;
        }
        blocks(block, slides, marks, &mut out);
    }
    if flow.cut {
        out.push_str("<p class=cut>…</p>\n");
    }
    out.push_str("</body></html>\n");
    first_mark(out)
}

/// The first find is where the address's `#f` takes the reader.
fn first_mark(page: String) -> String {
    page.replacen("<mark>", "<mark id=f>", 1)
}

/// `text` escaped, each find in it in a `<mark>`.
fn marked(text: &str, m: &Marks) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut at = 0;
    for (a, b) in m.find(text) {
        out.push_str(&escape(&text[at..a]));
        out.push_str("<mark>");
        out.push_str(&escape(&text[a..b]));
        out.push_str("</mark>");
        at = b;
    }
    out.push_str(&escape(&text[at..]));
    out
}

const HEAD: &str = "<!doctype html><html><head><meta charset=utf-8>\
<meta name=viewport content=\"width=device-width\"><style>\
html{background:#fff;color-scheme:light}\
body{margin:0;padding:14px 16px 24px;font:13px/1.45 system-ui,'Noto Sans',sans-serif;\
color:#1f2328;background:#fff;overflow-wrap:break-word}\
p{margin:0;min-height:1.45em}\
.h{font-weight:600}\
.mk{display:inline-block;width:1.6em;margin-left:-1.6em}\
table{border-collapse:collapse;width:100%;table-layout:fixed;margin:.4em 0;font-size:.92em}\
td{vertical-align:top;padding:.15em .3em}\
table.r td{border:1px solid #9aa0a6}\
tr.hd td{font-weight:600}\
img{display:block;max-width:100%;height:auto;margin:.4em 0}\
hr{border:0;border-top:1px dashed #b8bec5;margin:1em 0}\
.cut{color:#6e7781}\
body.book{padding:0;overflow-wrap:normal}\
.sheet{display:none}.sheet:target,.sheet:has(:target){display:block}\
.book:not(:has(:target))>.sheet:first-child{display:block}\
code{font:.92em/1.4 ui-monospace,'Noto Sans Mono',monospace}\
mark{background:#fff0a6;color:#1f2328;border-radius:2px;padding:0 1px;scroll-margin:5em 0 0}\
.tabs{display:flex;gap:2px;padding:6px 8px 0;border-bottom:1px solid #d0d7de;background:#f6f8fa;\
position:sticky;left:0;width:max-content;min-width:100%;box-sizing:border-box}\
.tabs a{padding:3px 10px;border:1px solid #d0d7de;border-bottom:0;border-radius:4px 4px 0 0;\
color:#57606a;text-decoration:none;font-size:12px;background:#eaeef2;white-space:nowrap}\
.tabs a.on{background:#fff;color:#1f2328;font-weight:600;margin-bottom:-1px}\
table.grid{width:auto;margin:0;font-size:12px;line-height:1.4}\
.grid td{border:1px solid #e1e4e8;padding:1px 4px;white-space:nowrap;overflow:hidden;\
vertical-align:middle;height:1.45em}\
.grid td.o{overflow:visible}\
.grid thead td{position:sticky;top:0;background:#fff;z-index:1;box-shadow:0 1px 0 #d0d7de}\
.sheet>.cut{padding:6px 8px}\
body.deck{padding:10px 12px 24px;background:#eef1f4}html:has(body.deck){background:#eef1f4}\
.slide{position:relative;background:#fff;border:1px solid #d0d7de;border-radius:8px;\
padding:14px 16px 16px;margin:0 0 10px;min-height:calc((100vw - 24px) * .42);box-sizing:border-box}\
.slide .no{position:absolute;top:6px;right:10px;font-size:11px;opacity:.6}\
.slide img,body>img{max-height:260px;object-fit:contain;object-position:left}\
</style></head>";

fn blocks(block: &Block, number: usize, m: &Marks, out: &mut String) {
    match block {
        Block::Para(p) => para(p, m, out),
        Block::Table(t) => table(t, m, out),
        Block::Picture(p) => picture(p, out),
        Block::Break => out.push_str("<hr>\n"),
        Block::Sheet(_) => {}
        Block::Slide(s) => {
            let mut css = String::new();
            if let Some(c) = s.background {
                let _ = write!(css, "background:{};", hex(c));
            }
            if s.dark() {
                css.push_str("color:#f6f8fa;border-color:transparent;");
            }
            let _ = write!(
                out,
                "<section class=slide style=\"{css}\"><span class=no>{number}</span>"
            );
            for b in &s.blocks {
                blocks(b, number, m, out);
            }
            out.push_str("</section>\n");
        }
    }
}

fn picture(p: &Picture, out: &mut String) {
    if PICTURE_TYPES.contains(&p.mime.as_str()) && crate::b64::is_base64(&p.data) {
        let _ = writeln!(
            out,
            "<img alt=\"\" style=\"width:{:.2}em\" src=\"data:{};base64,{}\">",
            p.width, p.mime, p.data
        );
    }
}

/// Each sheet a section, its tabs inside it with its own marked: shown by the
/// address's fragment, the first when there is none — no script needed.
fn book(sheets: &[&Sheet], locale: &Locale, m: &Marks, out: &mut String) {
    for (i, sheet) in sheets.iter().enumerate() {
        let _ = write!(out, "<section class=sheet id=s{i}>");
        if sheets.len() > 1 {
            out.push_str("<nav class=tabs>");
            for (j, other) in sheets.iter().enumerate() {
                let on = if i == j { " class=on" } else { "" };
                let _ = write!(out, "<a href=\"#s{j}\"{on}>{}</a>", escape(&other.name));
            }
            out.push_str("</nav>");
        }
        grid(sheet, locale, m, out);
        if sheet.cut {
            out.push_str("<p class=cut>…</p>");
        }
        out.push_str("</section>\n");
    }
}

fn grid(sheet: &Sheet, locale: &Locale, m: &Marks, out: &mut String) {
    let total: f32 = sheet.widths.iter().sum();
    let _ = write!(
        out,
        "<table class=grid style=\"width:{total:.2}em\"><colgroup>"
    );
    for w in &sheet.widths {
        let _ = write!(out, "<col style=\"width:{w:.2}em\">");
    }
    out.push_str("</colgroup>\n");
    for (n, row) in sheet.rows.iter().enumerate() {
        let head = n == 0 && sheet.frozen;
        if head {
            out.push_str("<thead>");
        } else if n == usize::from(sheet.frozen) {
            out.push_str("<tbody>");
        }
        out.push_str("<tr>");
        for (i, cell) in row.cells.iter().enumerate() {
            let spill = cell.spills() && row.cells.get(i + 1).is_none_or(|n| n.blank());
            let mut css = String::new();
            match cell.alignment() {
                Align::Left => {}
                Align::Center => css.push_str("text-align:center;"),
                Align::Right => css.push_str("text-align:right;"),
            }
            if cell.bold {
                css.push_str("font-weight:600;");
            }
            if cell.italic {
                css.push_str("font-style:italic;");
            }
            if let Some(c) = cell.color {
                let _ = write!(css, "color:{};", hex(c));
            }
            if let Some(c) = cell.shade {
                let _ = write!(css, "background:{};", hex(c));
            }
            out.push_str("<td");
            if cell.span > 1 {
                let _ = write!(out, " colspan={}", cell.span);
            }
            // One line a row, as the window draws it: a wrapping cell would
            // make every row a different height, and a preview a ragged one.
            if spill {
                out.push_str(" class=o");
            }
            if !css.is_empty() {
                let _ = write!(out, " style=\"{css}\"");
            }
            out.push('>');
            out.push_str(&marked(&cell.shown(locale), m));
            out.push_str("</td>");
        }
        out.push_str("</tr>");
        if head {
            out.push_str("</thead>");
        }
        out.push('\n');
    }
    if sheet.rows.len() > usize::from(sheet.frozen) {
        out.push_str("</tbody>");
    }
    out.push_str("</table>");
}

fn para(p: &Para, m: &Marks, out: &mut String) {
    let mut css = String::new();
    match p.align {
        Align::Left => {}
        Align::Center => css.push_str("text-align:center;"),
        Align::Right => css.push_str("text-align:right;"),
    }
    let indent = p.indent + if p.mark.is_empty() { 0.0 } else { 1.6 };
    if indent > 0.0 {
        let _ = write!(css, "padding-left:{indent:.2}em;");
    }
    if p.first != 0.0 && p.mark.is_empty() {
        let _ = write!(css, "text-indent:{:.2}em;", p.first);
    }
    if p.before > 0.0 {
        let _ = write!(css, "margin-top:{:.2}em;", p.before);
    }
    if p.after > 0.0 {
        let _ = write!(css, "margin-bottom:{:.2}em;", p.after);
    }
    if (p.size - 1.0).abs() > 0.01 {
        let _ = write!(css, "font-size:{:.3}em;", p.size);
    }
    if let Some(c) = p.shade {
        let _ = write!(css, "background:{};", hex(c));
    }
    let class = if p.heading > 0 { " class=h" } else { "" };
    if css.is_empty() {
        let _ = write!(out, "<p{class}>");
    } else {
        let _ = write!(out, "<p{class} style=\"{css}\">");
    }
    if !p.mark.is_empty() {
        let _ = write!(out, "<span class=mk>{}</span>", escape(&p.mark));
    }
    for r in &p.runs {
        run(r, m, out);
    }
    out.push_str("</p>\n");
}

fn run(r: &Run, m: &Marks, out: &mut String) {
    let mut css = String::new();
    if let Some(c) = r.color {
        let _ = write!(css, "color:{};", hex(c));
    }
    if let Some(c) = r.marker {
        let _ = write!(css, "background:{};", hex(c));
    }
    let mut open = String::new();
    let mut close = String::new();
    for (on, tag) in [
        (r.bold, "b"),
        (r.italic, "i"),
        (r.underline, "u"),
        (r.strike, "s"),
        (r.raise > 0, "sup"),
        (r.raise < 0, "sub"),
        (r.code, "code"),
    ] {
        if on {
            let _ = write!(open, "<{tag}>");
            close.insert_str(0, &format!("</{tag}>"));
        }
    }
    if !css.is_empty() {
        let _ = write!(open, "<span style=\"{css}\">");
        close.insert_str(0, "</span>");
    }
    out.push_str(&open);
    for (i, line) in r.text.split('\n').enumerate() {
        if i > 0 {
            out.push_str("<br>");
        }
        out.push_str(&marked(line, m));
    }
    out.push_str(&close);
}

fn table(t: &Table, m: &Marks, out: &mut String) {
    let _ = write!(
        out,
        "<table{}><colgroup>",
        if t.ruled { " class=r" } else { "" }
    );
    for w in &t.widths {
        let _ = write!(out, "<col style=\"width:{:.2}%\">", w * 100.0);
    }
    out.push_str("</colgroup>\n");
    for row in &t.rows {
        out.push_str(if row.head { "<tr class=hd>" } else { "<tr>" });
        for c in &row.cells {
            let _ = write!(out, "<td colspan={}", c.span.max(1));
            if let Some(s) = c.shade {
                let _ = write!(out, " style=\"background:{}\"", hex(s));
            }
            out.push('>');
            for p in &c.paras {
                para(p, m, out);
            }
            out.push_str("</td>");
        }
        out.push_str("</tr>\n");
    }
    out.push_str("</table>\n");
}

fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}
