//! Fast in-process text extraction from ZIP-based Office documents (`.docx`,
//! `.odt`, `.pptx`, `.xlsx`) so previews appear in milliseconds before or
//! without an external PDF converter.

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

const MAX_ARCHIVE: u64 = 64 * 1024 * 1024;
const MAX_ENTRY: usize = 8 * 1024 * 1024;
const TEXT_BUDGET: usize = 128 * 1024;

/// Extract readable plain text from `.docx`, `.odt`, `.pptx`, `.xlsx`, or `.udf`.
pub fn extract_text(path: &Path) -> Option<(String, bool)> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    if !matches!(ext.as_str(), "docx" | "odt" | "pptx" | "xlsx" | "udf") {
        return None;
    }
    let mut file = crate::open_file(path).ok()?;
    let len = file.metadata().ok()?.len();
    if !(22..=MAX_ARCHIVE).contains(&len) {
        return None;
    }
    let entries = read_zip_directory(&mut file, len)?;
    let mut out = String::new();
    let mut cut = false;

    match ext.as_str() {
        "docx" => {
            let xml = read_entry(
                &mut file,
                entries.iter().find(|e| e.name == "word/document.xml")?,
            )?;
            append_xml_text(
                &xml,
                &["w:p", "w:br", "w:tr"],
                &["w:tab"],
                &mut out,
                &mut cut,
            );
        }
        "odt" => {
            let xml = read_entry(&mut file, entries.iter().find(|e| e.name == "content.xml")?)?;
            append_xml_text(
                &xml,
                &["text:p", "text:h", "table:table-row"],
                &["text:tab"],
                &mut out,
                &mut cut,
            );
        }
        "udf" => {
            let xml = read_entry(&mut file, entries.iter().find(|e| e.name == "content.xml")?)?;
            if let Some(start) = xml.find("<![CDATA[")
                && let Some(end) = xml[start + 9..].rfind("]]>")
            {
                let body = &xml[start + 9..start + 9 + end];
                if body.len() > TEXT_BUDGET {
                    let mut at = TEXT_BUDGET;
                    while !body.is_char_boundary(at) {
                        at -= 1;
                    }
                    out.push_str(&body[..at]);
                    cut = true;
                } else {
                    out.push_str(body);
                }
            } else {
                append_xml_text(&xml, &["p", "content"], &[], &mut out, &mut cut);
            }
        }
        "pptx" => {
            let mut slides: Vec<&ZipEntry> = entries
                .iter()
                .filter(|e| e.name.starts_with("ppt/slides/slide") && e.name.ends_with(".xml"))
                .collect();
            slides.sort_by_key(|a| slide_num(&a.name));
            for (idx, slide) in slides.into_iter().enumerate() {
                if cut {
                    break;
                }
                if let Some(xml) = read_entry(&mut file, slide) {
                    if !out.is_empty() {
                        out.push_str("\n\n");
                    }
                    out.push_str(&format!("— {} —\n", idx + 1));
                    append_xml_text(&xml, &["a:p", "a:br"], &[], &mut out, &mut cut);
                }
            }
        }
        "xlsx" => {
            let shared = entries
                .iter()
                .find(|e| e.name == "xl/sharedStrings.xml")
                .and_then(|e| read_entry(&mut file, e))
                .map(|xml| collect_shared_strings(&xml))
                .unwrap_or_default();
            let mut sheets: Vec<&ZipEntry> = entries
                .iter()
                .filter(|e| e.name.starts_with("xl/worksheets/sheet") && e.name.ends_with(".xml"))
                .collect();
            sheets.sort_by(|a, b| a.name.cmp(&b.name));
            for sheet in sheets.into_iter().take(8) {
                if cut {
                    break;
                }
                if let Some(xml) = read_entry(&mut file, sheet) {
                    append_xlsx_sheet(&xml, &shared, &mut out, &mut cut);
                }
            }
        }
        _ => return None,
    }

    let trimmed = out.trim().to_owned();
    (!trimmed.is_empty()).then_some((trimmed, cut))
}

struct ZipEntry {
    name: String,
    method: u16,
    comp_size: u32,
    uncomp_size: u32,
    local_offset: u32,
}

