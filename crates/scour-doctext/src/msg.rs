//! An Outlook message (`.msg`): an OLE2 compound file of MAPI properties,
//! read with office_oxide's own reader. Its subject, sender, recipients and
//! date, its body — the HTML one drawn as a page where there is one — and
//! its attachments by name and size, as a message (`.eml`) is drawn; and its
//! words, for a content search.

use std::io::Cursor;

use office_oxide::cfb::CfbReader;
use scour_flow::Flow;

use crate::build::Builder;

/// A property's stream: `__substg1.0_` and its tag, as Unicode (`001F`), as
/// the code page's bytes (`001E`) or as binary (`0102`).
fn stream(
    cfb: &mut CfbReader<Cursor<&[u8]>>,
    folder: &str,
    id: &str,
    kind: &str,
) -> Option<Vec<u8>> {
    let name = format!("__substg1.0_{id}{kind}");
    let path = if folder.is_empty() {
        name
    } else {
        format!("{folder}/{name}")
    };
    cfb.open_stream_by_path(&path).ok()
}

/// A string property, in either of its two spellings.
fn string(cfb: &mut CfbReader<Cursor<&[u8]>>, folder: &str, id: &str) -> Option<String> {
    if let Some(utf16) = stream(cfb, folder, id, "001F") {
        let units: Vec<u16> = utf16
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        return Some(
            String::from_utf16_lossy(&units)
                .trim_end_matches('\0')
                .to_owned(),
        );
    }
    stream(cfb, folder, id, "001E").map(|bytes| {
        String::from_utf8(bytes.clone())
            .unwrap_or_else(|_| {
                bytes
                    .iter()
                    .map(|&b| scour_extract::cp1252_char(b))
                    .collect()
            })
            .trim_end_matches('\0')
            .to_owned()
    })
}

/// What a message is: the parts every face draws.
struct Message {
    subject: Option<String>,
    from: Option<String>,
    to: Option<String>,
    cc: Option<String>,
    date: Option<String>,
    body: Option<String>,
    html: Option<String>,
    attachments: Vec<(String, usize)>,
}

fn read(bytes: &[u8]) -> Result<Message, String> {
    let mut cfb = CfbReader::new(Cursor::new(bytes)).map_err(|e| e.to_string())?;
    let name = string(&mut cfb, "", "0C1A");
    let address = string(&mut cfb, "", "0C1F").or_else(|| string(&mut cfb, "", "5D01"));
    let from = match (name, address) {
        (Some(n), Some(a)) if !n.is_empty() && n != a => Some(format!("{n} <{a}>")),
        (n, a) => a.or(n),
    };
    // The HTML body is bytes in the message's code page; most write UTF-8.
    let html = stream(&mut cfb, "", "1013", "0102")
        .or_else(|| stream(&mut cfb, "", "1013", "001F"))
        .map(|b| String::from_utf8_lossy(&b).into_owned());
    let mut attachments = Vec::new();
    for n in 0..256 {
        let folder = format!("__attach_version1.0_#{n:08X}");
        if cfb.find_entry_by_path(&folder).is_none() {
            break;
        }
        let file = string(&mut cfb, &folder, "3707")
            .or_else(|| string(&mut cfb, &folder, "3704"))
            .unwrap_or_else(|| "(no name)".to_owned());
        let size = stream(&mut cfb, &folder, "3701", "0102").map_or(0, |d| d.len());
        attachments.push((file, size));
    }
    Ok(Message {
        subject: string(&mut cfb, "", "0037"),
        from,
        to: string(&mut cfb, "", "0E04"),
        cc: string(&mut cfb, "", "0E03"),
        date: date(&mut cfb),
        body: string(&mut cfb, "", "1000"),
        html,
        attachments,
    })
}

/// When it was sent, or else delivered: a `FILETIME` among the message's
/// fixed-size properties, as `2026-11-28 13:04` in UTC.
fn date(cfb: &mut CfbReader<Cursor<&[u8]>>) -> Option<String> {
    let props = cfb.open_stream("__properties_version1.0").ok()?;
    // A message's own properties stream opens with 32 bytes of counts.
    let entries = props.get(32..)?;
    for wanted in [0x0039_0040u32, 0x0E06_0040] {
        for e in entries.as_chunks::<16>().0 {
            if u32::from_le_bytes([e[0], e[1], e[2], e[3]]) == wanted {
                let ticks = u64::from_le_bytes(e[8..16].try_into().ok()?);
                let secs = (ticks / 10_000_000).checked_sub(11_644_473_600)?;
                return Some(crate::when(secs as i64));
            }
        }
    }
    None
}

/// The message in `bytes`, drawn.
pub fn flow(bytes: &[u8]) -> Result<Flow, String> {
    let m = read(bytes)?;
    let mut b = Builder::default();
    if let Some(s) = m.subject.as_deref().filter(|s| !s.trim().is_empty()) {
        b.heading(2);
        b.words(s.trim(), false);
        b.end_heading();
    }
    for (name, value) in [
        ("From", &m.from),
        ("To", &m.to),
        ("Cc", &m.cc),
        ("Date", &m.date),
    ] {
        if let Some(v) = value.as_deref().filter(|v| !v.trim().is_empty()) {
            crate::mail::field(&mut b, name, v.trim());
        }
    }
    b.rule();
    match (&m.html, &m.body) {
        (Some(html), _) => crate::html::read(html, &mut b, &mut |_| None),
        (None, Some(body)) => crate::mail::plain(&mut b, body),
        (None, None) => {}
    }
    crate::mail::attached(&mut b, m.attachments.iter().map(|(n, s)| (n.as_str(), *s)));
    Ok(b.finish())
}

/// The message's words, for a content search: subject, people, body.
pub fn text(bytes: &[u8]) -> Result<String, String> {
    let m = read(bytes)?;
    let mut out = String::new();
    for p in [&m.subject, &m.from, &m.to, &m.cc, &m.body]
        .into_iter()
        .flatten()
    {
        out.push_str(p);
        out.push('\n');
    }
    if m.body.is_none()
        && let Some(html) = &m.html
    {
        let mut b = Builder::default();
        crate::html::read(html, &mut b, &mut |_| None);
        for block in b.finish().blocks {
            if let scour_flow::Block::Para(p) = block {
                out.push_str(&p.text());
                out.push('\n');
            }
        }
    }
    for (name, _) in &m.attachments {
        out.push_str(name);
        out.push('\n');
    }
    Ok(out)
}
