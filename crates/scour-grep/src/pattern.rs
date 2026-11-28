//! What is looked for, compiled once: the text as UTF-8, and as UTF-16 for
//! the files that keep it that way.

use std::fmt::Write as _;
use std::sync::{Arc, OnceLock};

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
    /// Bytes with gaps: `None` is any byte. `at_start`: only at offset zero.
    Wild {
        bytes: Vec<Option<u8>>,
        at_start: bool,
    },
}

impl Needle {
    /// `DE AD be-ef` → the four bytes; `?? ` is any byte, and a leading `^`
    /// says at the start of the file. None unless it is whole pairs.
    pub fn hex(text: &str) -> Option<Needle> {
        let text = text.trim();
        let (at_start, text) = match text.strip_prefix('^') {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        let digits: Vec<u8> = text
            .bytes()
            .filter(|b| !matches!(b, b' ' | b'-' | b':' | b'_'))
            .collect();
        if digits.is_empty() || !digits.len().is_multiple_of(2) {
            return None;
        }
        let nibble = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
        let bytes = digits
            .as_chunks::<2>()
            .0
            .iter()
            .map(|p| match p {
                [b'?', b'?'] => Some(None),
                _ => Some(Some(nibble(p[0])? << 4 | nibble(p[1])?)),
            })
            .collect::<Option<Vec<Option<u8>>>>()?;
        if bytes.iter().all(Option::is_none) {
            return None;
        }
        if !at_start && bytes.iter().all(Option::is_some) {
            return Some(Needle::Bytes(bytes.into_iter().flatten().collect()));
        }
        Some(Needle::Wild { bytes, at_start })
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

/// A compiled needle. `unicode` reads text that is UTF-8 and `narrow` text
/// that may be in Windows' Turkish code page; the rest read the same
/// characters as UTF-16, for a file that says it is or one that is not text
/// at all.
#[derive(Debug, Clone)]
pub struct Pattern {
    narrow: Regex,
    /// The needle's UTF-8 spelling alone, for a buffer that is UTF-8 — nearly
    /// every one. Not `narrow` with its code-page spelling beside it: that
    /// alternation stops the engine's literal search, and `aile` in Turkish
    /// text is read at 871 MB/s by it and at 13,531 by this (28 November, 64
    /// MB, one thread). None when the two are one expression.
    unicode: Option<Regex>,
    /// Its text is UTF-8 already — words taken out of a document — so no
    /// buffer is checked.
    words: bool,
    wide: Option<Wide>,
    raw_bytes: bool,
    /// Matches only at a file's first byte.
    at_start: bool,
    /// Each alternative alone, anchored at both ends, when a misspelt find
    /// carries the character before it that says a word begins there.
    bodies: Option<Arc<Bodies>>,
    /// What a buffer must hold for a misspelling to be in it: the expression
    /// itself has no literal to look for first.
    pieces: Option<Arc<Pattern>>,
}

/// One alternative of a text pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Term {
    /// These characters, as they are.
    Plain(String),
    /// This word, or one `u8` mistakes from it where a word begins.
    Near(String, u8),
}

/// Built at the first find: as long to build as the pattern itself, and
/// most patterns find nothing.
#[derive(Debug)]
struct Bodies {
    /// UTF-8 text, text of either spelling, little- and big-endian UTF-16,
    /// and any of them.
    spelled: [String; 5],
    built: OnceLock<Option<[Regex; 5]>>,
}

/// Which of a pattern's expressions reads a buffer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Reading {
    Unicode,
    Narrow,
    Le,
    Be,
    Any,
}

#[derive(Debug, Clone)]
struct Wide {
    le: Regex,
    be: Regex,
    /// UTF-8 or either UTF-16: nothing says which a binary file holds.
    any: Regex,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Encoding {
    /// Windows' Turkish code page, for text files that are not UTF-8.
    Cp1254,
    Utf8,
    Utf16Le,
    Utf16Be,
}

/// How letters are compared. `exact_case` off reads `a` as `A` too, and the
/// four Turkish i's as one; `exact_accents` off reads a letter typed with no
/// mark as that letter with Turkish's marks — `sirket` finds `şirket`,
/// `Necmeddin` finds `Necmeddîn` (see [`MARKED`] for why those alone) — while
/// a letter typed with one is that letter alone, as the index's reading of
/// names has it. A bare `bool` is the case alone,
/// marks set aside, which is the default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Letters {
    pub exact_case: bool,
    pub exact_accents: bool,
}

impl From<bool> for Letters {
    fn from(exact_case: bool) -> Letters {
        Letters {
            exact_case,
            exact_accents: false,
        }
    }
}

impl Pattern {
    /// The needle, its letters compared as `letters` says.
    pub fn new(needle: &Needle, letters: impl Into<Letters>) -> Result<Pattern, BadPattern> {
        let letters = letters.into();
        let exact_case = letters.exact_case;
        match needle {
            Needle::Text(text) if text.is_empty() => Err(BadPattern::Empty),
            Needle::Regex(text) if text.is_empty() => Err(BadPattern::Empty),
            Needle::Bytes(bytes) if bytes.is_empty() => Err(BadPattern::Empty),
            Needle::Wild { bytes, .. } if bytes.is_empty() => Err(BadPattern::Empty),
            Needle::Text(text) => Self::any_text(std::slice::from_ref(text), letters),
            Needle::Bytes(bytes) => Ok(Pattern {
                narrow: raw(&escaped(bytes))?,
                unicode: None,
                words: false,
                wide: None,
                raw_bytes: true,
                at_start: false,
                bodies: None,
                pieces: None,
            }),
            Needle::Wild { bytes, at_start } => {
                let mut expr = String::from(if *at_start { "\\A" } else { "" });
                for b in bytes {
                    match b {
                        Some(b) => expr.push_str(&escaped(&[*b])),
                        None => expr.push_str("(?s:.)"),
                    }
                }
                Ok(Pattern {
                    narrow: raw(&expr)?,
                    unicode: None,
                    words: false,
                    wide: None,
                    raw_bytes: true,
                    at_start: *at_start,
                    bodies: None,
                    pieces: None,
                })
            }
            Needle::Regex(text) => RegexBuilder::new(text)
                .case_insensitive(!exact_case)
                .multi_line(true)
                .build()
                .map(|narrow| Pattern {
                    narrow,
                    unicode: None,
                    words: false,
                    wide: None,
                    raw_bytes: false,
                    at_start: false,
                    bodies: None,
                    pieces: None,
                })
                .map_err(|e| BadPattern::Regex(e.to_string())),
        }
    }

