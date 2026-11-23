//! SIMD-accelerated text extraction for UYAP `.udf` (`<![CDATA[...]]>`) and
//! XML-based Office / OpenDocument / EPUB streams.

use memchr::{memchr, memmem, memrchr_iter};

/// Zero-copy slice of a UYAP `.udf` `<content><![CDATA[...]]></content>` block.
pub(crate) fn udf_cdata(xml: &[u8]) -> Option<&[u8]> {
    let head = &xml[..xml.len().min(1024)];
    let start = memmem::find(head, b"<![CDATA[")? + 9;
    let body = &xml[start..];
    for gt in memrchr_iter(b'>', body) {
        if gt >= 2 && body[gt - 2..gt] == *b"]]" {
            return Some(&body[..gt - 2]);
        }
    }
    None
}

/// Strip XML tags into `out` (reusing its capacity) up to `cap` bytes, joining
/// split inline runs (`<w:t>...</w:t>`) and turning block tags into newlines.
pub(crate) fn strip_xml(xml: &[u8], out: &mut Vec<u8>, cap: usize) {
    out.clear();
    let mut rest = xml;
    while !rest.is_empty() && out.len() < cap {
        let Some(lt) = memchr(b'<', rest) else {
            append_text(rest, out, cap);
            break;
        };
        if lt > 0 {
            append_text(&rest[..lt], out, cap);
            if out.len() >= cap {
                break;
            }
        }
        let after_lt = &rest[lt + 1..];
        if after_lt.starts_with(b"![CDATA[") {
            let cdata = &after_lt[8..];
            match memmem::find(cdata, b"]]>") {
                Some(end) => {
                    let take = end.min(cap.saturating_sub(out.len()));
                    out.extend_from_slice(&cdata[..take]);
                    rest = &cdata[end + 3..];
                    continue;
                }
                None => {
                    let take = cdata.len().min(cap.saturating_sub(out.len()));
                    out.extend_from_slice(&cdata[..take]);
                    break;
                }
            }
        }
        let Some(gt) = memchr(b'>', after_lt) else {
            break;
        };
        let tag = &after_lt[..gt];
        match tag_sep(tag) {
            Sep::Newline => {
                if !out.is_empty() && !out.ends_with(b"\n") {
                    out.push(b'\n');
                }
            }
            Sep::Tab => {
                if !out.is_empty() && !matches!(out.last(), Some(b'\n' | b'\t' | b' ')) {
                    out.push(b'\t');
                }
            }
            Sep::None => {}
        }
        rest = &after_lt[gt + 1..];
    }
}

#[derive(Clone, Copy)]
enum Sep {
    None,
    Newline,
    Tab,
}

#[inline]
fn tag_sep(tag: &[u8]) -> Sep {
    if tag.is_empty() || tag[0] == b'?' || tag[0] == b'!' {
        return Sep::None;
    }
    let closing = tag[0] == b'/';
    let self_closing = !closing && tag.ends_with(b"/");
    let body = if closing { &tag[1..] } else { tag };
    let end = body
        .iter()
        .position(|&b| matches!(b, b' ' | b'\t' | b'\r' | b'\n' | b'/'))
        .unwrap_or(body.len());
    let name = &body[..end];

    if closing {
        return match name {
            b"w:p" | b"w:tr" | b"a:p" | b"text:p" | b"text:h" | b"table:table-row" | b"row"
            | b"si" | b"p" | b"div" | b"li" | b"tr" | b"h1" | b"h2" | b"h3" | b"h4" | b"h5"
            | b"h6" => Sep::Newline,
            b"w:tc" | b"table:table-cell" | b"c" | b"td" | b"th" => Sep::Tab,
            _ => Sep::None,
        };
    }
    match name {
        b"w:br" | b"a:br" | b"text:line-break" | b"br" => Sep::Newline,
        b"w:tab" | b"text:tab" => Sep::Tab,
        b"w:p" | b"a:p" | b"text:p" if self_closing => Sep::Newline,
        _ => Sep::None,
    }
}

#[inline]
fn append_text(span: &[u8], out: &mut Vec<u8>, cap: usize) {
    let rem = cap.saturating_sub(out.len());
    let span = &span[..span.len().min(rem)];
    if memchr(b'&', span).is_none() {
        out.extend_from_slice(span);
        return;
    }
    let mut rest = span;
    while !rest.is_empty() && out.len() < cap {
        let Some(amp) = memchr(b'&', rest) else {
            let take = rest.len().min(cap.saturating_sub(out.len()));
            out.extend_from_slice(&rest[..take]);
            break;
        };
        if amp > 0 {
            let take = amp.min(cap.saturating_sub(out.len()));
            out.extend_from_slice(&rest[..take]);
        }
        let after = &rest[amp + 1..];
        let Some(semi) = memchr(b';', &after[..after.len().min(12)]) else {
            out.push(b'&');
            rest = after;
            continue;
        };
        let ent = &after[..semi];
        match ent {
            b"amp" => out.push(b'&'),
            b"lt" => out.push(b'<'),
            b"gt" => out.push(b'>'),
            b"quot" => out.push(b'"'),
            b"apos" => out.push(b'\''),
            b"nbsp" => out.push(b' '),
            _ if ent.starts_with(b"#x") || ent.starts_with(b"#X") => {
                if let Ok(s) = std::str::from_utf8(&ent[2..])
                    && let Ok(code) = u32::from_str_radix(s, 16)
                    && let Some(ch) = char::from_u32(code)
                {
                    let mut b = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
                }
            }
            _ if ent.starts_with(b"#") => {
                if let Ok(s) = std::str::from_utf8(&ent[1..])
                    && let Ok(code) = s.parse::<u32>()
                    && let Some(ch) = char::from_u32(code)
                {
                    let mut b = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut b).as_bytes());
                }
            }
            _ => {
                out.push(b'&');
                rest = after;
                continue;
            }
        }
        rest = &after[semi + 1..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn udf_cdata_extracts_zero_copy_slice() {
        let sample = b"<?xml version=\"1.0\"?><template><content><![CDATA[ADANA MAHKEMES\xC4\xB0\nDAVACI: Ali]]></content><properties><p/></properties></template>";
        let got = udf_cdata(sample).expect("cdata");
        assert_eq!(got, "ADANA MAHKEMESİ\nDAVACI: Ali".as_bytes());
    }

    #[test]
    fn strip_xml_joins_word_runs_and_breaks_paragraphs() {
        let docx = b"<w:body><w:p><w:r><w:t>Mahke</w:t></w:r><w:r><w:t>mesi</w:t></w:r></w:p><w:p><w:r><w:t>Karar &amp; Sonu&#231;</w:t></w:r></w:p></w:body>";
        let mut out = Vec::new();
        strip_xml(docx, &mut out, 4096);
        assert_eq!(std::str::from_utf8(&out).unwrap(), "Mahkemesi\nKarar & Sonuç\n");
    }
}
