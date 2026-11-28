//! An RTF document as a flow: its paragraphs with their bold, italics,
//! underlining and strikes, its tables as tables, in one pass. Read as the
//! content search reads RTF for its words (`scour_extract::rtf_text`): the
//! code page `\ansicpg` names, `\uN` as itself, and the groups that are not
//! text — fonts, colours, pictures, fields' instructions — passed over.

use scour_flow::Flow;

use crate::build::Builder;

/// Destinations whose contents are not the document's text.
const SKIPPED: &[&[u8]] = &[
    b"fonttbl",
    b"colortbl",
    b"stylesheet",
    b"info",
    b"pict",
    b"object",
    b"objdata",
    b"themedata",
    b"colorschememapping",
    b"datastore",
    b"latentstyles",
    b"listtable",
    b"listoverridetable",
    b"rsidtbl",
    b"generator",
    b"xmlnstbl",
    b"mmathPr",
    b"filetbl",
    b"revtbl",
    b"pgdsctbl",
    b"fldinst",
    b"bkmkstart",
    b"bkmkend",
    b"wgrffmtfilter",
    b"defchp",
    b"defpap",
    b"header",
    b"footer",
    b"headerl",
    b"headerr",
    b"footerl",
    b"footerr",
    b"pntext",
    b"listtext",
];

/// A group's state: what its words look like, and whether it is text at all.
#[derive(Clone, Copy, Default)]
struct Group {
    skip: bool,
    uc: u32,
    bold: bool,
    italic: bool,
    underline: bool,
    strike: bool,
}

/// The document in `bytes`.
pub fn flow(bytes: &[u8]) -> Result<Flow, String> {
    if !bytes.starts_with(b"{\\rtf") {
        return Err("not RTF".into());
    }
    let mut b = Builder::default();
    let mut decode = scour_extract::code_page(1252);
    let mut stack: Vec<Group> = Vec::with_capacity(32);
    let mut group = Group {
        uc: 1,
        ..Group::default()
    };
    // Fallback characters still to be passed over after a `\uN`.
    let mut owed = 0u32;
    let mut in_table = false;
    let mut cell_open = false;
    // A `\pard` in a table: the paragraph after it is out of the table unless
    // `\intbl` says it is in.
    let mut leaving = false;
    let mut text = String::new();
    let mut i = 0usize;
    // Words are gathered and set in the look they were read in, a run at a time.
    let flush = |b: &mut Builder, text: &mut String, g: &Group| {
        if text.is_empty() {
            return;
        }
        b.look.bold = u8::from(g.bold);
        b.look.italic = u8::from(g.italic);
        b.look.underline = u8::from(g.underline);
        b.look.strike = u8::from(g.strike);
        b.words(text, false);
        text.clear();
    };
    while i < bytes.len() && !b.full() {
        let c = bytes[i];
        i += 1;
        match c {
            b'{' => {
                flush(&mut b, &mut text, &group);
                stack.push(group);
                owed = 0;
            }
            b'}' => {
                flush(&mut b, &mut text, &group);
                group = stack.pop().unwrap_or(group);
                owed = 0;
            }
            b'\r' | b'\n' => {}
            b'\\' => {
                let Some(&next) = bytes.get(i) else { break };
                if !next.is_ascii_alphabetic() {
                    i += 1;
                    match next {
                        b'\'' => {
                            let byte = bytes.get(i..i + 2).and_then(hex_byte);
                            i += 2;
                            if owed > 0 {
                                owed -= 1;
                            } else if let (Some(byte), false) = (byte, group.skip) {
                                text.push(decode(byte));
                            }
                        }
                        b'*' => group.skip = true,
                        b'~' if !group.skip => text.push('\u{a0}'),
                        b'_' if !group.skip => text.push('-'),
                        b'\\' | b'{' | b'}' if !group.skip => text.push(next as char),
                        _ => {}
                    }
                    continue;
                }
                let (word, param, after) = control_word(bytes, i);
                i = after;
                if word == b"bin" {
                    i = i.saturating_add(param.unwrap_or(0).max(0) as usize);
                    continue;
                }
                if owed > 0 && !SKIPPED.contains(&word) {
                    owed -= 1;
                    continue;
                }
                let on = param != Some(0);
                // A change of look ends the run read in the old one.
                if matches!(
                    word,
                    b"b" | b"i"
                        | b"ul"
                        | b"ulnone"
                        | b"strike"
                        | b"plain"
                        | b"par"
                        | b"line"
                        | b"cell"
                        | b"row"
                        | b"intbl"
                        | b"pard"
                        | b"tab"
                ) {
                    flush(&mut b, &mut text, &group);
                }
                match word {
                    b"ansicpg" => decode = scour_extract::code_page(param.unwrap_or(1252) as u32),
                    b"uc" => group.uc = param.unwrap_or(1).max(0) as u32,
                    b"u" => {
                        let n = param.unwrap_or(0);
                        let n = if n < 0 { n + 65_536 } else { n } as u32;
                        if !group.skip {
                            text.push(char::from_u32(n).unwrap_or('\u{FFFD}'));
                        }
                        owed = group.uc;
                    }
                    _ if SKIPPED.contains(&word) => group.skip = true,
                    _ if group.skip => {}
                    b"b" => group.bold = on,
                    b"i" => group.italic = on,
                    b"ul" => group.underline = on,
                    b"ulnone" => group.underline = false,
                    b"strike" | b"striked" => group.strike = on,
                    b"plain" => {
                        group.bold = false;
                        group.italic = false;
                        group.underline = false;
                        group.strike = false;
                    }
                    b"par" | b"sect" | b"page" => {
                        if in_table {
                            b.words("\n", false);
                        } else {
                            b.flush();
                        }
                    }
                    b"line" => b.words("\n", false),
                    b"tab" => b.words("\t", false),
                    b"intbl" => {
                        leaving = false;
                        if !in_table {
                            b.table(Vec::new());
                            b.row(false);
                            b.cell();
                            in_table = true;
                            cell_open = true;
                        }
                    }
                    b"cell" if in_table => {
                        b.end_cell();
                        cell_open = false;
                    }
                    b"row" if in_table => b.end_row(),
                    b"pard" if in_table => leaving = true,
                    b"emdash" => text.push('—'),
                    b"endash" => text.push('–'),
                    b"bullet" => text.push('•'),
                    b"lquote" => text.push('‘'),
                    b"rquote" => text.push('’'),
                    b"ldblquote" => text.push('“'),
                    b"rdblquote" => text.push('”'),
                    _ => {}
                }
            }
            _ if owed > 0 => owed -= 1,
            _ if !group.skip => {
                if in_table && leaving {
                    b.end_table();
                    in_table = false;
                    leaving = false;
                }
                if in_table && !cell_open {
                    b.cell();
                    cell_open = true;
                }
                // RTF is 7-bit; a stray high byte is read through the code page.
                text.push(if c.is_ascii() { c as char } else { decode(c) });
            }
            _ => {}
        }
    }
    flush(&mut b, &mut text, &group);
    if in_table {
        b.end_table();
    }
    Ok(b.finish())
}