    /// One compiled DFA matching any of `alts` (`a;b` / `a|b`) in a single pass.
    pub fn any_text(alts: &[String], letters: impl Into<Letters>) -> Result<Pattern, BadPattern> {
        let terms: Vec<Term> = alts.iter().map(|t| Term::Plain(t.clone())).collect();
        Self::any_term(&terms, letters.into())
    }

    pub(crate) fn any_term(alts: &[Term], letters: Letters) -> Result<Pattern, BadPattern> {
        let alts: Vec<&Term> = alts.iter().filter(|t| !t.text().is_empty()).collect();
        if alts.is_empty() {
            return Err(BadPattern::Empty);
        }
        let near = alts.iter().any(|t| matches!(t, Term::Near(..)));
        let join_enc = |enc: Encoding, body: bool| -> Option<String> {
            let spelled: Vec<String> = alts
                .iter()
                .filter_map(|t| match t {
                    Term::Plain(text) => spelled(text, enc, letters),
                    Term::Near(word, edits) => {
                        let it = crate::near::Near::new(word, *edits, enc, letters)?;
                        Some(if body { it.body() } else { it.find() })
                    }
                })
                .collect();
            match spelled.as_slice() {
                [] => None,
                [only] => Some(only.clone()),
                many => Some(
                    many.iter()
                        .map(|t| format!("(?:{t})"))
                        .collect::<Vec<_>>()
                        .join("|"),
                ),
            }
        };
        let readings = |body: bool| -> Result<[String; 5], BadPattern> {
            let utf8 = join_enc(Encoding::Utf8, body).ok_or(BadPattern::Empty)?;
            let le = join_enc(Encoding::Utf16Le, body).ok_or(BadPattern::Empty)?;
            let be = join_enc(Encoding::Utf16Be, body).ok_or(BadPattern::Empty)?;
            // Only when it differs: an ASCII needle is the same bytes in 1254.
            let narrow = match join_enc(Encoding::Cp1254, body) {
                Some(legacy) if legacy != utf8 => format!("(?:{utf8})|(?:{legacy})"),
                _ => utf8.clone(),
            };
            let any = format!("(?:{narrow})|(?:{le})|(?:{be})");
            Ok([utf8, narrow, le, be, any])
        };
        let [utf8, narrow, le, be, any] = readings(false)?;
        let pieces = if near {
            let plain: Option<Vec<Term>> = alts
                .iter()
                .map(|t| match t {
                    Term::Plain(text) => Some(vec![Term::Plain(text.clone())]),
                    Term::Near(word, edits) => crate::near::pieces(word, *edits)
                        .map(|p| p.into_iter().map(Term::Plain).collect()),
                })
                .collect::<Option<Vec<Vec<Term>>>>()
                .map(|all| all.concat());
            match plain {
                Some(plain) => Some(Arc::new(Self::any_term(&plain, letters)?)),
                None => None,
            }
        } else {
            None
        };
        let bodies = if near {
            Some(Arc::new(Bodies {
                spelled: readings(true)?,
                built: OnceLock::new(),
            }))
        } else {
            None
        };
        Ok(Pattern {
            wide: Some(Wide {
                any: raw(&any)?,
                le: raw(&le)?,
                be: raw(&be)?,
            }),
            unicode: if utf8 == narrow {
                None
            } else {
                Some(raw(&utf8)?)
            },
            narrow: raw(&narrow)?,
            words: false,
            raw_bytes: false,
            at_start: false,
            bodies,
            pieces,
        })
    }

