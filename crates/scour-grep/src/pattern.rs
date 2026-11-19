//! What is looked for, compiled once: the text as UTF-8, and as UTF-16 for
//! the files that keep it that way.

use std::fmt::Write as _;

use regex::bytes::{Regex, RegexBuilder};

use crate::excerpt::Kind;

/// What was asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Needle {
    /// These characters, as they are.
    Text(String),
    /// A regular expression over the bytes of each line.
    Regex(String),
    /// These bytes, wherever they are.
    Bytes(Vec<u8>),
}

impl Needle {
    /// `DE AD be-ef` → the four bytes. None unless it is whole pairs of hex digits.
    pub fn hex(text: &str) -> Option<Needle> {
        let digits: Vec<u8> = text
            .bytes()
            .filter(|b| !matches!(b, b' ' | b'-' | b':' | b'_'))
            .collect();
        if digits.is_empty() || !digits.len().is_multiple_of(2) {
            return None;
        }
        let nibble = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
        digits
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| Some(nibble(p[0])? << 4 | nibble(p[1])?))
            .collect::<Option<Vec<u8>>>()
            .map(Needle::Bytes)
    }
}

/// Why a needle could not be compiled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BadPattern {
    Empty,
    /// The expression engine's own words.
    Regex(String),
}

impl std::fmt::Display for BadPattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BadPattern::Empty => f.write_str("nothing to look for"),
            BadPattern::Regex(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for BadPattern {}

/// A compiled needle. `narrow` reads text; the rest read the same characters
/// as UTF-16, for a file that says it is or one that is not text at all.
#[derive(Debug, Clone)]
pub struct Pattern {
    narrow: Regex,
    wide: Option<Wide>,
}

#[derive(Debug, Clone)]
struct Wide {
    le: Regex,
    be: Regex,
    /// UTF-8 or either UTF-16: nothing says which a binary file holds.
    any: Regex,
}

#[derive(Clone, Copy)]
enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

impl Pattern {
    /// `exact_case` off reads `a` as `A` too, and the four Turkish i's as one.
    pub fn new(needle: &Needle, exact_case: bool) -> Result<Pattern, BadPattern> {
        match needle {
            Needle::Text(text) if text.is_empty() => Err(BadPattern::Empty),
            Needle::Regex(text) if text.is_empty() => Err(BadPattern::Empty),
            Needle::Bytes(bytes) if bytes.is_empty() => Err(BadPattern::Empty),
            Needle::Text(text) => {
                let utf8 = spelled(text, Encoding::Utf8, exact_case);
                let le = spelled(text, Encoding::Utf16Le, exact_case);
                let be = spelled(text, Encoding::Utf16Be, exact_case);
                Ok(Pattern {
                    narrow: raw(&utf8)?,
                    wide: Some(Wide {
                        any: raw(&format!("(?:{utf8})|(?:{le})|(?:{be})"))?,
                        le: raw(&le)?,
                        be: raw(&be)?,
                    }),
                })
            }
            Needle::Bytes(bytes) => Ok(Pattern {
                narrow: raw(&escaped(bytes))?,
                wide: None,
            }),
            Needle::Regex(text) => RegexBuilder::new(text)
                .case_insensitive(!exact_case)
                .multi_line(true)
                .build()
                .map(|narrow| Pattern { narrow, wide: None })
                .map_err(|e| BadPattern::Regex(e.to_string())),
        }
    }

    /// The expression that reads a file of this kind.
    pub(crate) fn reading(&self, kind: Kind) -> &Regex {
        match (kind, &self.wide) {
            (Kind::Wide(false), Some(wide)) => &wide.le,
            (Kind::Wide(true), Some(wide)) => &wide.be,
            (Kind::Binary, Some(wide)) => &wide.any,
            _ => &self.narrow,
        }
    }

    /// Where a find in a binary file really starts. Little-endian `needle` is
    /// also big-endian `needle` one byte early, short of its last byte; the
    /// little-endian reading is taken, being what such files hold.
    pub(crate) fn settled(&self, hay: &[u8], start: usize, end: usize) -> (usize, usize) {
        let later = self
            .wide
            .as_ref()
            .and_then(|wide| wide.le.find_at(hay, start));
        match later {
            Some(le) if le.start() == start + 1 => (le.start(), le.end()),
            _ => (start, end),
        }
    }
}

/// A byte-for-byte expression: no Unicode classes, `\xNN` is one byte.
fn raw(expr: &str) -> Result<Regex, BadPattern> {
    RegexBuilder::new(expr)
        .unicode(false)
        .build()
        .map_err(|e| BadPattern::Regex(e.to_string()))
}

fn escaped(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 4);
    for b in bytes {
        let _ = write!(out, "\\x{b:02X}");
    }
    out
}

