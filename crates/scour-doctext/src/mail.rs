//! A message (`.eml`) as a flow: its subject as the heading, who sent it to
//! whom and when, its body — drawn as a page is when it is HTML, with the
//! pictures it carries — and the files attached to it, by name and size.
//! The header names are a message's own (`From`, `To`), in every language.

use mail_parser::{Address, MessageParser, MimeHeaders};
use scour_flow::Flow;

use crate::build::Builder;

/// The message in `bytes`.
pub fn flow(bytes: &[u8]) -> Result<Flow, String> {
    let message = MessageParser::default()
        .parse(bytes)
        .ok_or("not a message")?;
    let mut b = Builder::default();
    if let Some(subject) = message.subject().filter(|s| !s.trim().is_empty()) {
        b.heading(2);
        b.words(subject.trim(), false);
        b.end_heading();
    }
    for (name, who) in [
        ("From", message.from()),
        ("To", message.to()),
        ("Cc", message.cc()),
    ] {
        if let Some(who) = who.map(people).filter(|w| !w.is_empty()) {
            field(&mut b, name, &who);
        }
    }
    if let Some(d) = message.date() {
        let when = format!(
            "{}-{:02}-{:02} {:02}:{:02}",
            d.year, d.month, d.day, d.hour, d.minute
        );
        field(&mut b, "Date", &when);
    }
    b.rule();
    // The body as written: HTML where there is a part of it, else its text.
    let html = message
        .html_part(0)
        .filter(|p| p.is_text_html())
        .and_then(|p| p.text_contents());
    if let Some(html) = html {
        crate::html::read(html, &mut b, &mut |src| {
            let id = src.strip_prefix("cid:")?;
            message
                .parts
                .iter()
                .find(|p| {
                    p.content_id()
                        .is_some_and(|c| c.trim_matches(['<', '>']) == id)
                })
                .map(|p| p.contents().to_vec())
        });
    } else {
        for part in 0..message.text_body_count() {
            if let Some(text) = message.body_text(part) {
                plain(&mut b, &text);
            }
        }
    }
    let attached: Vec<String> = message
        .attachments()
        .filter(|a| !a.is_message() || a.attachment_name().is_some())
        .map(|a| {
            format!(
                "📎 {} — {}",
                a.attachment_name().unwrap_or("(no name)"),
                size(a.len())
            )
        })
        .collect();
    if !attached.is_empty() {
        b.rule();
        b.list(None);
        for a in attached {
            b.item();
            b.words(&a, false);
        }
        b.end_list();
    }
    Ok(b.finish())
}

/// Each person as their name and address: `Ada <ada@example.org>`.
fn people(who: &Address<'_>) -> String {
    who.iter()
        .map(|a| match (a.name(), a.address()) {
            (Some(n), Some(e)) if !n.trim().is_empty() => format!("{} <{e}>", n.trim()),
            (_, Some(e)) => e.to_owned(),
            (Some(n), None) => n.trim().to_owned(),
            (None, None) => String::new(),
        })
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(", ")
}

/// One header line: its name bold, then what it says.
fn field(b: &mut Builder, name: &str, value: &str) {
    b.flush();
    b.look.bold += 1;
    b.words(&format!("{name}: "), false);
    b.look.bold -= 1;
    b.words(value, false);
    b.flush();
}

/// A text body: a paragraph for each run of lines, its lines kept.
fn plain(b: &mut Builder, text: &str) {
    for block in text.replace("\r\n", "\n").split("\n\n") {
        let block = block.trim_matches('\n');
        if block.trim().is_empty() {
            continue;
        }
        b.flush();
        b.words(block, false);
        b.flush();
        if b.full() {
            return;
        }
    }
}

fn size(bytes: usize) -> String {
    match bytes {
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / f64::from(1 << 20)),
        n if n >= 1 << 10 => format!("{:.0} KB", n as f64 / 1024.0),
        n => format!("{n} B"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scour_flow::Block;

    fn words(flow: &Flow) -> Vec<String> {
        flow.blocks
            .iter()
            .filter_map(|b| match b {
                Block::Para(p) => Some(p.text()),
                Block::Break => Some("---".into()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn subject_people_body_and_what_is_attached() {
        let eml = "From: Ada Lovelace <ada@example.org>\r\nTo: bob@example.org\r\n\
                   Subject: =?UTF-8?Q?Toplant=C4=B1?=\r\nDate: Fri, 28 Nov 2026 13:04:00 +0300\r\n\
                   MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b\"\r\n\r\n\
                   --b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nMerhaba,\r\nyarın.\r\n\r\nAda\r\n\
                   --b\r\nContent-Type: application/pdf; name=\"rapor.pdf\"\r\n\
                   Content-Disposition: attachment; filename=\"rapor.pdf\"\r\n\
                   Content-Transfer-Encoding: base64\r\n\r\nJVBERi0xLjQ=\r\n--b--\r\n";
        let flow = flow(eml.as_bytes()).unwrap();
        assert_eq!(
            words(&flow),
            [
                "Toplantı",
                "From: Ada Lovelace <ada@example.org>",
                "To: bob@example.org",
                "Date: 2026-11-28 13:04",
                "---",
                "Merhaba,\nyarın.",
                "Ada",
                "---",
                "📎 rapor.pdf — 8 B",
            ]
        );
    }

    #[test]
    fn an_html_body_is_drawn_as_a_page() {
        let eml = "Subject: x\r\nContent-Type: text/html; charset=utf-8\r\n\r\n\
                   <html><body><p>Bir <b>kalın</b></p><script>kötü()</script></body></html>";
        let flow = flow(eml.as_bytes()).unwrap();
        let w = words(&flow);
        assert_eq!(w.last().map(String::as_str), Some("Bir kalın"));
        assert!(!w.iter().any(|w| w.contains("kötü")));
    }
}