    /// The same needle for text known to be UTF-8 — the words taken out of a
    /// document — which is read without being checked.
    pub(crate) fn for_words(&self) -> Pattern {
        Pattern {
            words: true,
            pieces: self.pieces.as_ref().map(|p| Arc::new(p.for_words())),
            ..self.clone()
        }
    }

    pub(crate) fn is_raw_bytes(&self) -> bool {
        self.raw_bytes
    }

    pub(crate) fn at_start(&self) -> bool {
        self.at_start
    }

    /// Which expression reads `hay`, a buffer of a file of this kind: text
    /// by its UTF-8 spelling alone when every byte of it is UTF-8.
    pub(crate) fn reading_of(&self, kind: Kind, hay: &[u8]) -> Reading {
        match (kind, &self.wide) {
            (Kind::Wide(false), Some(_)) => Reading::Le,
            (Kind::Wide(true), Some(_)) => Reading::Be,
            (Kind::Binary, Some(_)) => Reading::Any,
            _ if self.unicode.is_some() && (self.words || is_utf8(hay)) => Reading::Unicode,
            _ => Reading::Narrow,
        }
    }

    pub(crate) fn reading(&self, reading: Reading) -> &Regex {
        match (reading, &self.wide, &self.unicode) {
            (Reading::Le, Some(wide), _) => &wide.le,
            (Reading::Be, Some(wide), _) => &wide.be,
            (Reading::Any, Some(wide), _) => &wide.any,
            (Reading::Unicode, _, Some(unicode)) => unicode,
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

    /// The span of every find in `hay` read as `kind`, in order.
    pub(crate) fn finds<'a>(
        &'a self,
        kind: Kind,
        hay: &'a [u8],
    ) -> impl Iterator<Item = (usize, usize)> + 'a {
        let reading = self.reading_of(kind, hay);
        let held = self
            .pieces
            .as_ref()
            .is_none_or(|p| p.reading(reading).is_match(hay));
        self.reading(reading)
            .find_iter(if held { hay } else { &[] })
            .filter(|m| !m.is_empty())
            .map(move |m| {
                let (start, end) = match kind {
                    Kind::Binary => self.settled(hay, m.start(), m.end()),
                    _ => (m.start(), m.end()),
                };
                (self.word_begun(kind, reading, hay, start, end), end)
            })
    }