fn read_zip_directory(file: &mut std::fs::File, len: u64) -> Option<Vec<ZipEntry>> {
    let tail_len = len.min(65_557) as usize;
    file.seek(SeekFrom::Start(len - tail_len as u64)).ok()?;
    let mut tail = vec![0u8; tail_len];
    file.read_exact(&mut tail).ok()?;

    let eocd = (0..=tail.len().checked_sub(22)?)
        .rev()
        .find(|&i| tail[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])?;
    let count = u16::from_le_bytes([tail[eocd + 10], tail[eocd + 11]]) as usize;
    let cd_size = u32::from_le_bytes(tail[eocd + 12..eocd + 16].try_into().ok()?) as usize;
    let cd_offset = u32::from_le_bytes(tail[eocd + 16..eocd + 20].try_into().ok()?) as u64;
    if cd_size > 4 * 1024 * 1024 || cd_offset + cd_size as u64 > len {
        return None;
    }

    file.seek(SeekFrom::Start(cd_offset)).ok()?;
    let mut cd = vec![0u8; cd_size];
    file.read_exact(&mut cd).ok()?;

    let mut out = Vec::with_capacity(count.min(1024));
    let mut at = 0usize;
    while at + 46 <= cd.len() && out.len() < count {
        if cd[at..at + 4] != [0x50, 0x4b, 0x01, 0x02] {
            break;
        }
        let method = u16::from_le_bytes([cd[at + 10], cd[at + 11]]);
        let comp_size = u32::from_le_bytes(cd[at + 20..at + 24].try_into().ok()?);
        let uncomp_size = u32::from_le_bytes(cd[at + 24..at + 28].try_into().ok()?);
        let nlen = u16::from_le_bytes([cd[at + 28], cd[at + 29]]) as usize;
        let elen = u16::from_le_bytes([cd[at + 30], cd[at + 31]]) as usize;
        let clen = u16::from_le_bytes([cd[at + 32], cd[at + 33]]) as usize;
        let local_offset = u32::from_le_bytes(cd[at + 42..at + 46].try_into().ok()?);
        let name_end = at.checked_add(46)?.checked_add(nlen)?;
        let next = name_end.checked_add(elen)?.checked_add(clen)?;
        if next > cd.len() {
            break;
        }
        if let Ok(name) = std::str::from_utf8(&cd[at + 46..name_end]) {
            out.push(ZipEntry {
                name: name.to_owned(),
                method,
                comp_size,
                uncomp_size,
                local_offset,
            });
        }
        at = next;
    }
    Some(out)
}

fn read_entry(file: &mut std::fs::File, entry: &ZipEntry) -> Option<String> {
    if entry.comp_size as usize > MAX_ENTRY || entry.uncomp_size as usize > MAX_ENTRY {
        return None;
    }
    file.seek(SeekFrom::Start(entry.local_offset as u64)).ok()?;
    let mut lhdr = [0u8; 30];
    file.read_exact(&mut lhdr).ok()?;
    if lhdr[0..4] != [0x50, 0x4b, 0x03, 0x04] {
        return None;
    }
    let nlen = u16::from_le_bytes([lhdr[26], lhdr[27]]) as u64;
    let elen = u16::from_le_bytes([lhdr[28], lhdr[29]]) as u64;
    file.seek(SeekFrom::Current((nlen + elen) as i64)).ok()?;

    let mut raw = vec![0u8; entry.comp_size as usize];
    file.read_exact(&mut raw).ok()?;
    let bytes = match entry.method {
        0 => raw,
        8 => miniz_oxide::inflate::decompress_to_vec_with_limit(&raw, MAX_ENTRY).ok()?,
        _ => return None,
    };
    String::from_utf8(bytes).ok()
}