/// `text` in one encoding, each character every way its case may be written.
fn spelled(text: &str, encoding: Encoding, exact_case: bool) -> String {
    let mut out = String::new();
    for c in text.chars() {
        let forms = if exact_case { vec![c] } else { cases(c) };
        if let [one] = forms[..] {
            out.push_str(&escaped(&encoded(one, encoding)));
            continue;
        }
        out.push_str("(?:");
        for (i, form) in forms.iter().enumerate() {
            if i > 0 {
                out.push('|');
            }
            out.push_str(&escaped(&encoded(*form, encoding)));
        }
        out.push(')');
    }
    out
}

/// The ways one character is written when case is set aside. `i`, `I`, `İ`
/// and `ı` are one letter here, as they are to the index's own folding.
fn cases(c: char) -> Vec<char> {
    if matches!(c, 'i' | 'I' | 'İ' | 'ı') {
        return vec!['i', 'I', 'İ', 'ı'];
    }
    let mut forms = vec![c];
    let mut add = |mut other: Box<dyn Iterator<Item = char>>| {
        // A case that is two characters (`ß` → `SS`) is not another spelling of one.
        if let (Some(one), None) = (other.next(), other.next())
            && !forms.contains(&one)
        {
            forms.push(one);
        }
    };
    add(Box::new(c.to_lowercase()));
    add(Box::new(c.to_uppercase()));
    forms
}

fn encoded(c: char, encoding: Encoding) -> Vec<u8> {
    match encoding {
        Encoding::Utf8 => c.encode_utf8(&mut [0; 4]).as_bytes().to_vec(),
        Encoding::Utf16Le => c
            .encode_utf16(&mut [0; 2])
            .iter()
            .flat_map(|u| u.to_le_bytes())
            .collect(),
        Encoding::Utf16Be => c
            .encode_utf16(&mut [0; 2])
            .iter()
            .flat_map(|u| u.to_be_bytes())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_reads_pairs_and_refuses_the_rest() {
        assert_eq!(
            Needle::hex("DE ad-BE:ef"),
            Some(Needle::Bytes(vec![0xDE, 0xAD, 0xBE, 0xEF]))
        );
        assert_eq!(Needle::hex("ABC"), None);
        assert_eq!(Needle::hex("zz"), None);
        assert_eq!(Needle::hex(""), None);
    }

    #[test]
    fn the_turkish_i_is_one_letter_unless_case_is_exact() {
        let loose = Pattern::new(&Needle::Text("dizin".into()), false).unwrap();
        assert!(loose.narrow.is_match("DİZİN".as_bytes()));
        assert!(loose.narrow.is_match("DIZIN".as_bytes()));
        let exact = Pattern::new(&Needle::Text("dizin".into()), true).unwrap();
        assert!(!exact.narrow.is_match("Dizin".as_bytes()));
    }

    #[test]
    fn wide_finds_utf16_of_either_order() {
        let p = Pattern::new(&Needle::Text("şey".into()), false).unwrap();
        let le: Vec<u8> = "ŞEY".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let be: Vec<u8> = "şey".encode_utf16().flat_map(u16::to_be_bytes).collect();
        assert!(p.reading(Kind::Wide(false)).is_match(&le));
        assert!(p.reading(Kind::Wide(true)).is_match(&be));
        assert!(p.reading(Kind::Binary).is_match(&be));
        assert!(!p.reading(Kind::Text).is_match(&le));
    }

    #[test]
    fn a_binary_find_starts_where_the_little_endian_text_does() {
        let p = Pattern::new(&Needle::Text("needle".into()), false).unwrap();
        let hay: Vec<u8> = "a needle b"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let found = p.reading(Kind::Binary).find(&hay).unwrap();
        assert_eq!(found.start(), 3);
        assert_eq!(p.settled(&hay, found.start(), found.end()), (4, 16));
    }

    #[test]
    fn an_empty_needle_is_refused() {
        assert_eq!(
            Pattern::new(&Needle::Text(String::new()), false).unwrap_err(),
            BadPattern::Empty
        );
        assert!(matches!(
            Pattern::new(&Needle::Regex("(".into()), false),
            Err(BadPattern::Regex(_))
        ));
    }
}