    /// A misspelt find begins with the character before the word: stepped
    /// over, so the span is the word alone.
    fn word_begun(
        &self,
        kind: Kind,
        reading: Reading,
        hay: &[u8],
        start: usize,
        end: usize,
    ) -> usize {
        let Some(bodies) = &self.bodies else {
            return start;
        };
        let built = bodies.built.get_or_init(|| {
            let whole = |e: &String| raw(&format!("\\A(?:{e})\\z")).ok();
            let [utf8, narrow, le, be, any] = &bodies.spelled;
            Some([
                whole(utf8)?,
                whole(narrow)?,
                whole(le)?,
                whole(be)?,
                whole(any)?,
            ])
        });
        let Some([utf8, narrow, le, be, any]) = built else {
            return start;
        };
        let body = match reading {
            Reading::Unicode => utf8,
            Reading::Narrow => narrow,
            Reading::Le => le,
            Reading::Be => be,
            Reading::Any => any,
        };
        if body.is_match(&hay[start..end]) {
            return start;
        }
        let widths: &[usize] = match kind {
            Kind::Text => &[1, 2, 3, 4],
            Kind::Wide(_) => &[2],
            Kind::Binary => &[2, 1, 3, 4],
        };
        widths
            .iter()
            .map(|w| start + w)
            .find(|&from| from < end && body.is_match(&hay[from..end]))
            .unwrap_or(start)
    }
}

impl Term {
    fn text(&self) -> &str {
        match self {
            Term::Plain(text) | Term::Near(text, _) => text,
        }
    }
}

/// Whether every byte of `hay` is UTF-8, a character the read cut at either
/// end counted whole. `simdutf8` checks Turkish text at 16 GB/s; `std`'s
/// check, at 0.9, would be slower than the search it decides.
fn is_utf8(hay: &[u8]) -> bool {
    let cut = hay
        .iter()
        .take(3)
        .take_while(|&&b| b & 0xC0 == 0x80)
        .count();
    match simdutf8::compat::from_utf8(&hay[cut..]) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none(),
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

/// `text` in one encoding, each character every way its case may be written
/// and, in Unicode, composed or not. None when the encoding cannot write it.
fn spelled(text: &str, encoding: Encoding, letters: Letters) -> Option<String> {
    let mut out = String::new();
    for c in text.chars() {
        out.push_str(&atom(c, encoding, letters)?);
    }
    Some(out)
}

/// One character of [`spelled`]. None when the encoding cannot write it.
pub(crate) fn atom(c: char, encoding: Encoding, letters: Letters) -> Option<String> {
    // A space in a phrase is any run of blanks and line breaks: a PDF's
    // text breaks lines mid-phrase (15 % of phrases in 300 court PDFs).
    match (c, encoding) {
        // Tab, line breaks, form feed, the no-break and the typographic spaces.
        (' ', Encoding::Utf8) => {
            return Some(
                "(?:[\\t\\n\\x0B\\x0C\\r ]|\\xC2\\xA0|\\xE2\\x80[\\x80-\\x8A\\xAF])+".into(),
            );
        }
        (' ', Encoding::Cp1254) => return Some("[\\t\\n\\x0B\\x0C\\r \\xA0]+".into()),
        _ => {}
    }
    let mut forms = if letters.exact_case {
        vec![c]
    } else {
        cases(c)
    };
    // A letter typed with no mark stands for itself with any: each of its
    // forms brings its marked ones.
    if !letters.exact_accents && c.is_alphabetic() && scour_extract::decomposed(c).is_none() {
        let marked: Vec<char> = forms
            .iter()
            .flat_map(|&f| marked(f).iter().copied())
            .collect();
        for m in marked {
            if !forms.contains(&m) {
                forms.push(m);
            }
        }
    }
    let mut ways: Vec<Vec<u8>> = Vec::new();
    for form in forms {
        ways.extend(encoded(form, encoding));
        if encoding != Encoding::Cp1254
            && let Some(nfd) = scour_extract::decomposed(form)
        {
            let mut bytes = Vec::new();
            for part in nfd.chars() {
                bytes.extend(encoded(part, encoding)?);
            }
            ways.push(bytes);
        }
    }
    Some(match ways.as_slice() {
        [] => return None,
        [one] => escaped(one),
        many => {
            let mut out = String::from("(?:");
            for (i, way) in many.iter().enumerate() {
                if i > 0 {
                    out.push('|');
                }
                out.push_str(&escaped(way));
            }
            out.push(')');
            out
        }
    })
}

/// The marked letters a plain one stands for: Turkish's — `â î û`, `ç ğ ö ş ü`
/// and their capitals (`ı` and `İ` are [`cases`]).
///
/// **Not every mark in Latin-1** (28 November, 64 MB of Turkish text, one
/// thread, the UTF-8 spelling alone): folding them all puts fifteen to
/// twenty spellings on each vowel, and a short word of vowels loses the
/// engine's literal search — `aile` is read at 446 MB/s, against 13,531
/// with Turkish's marks alone.
const MARKED: [char; 16] = [
    'â', 'Â', 'î', 'Î', 'û', 'Û', 'ç', 'Ç', 'ö', 'Ö', 'ü', 'Ü', 'ğ', 'Ğ', 'ş', 'Ş',
];

/// The letters of [`MARKED`] that are `base` with a mark on it, by each one's
/// own decomposition.
fn marked(base: char) -> &'static [char] {
    static TABLE: OnceLock<std::collections::HashMap<char, Vec<char>>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        let mut table: std::collections::HashMap<char, Vec<char>> =
            std::collections::HashMap::new();
        for letter in MARKED {
            if let Some(first) = scour_extract::decomposed(letter).and_then(|d| d.chars().next())
                && first.is_ascii_alphabetic()
            {
                table.entry(first).or_default().push(letter);
            }
        }
        table
    });
    table.get(&base).map_or(&[], Vec::as_slice)
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