fn append_xml_text(xml: &str, breaks: &[&str], tabs: &[&str], out: &mut String, cut: &mut bool) {
    let mut rest = xml;
    while !rest.is_empty() {
        if out.len() >= TEXT_BUDGET {
            *cut = true;
            break;
        }
        if let Some(lt) = rest.find('<') {
            if lt > 0 {
                decode_entities(&rest[..lt], out);
            }
            let Some(gt) = rest[lt..].find('>') else {
                break;
            };
            let tag = &rest[lt + 1..lt + gt];
            let clean = tag
                .trim_start_matches('/')
                .split_whitespace()
                .next()
                .unwrap_or("");
            let is_break = breaks.contains(&clean)
                && ((tag.starts_with('/') && !out.ends_with('\n'))
                    || (!tag.starts_with('/') && tag.ends_with('/')));
            if is_break {
                out.push('\n');
            } else if tabs.contains(&clean) {
                out.push('\t');
            }
            rest = &rest[lt + gt + 1..];
        } else {
            decode_entities(rest, out);
            break;
        }
    }
}

fn collect_shared_strings(xml: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = xml;
    while let Some(start) = rest.find("<si") {
        let after = &rest[start..];
        let Some(end) = after.find("</si>") else {
            break;
        };
        let mut cell = String::new();
        let mut dummy = false;
        append_xml_text(&after[..end], &[], &[], &mut cell, &mut dummy);
        out.push(cell);
        rest = &after[end + 5..];
    }
    out
}

fn append_xlsx_sheet(xml: &str, shared: &[String], out: &mut String, cut: &mut bool) {
    let mut rest = xml;
    while let Some(rstart) = rest.find("<row") {
        if out.len() >= TEXT_BUDGET {
            *cut = true;
            break;
        }
        let after = &rest[rstart..];
        let Some(rend) = after.find("</row>") else {
            break;
        };
        let row_xml = &after[..rend];
        let mut cols = Vec::new();
        let mut c_rest = row_xml;
        while let Some(cstart) = c_rest.find("<c ") {
            let cafter = &c_rest[cstart..];
            let Some(cend) = cafter.find("</c>") else {
                break;
            };
            let cell_xml = &cafter[..cend];
            let is_shared = cell_xml.contains("t=\"s\"");
            if let Some(vstart) = cell_xml.find("<v>")
                && let Some(vend) = cell_xml[vstart + 3..].find("</v>")
            {
                let raw = &cell_xml[vstart + 3..vstart + 3 + vend];
                let val = if is_shared {
                    raw.parse::<usize>()
                        .ok()
                        .and_then(|i| shared.get(i))
                        .map_or("", String::as_str)
                } else {
                    raw
                };
                if !val.is_empty() {
                    cols.push(val.to_owned());
                }
            }
            c_rest = &cafter[cend + 4..];
        }
        if !cols.is_empty() {
            out.push_str(&cols.join("\t"));
            out.push('\n');
        }
        rest = &after[rend + 6..];
    }
}

fn decode_entities(s: &str, out: &mut String) {
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        if let Some(semi) = after.find(';') {
            let ent = &after[..semi];
            match ent {
                "amp" => out.push('&'),
                "lt" => out.push('<'),
                "gt" => out.push('>'),
                "quot" => out.push('"'),
                "apos" => out.push('\''),
                _ if ent.starts_with("#x") => {
                    if let Some(c) = u32::from_str_radix(&ent[2..], 16)
                        .ok()
                        .and_then(char::from_u32)
                    {
                        out.push(c);
                    }
                }
                _ if ent.starts_with('#') => {
                    if let Some(c) = ent[1..].parse::<u32>().ok().and_then(char::from_u32) {
                        out.push(c);
                    }
                }
                _ => {
                    out.push('&');
                    out.push_str(ent);
                    out.push(';');
                }
            }
            rest = &after[semi + 1..];
        } else {
            out.push('&');
            rest = after;
            break;
        }
    }
    out.push_str(rest);
}

fn slide_num(name: &str) -> u32 {
    name.trim_start_matches("ppt/slides/slide")
        .trim_end_matches(".xml")
        .parse()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_paragraphs_and_entities_from_xml() {
        let xml = "<w:body><w:p><w:r><w:t>Merhaba &amp; Dünya</w:t></w:r></w:p><w:p><w:r><w:t>İkinci satır</w:t></w:r></w:p></w:body>";
        let mut out = String::new();
        let mut cut = false;
        append_xml_text(xml, &["w:p"], &["w:tab"], &mut out, &mut cut);
        assert_eq!(out.trim(), "Merhaba & Dünya\nİkinci satır");
        assert!(!cut);
    }
}
