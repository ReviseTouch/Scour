//! An RTF document's text: control words dropped, the groups that are not
//! text skipped, `\'hh` read through the document's code page and `\uN` as
//! itself. Read in one pass, in process, without a converter.

use crate::codepage::code_page;

/// Whether these are the first bytes of an RTF document.
pub fn is_rtf(head: &[u8]) -> bool {
    head.starts_with(b"{\\rtf")
}

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
];

#[derive(Clone, Copy)]
struct Group {
    skip: bool,
    /// Characters standing in for each `\uN` that follows.
    uc: u32,
}

/// Append the text of `rtf` to `out`, at most `cap` bytes of it. False when
/// `rtf` is not RTF at all.
pub fn rtf_text(rtf: &[u8], out: &mut String, cap: usize) -> bool {
    if !is_rtf(rtf) {
        return false;
    }
    let mut decode = code_page(1252);
    let mut stack: Vec<Group> = Vec::with_capacity(32);
    let mut group = Group { skip: false, uc: 1 };
    // Fallback characters still to be passed over after a `\uN`.
    let mut owed = 0u32;
    let mut i = 0usize;
    while i < rtf.len() && out.len() < cap {
        let b = rtf[i];
        i += 1;
        match b {
            b'{' => {
                stack.push(group);
                owed = 0;
            }
            b'}' => {
                group = stack.pop().unwrap_or(group);
                owed = 0;
            }
            b'\r' | b'\n' => {}
            b'\\' => {
                let Some(&next) = rtf.get(i) else { break };
                if next.is_ascii_alphabetic() {
                    let (word, param, after) = control_word(rtf, i);
                    i = after;
                    if word == b"bin" {
                        // Raw bytes follow, as many as it says.
                        i = i.saturating_add(param.unwrap_or(0).max(0) as usize);
                        continue;
                    }
                    if owed > 0 && !is_destination(word) {
                        owed -= 1;
                        continue;
                    }
                    match word {
                        b"ansicpg" => decode = code_page(param.unwrap_or(1252) as u32),
                        b"uc" => group.uc = param.unwrap_or(1).max(0) as u32,
                        b"u" => {
                            let n = param.unwrap_or(0);
                            let n = if n < 0 { n + 65_536 } else { n } as u32;
                            if !group.skip {
                                out.push(char::from_u32(n).unwrap_or('\u{FFFD}'));
                            }
                            owed = group.uc;
                        }
                        _ if SKIPPED.contains(&word) => group.skip = true,
                        _ if !group.skip => {
                            if let Some(s) = said(word) {
                                out.push_str(s);
                            }
                        }
                        _ => {}
                    }
                } else {
                    i += 1;
                    match next {
                        b'\'' => {
                            let byte = rtf.get(i..i + 2).and_then(hex_byte);
                            i += 2;
                            if owed > 0 {
                                owed -= 1;
                            } else if let (Some(byte), false) = (byte, group.skip) {
                                out.push(decode(byte));
                            }
                        }
                        b'*' => group.skip = true,
                        b'~' if !group.skip => out.push(' '),
                        b'_' if !group.skip => out.push('-'),
                        b'\\' | b'{' | b'}' if !group.skip => out.push(next as char),
                        _ => {}
                    }
                }
            }
            _ if owed > 0 => owed -= 1,
            _ if !group.skip => {
                // RTF is 7-bit; a stray high byte is read through the code page.
                out.push(if b.is_ascii() { b as char } else { decode(b) });
            }
            _ => {}
        }
    }
    true
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

fn is_destination(word: &[u8]) -> bool {
    SKIPPED.contains(&word)
}

fn hex_byte(pair: &[u8]) -> Option<u8> {
    let nibble = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    Some(nibble(pair[0])? << 4 | nibble(pair[1])?)
}

/// What a control word means as text.
fn said(word: &[u8]) -> Option<&'static str> {
    Some(match word {
        b"par" | b"line" | b"sect" | b"page" | b"row" => "\n",
        b"tab" | b"cell" => "\t",
        b"emdash" => "—",
        b"endash" => "–",
        b"bullet" => "•",
        b"lquote" => "‘",
        b"rquote" => "’",
        b"ldblquote" => "“",
        b"rdblquote" => "”",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(rtf: &str) -> String {
        let mut out = String::new();
        assert!(rtf_text(rtf.as_bytes(), &mut out, 1 << 20));
        out
    }

    #[test]
    fn turkish_letters_come_through_the_code_page_and_as_unicode() {
        let rtf = r"{\rtf1\ansi\ansicpg1254{\fonttbl{\f0 Times;}}\f0 Mahkemesi'ne \'fe\'fd\'f0\par \u351?ey}";
        assert_eq!(text(rtf), "Mahkemesi'ne şığ\nşey");
    }

    #[test]
    fn what_is_not_text_is_not_returned() {
        let rtf = r"{\rtf1{\*\generator Word;}{\info{\title secret}}{\pict\pngblip 89504e47}body{\field{\*\fldinst HYPERLINK x}{\fldrslt link}}}";
        assert_eq!(text(rtf), "bodylink");
    }

    #[test]
    fn escapes_and_binary_runs() {
        assert_eq!(text(r"{\rtf1 a\{b\}c\\d}"), "a{b}c\\d");
        let mut rtf = b"{\\rtf1 x\\bin4 ".to_vec();
        rtf.extend_from_slice(b"\x00}{\\");
        rtf.extend_from_slice(b"y}");
        let mut out = String::new();
        assert!(rtf_text(&rtf, &mut out, 100));
        assert_eq!(out, "xy");
    }

    #[test]
    fn not_rtf_is_said() {
        assert!(!rtf_text(b"plain", &mut String::new(), 10));
    }
}