fn encoded(c: char, encoding: Encoding) -> Option<Vec<u8>> {
    Some(match encoding {
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
        Encoding::Cp1254 => vec![scour_extract::cp1254_byte(c)?],
    })
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

    /// A letter typed with no mark is that letter with any; one typed with a
    /// mark is itself — and with the marks made exact, every letter is.
    #[test]
    fn a_plain_letter_finds_its_marked_forms_unless_marks_are_exact() {
        let finds = |needle: &str, letters: Letters, text: &str| {
            Pattern::new(&Needle::Text(needle.into()), letters)
                .unwrap()
                .narrow
                .is_match(text.as_bytes())
        };
        let loose = Letters::default();
        assert!(finds(
            "Necmeddin",
            loose,
            "Mardin, Necmeddîn İlgāzi Mâristânı"
        ));
        assert!(finds("sirket", loose, "Deniz Yapı Anonim ŞİRKETİ"));
        assert!(finds("maristani", loose, "Mâristânı"));
        assert!(finds("ogrenci", loose, "Öğrenci: Zeynep"));
        // Decomposed in the file: an `s` and a combining cedilla.
        assert!(finds("sirket", loose, "s\u{327}irket"));
        // Typed with its mark, it is that letter.
        assert!(!finds("şirket", loose, "sirket"));
        assert!(finds("şirket", loose, "ŞİRKET"));
        let exact = Letters {
            exact_case: false,
            exact_accents: true,
        };
        assert!(!finds("Necmeddin", exact, "Necmeddîn"));
        assert!(finds("Necmeddîn", exact, "NECMEDDÎN"));
        // The case and the marks are apart.
        let case_only = Letters {
            exact_case: true,
            exact_accents: false,
        };
        assert!(finds("sirket", case_only, "şirket"));
        assert!(!finds("sirket", case_only, "Şirket"));
    }

    /// A buffer that is UTF-8 is read by that spelling alone. The code page's
    /// `â` is the byte that leads `’` in UTF-8, so `kira` found `kir’` in it.
    #[test]
    fn utf8_text_is_not_read_as_the_code_page_too() {
        let p = Pattern::new(&Needle::Text("kira".into()), false).unwrap();
        let finds = |hay: &[u8]| p.finds(Kind::Text, hay).count();
        assert_eq!(finds("Bu kir’den, KİRÂ".as_bytes()), 1);
        // `kirâ` in 1254, and a line of each spelling in one file.
        assert_eq!(finds(b"kir\xE2 bedeli"), 1);
        let s = Pattern::new(&Needle::Text("sirket".into()), false).unwrap();
        let mixed = b"\xC5\x9Firket ve \xFEirket";
        assert_eq!(s.finds(Kind::Text, mixed).count(), 2);
        // Words taken out of a document are not checked.
        let words = s.for_words();
        assert_eq!(words.finds(Kind::Text, "ŞİRKET".as_bytes()).count(), 1);
    }

    #[test]
    fn a_character_cut_by_the_read_does_not_make_text_another_spelling() {
        let text = "aşk".as_bytes();
        assert!(is_utf8(&text[..2]));
        assert!(is_utf8(&text[2..]));
        assert!(is_utf8(b""));
        assert!(!is_utf8(b"a\xFEk"));
        assert!(!is_utf8(b"\xC5\x9F\xFF\xC5\x9F"));
    }

    #[test]
    fn wide_finds_utf16_of_either_order() {
        let p = Pattern::new(&Needle::Text("şey".into()), false).unwrap();
        let le: Vec<u8> = "ŞEY".encode_utf16().flat_map(u16::to_le_bytes).collect();
        let be: Vec<u8> = "şey".encode_utf16().flat_map(u16::to_be_bytes).collect();
        assert!(p.reading(Reading::Le).is_match(&le));
        assert!(p.reading(Reading::Be).is_match(&be));
        assert!(p.reading(Reading::Any).is_match(&be));
        assert!(!p.reading(Reading::Narrow).is_match(&le));
    }

    #[test]
    fn a_binary_find_starts_where_the_little_endian_text_does() {
        let p = Pattern::new(&Needle::Text("needle".into()), false).unwrap();
        let hay: Vec<u8> = "a needle b"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let found = p.reading(Reading::Any).find(&hay).unwrap();
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
