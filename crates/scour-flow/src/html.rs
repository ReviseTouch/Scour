//! A flow as a page of its own, for a browser to set: every word escaped, every
//! colour a number, every picture one of [`PICTURE_TYPES`] as a `data:` URL.
//! It runs no script and loads nothing, and is served under a policy that
//! says so.

use std::fmt::Write as _;

use crate::{Align, Block, Flow, PICTURE_TYPES, Para, Run, Table};

/// The page: white paper, dark ink, the body at 13 px.
pub fn html(flow: &Flow) -> String {
    let mut out = String::with_capacity(16 * 1024);
    out.push_str(HEAD);
    for block in &flow.blocks {
        match block {
            Block::Para(p) => para(p, &mut out),
            Block::Table(t) => table(t, &mut out),
            Block::Picture(p) => {
                if PICTURE_TYPES.contains(&p.mime.as_str()) && crate::b64::is_base64(&p.data) {
                    let _ = writeln!(
                        out,
                        "<img alt=\"\" style=\"width:{:.2}em\" src=\"data:{};base64,{}\">",
                        p.width, p.mime, p.data
                    );
                }
            }
            Block::Break => out.push_str("<hr>\n"),
        }
    }
    if flow.cut {
        out.push_str("<p class=cut>…</p>\n");
    }
    out.push_str("</body></html>\n");
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
table{border-collapse:collapse;width:100%;table-layout:fixed;margin:.4em 0}\
td{vertical-align:top;padding:.15em .35em}\
table.r td{border:1px solid #9aa0a6}\
tr.hd td{font-weight:600}\
img{display:block;max-width:100%;height:auto;margin:.4em 0}\
hr{border:0;border-top:1px dashed #b8bec5;margin:1em 0}\
.cut{color:#6e7781}\
</style></head><body>\n";

fn para(p: &Para, out: &mut String) {
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
        run(r, out);
    }
    out.push_str("</p>\n");
}

fn run(r: &Run, out: &mut String) {
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
        out.push_str(&escape(line));
    }
    out.push_str(&close);
}

fn table(t: &Table, out: &mut String) {
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
                para(p, out);
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
