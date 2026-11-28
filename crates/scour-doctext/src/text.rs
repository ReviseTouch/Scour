//! A document's text, by the library its format needs.

use std::io::Cursor;

use crate::format::Format;

/// What a document gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Text {
    /// Its words, at most `cap` bytes of them.
    Words(String),
    /// It opened, and holds no text: a scan without a text layer.
    None,
    /// It would not open: the library's own words, for a log.
    Failed(String),
}

/// The text of `bytes`, read as `format`, at most `cap` bytes.
pub fn text(bytes: &[u8], format: Format, cap: usize) -> Text {
    let got = match format {
        Format::Pdf => pdf(bytes, cap),
        Format::Doc | Format::Xls | Format::Ppt => office(bytes, format),
        Format::Mail => mail(bytes, cap),
        Format::Msg => crate::msg::text(bytes),
    };
    match got {
        Ok(mut words) => {
            cut(&mut words, cap);
            if words.chars().any(char::is_alphanumeric) {
                Text::Words(words)
            } else {
                Text::None
            }
        }
        Err(why) => Text::Failed(why),
    }
}

fn pdf(bytes: &[u8], cap: usize) -> Result<String, String> {
    let doc = pdf_oxide::PdfDocument::from_bytes(bytes.to_vec()).map_err(|e| e.to_string())?;
    if doc.is_encrypted() {
        return Err("encrypted".into());
    }
    let pages = doc.page_count().map_err(|e| e.to_string())?;
    let mut out = String::new();
    for page in 0..pages {
        out.push_str(&doc.extract_text(page).map_err(|e| e.to_string())?);
        out.push('\n');
        if out.len() >= cap {
            break;
        }
    }
    Ok(out)
}

fn office(bytes: &[u8], format: Format) -> Result<String, String> {
    let kind = match format {
        Format::Doc => office_oxide::DocumentFormat::Doc,
        Format::Xls => office_oxide::DocumentFormat::Xls,
        _ => office_oxide::DocumentFormat::Ppt,
    };
    let doc = office_oxide::Document::from_reader(Cursor::new(bytes.to_vec()), kind)
        .map_err(|e| e.to_string())?;
    Ok(doc.plain_text())
}

/// Its subject and every body part as text; attachments are files of their own.
fn mail(bytes: &[u8], cap: usize) -> Result<String, String> {
    let message = mail_parser::MessageParser::default()
        .parse(bytes)
        .ok_or("not a message")?;
    let mut out = String::new();
    if let Some(subject) = message.subject() {
        out.push_str(subject);
        out.push('\n');
    }
    for part in 0..message.text_body_count().max(message.html_body_count()) {
        if let Some(body) = message.body_text(part) {
            out.push_str(&body);
            out.push('\n');
        }
        if out.len() >= cap {
            break;
        }
    }
    Ok(out)
}

/// At most `cap` bytes, cut at a character's edge.
fn cut(text: &mut String, cap: usize) {
    if text.len() > cap {
        let mut at = cap;
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        text.truncate(at);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_message_gives_its_subject_and_body() {
        let eml = "From: a@b\r\nSubject: =?UTF-8?B?xLBjcmEgZGFpcmVzaQ==?=\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nDosya no 2024/1\r\n";
        match text(eml.as_bytes(), Format::Mail, 1 << 20) {
            Text::Words(w) => {
                assert!(w.contains("İcra dairesi"), "{w}");
                assert!(w.contains("Dosya no 2024/1"), "{w}");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn what_will_not_open_says_so_and_does_not_panic() {
        assert!(matches!(
            text(b"%PDF-1.7 broken", Format::Pdf, 100),
            Text::Failed(_)
        ));
        let ole = b"\xD0\xCF\x11\xE0\xA1\xB1\x1A\xE1 not really";
        assert!(matches!(text(ole, Format::Doc, 100), Text::Failed(_)));
    }

    #[test]
    fn text_is_cut_at_a_character() {
        let mut s = "şşş".to_string();
        cut(&mut s, 3);
        assert_eq!(s, "ş");
    }
}