/// `\word-12 ` → the word, its number, and where the text resumes.
fn control_word(rtf: &[u8], from: usize) -> (&[u8], Option<i32>, usize) {
    let mut i = from;
    while i < rtf.len() && rtf[i].is_ascii_alphabetic() && i - from < 32 {
        i += 1;
    }
    let word = &rtf[from..i];
    let digits = i;
    if rtf.get(i) == Some(&b'-') {
        i += 1;
    }
    while i < rtf.len() && rtf[i].is_ascii_digit() && i - digits < 11 {
        i += 1;
    }
    let param = std::str::from_utf8(&rtf[digits..i])
        .ok()
        .and_then(|s| s.parse().ok());
    // One space ends a control word and is not text.
    if rtf.get(i) == Some(&b' ') {
        i += 1;
    }
    (word, param, i)
}

fn hex_byte(pair: &[u8]) -> Option<u8> {
    let nibble = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    Some(nibble(pair[0])? << 4 | nibble(pair[1])?)
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
    fn paragraphs_with_their_look_and_turkish_letters() {
        let rtf = r"{\rtf1\ansi\ansicpg1254{\fonttbl{\f0 Times;}}{\colortbl;\red0\green0\blue0;}
\f0 Mahkemesi'ne {\b kal\'fdn} ve {\i e\'f0ik}\par \u351?ey \ul alt\ulnone\par}";
        let flow = flow(rtf.as_bytes()).unwrap();
        let p = paras(&flow);
        assert_eq!(p[0].text(), "Mahkemesi'ne kalın ve eğik");
        assert!(p[0].runs.iter().any(|r| r.bold && r.text == "kalın"));
        assert!(p[0].runs.iter().any(|r| r.italic && r.text == "eğik"));
        assert_eq!(p[1].text(), "şey alt");
        assert!(p[1].runs.iter().any(|r| r.underline && r.text == "alt"));
        assert!(!p.iter().any(|p| p.text().contains("Times")));
    }

    #[test]
    fn a_table_is_a_table() {
        let rtf = r"{\rtf1\ansi \trowd\cellx1000\cellx2000 \intbl a\cell b\cell\row \trowd\cellx1000\cellx2000 \intbl c\cell d\cell\row\pard sonra\par}";
        let flow = flow(rtf.as_bytes()).unwrap();
        let Some(Block::Table(t)) = flow.blocks.first() else {
            panic!("{:?}", flow.blocks);
        };
        assert_eq!(t.rows.len(), 2);
        assert_eq!(t.rows[1].cells[1].paras[0].text(), "d");
    }
}
