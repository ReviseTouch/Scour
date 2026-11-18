//! Case folding and translation. Folding lives in the contract because the index
//! and the query must fold identically: a name folded when it was indexed and a
//! term folded years later disagree silently — the file is simply not found.
//!
//! [`DefaultFolder`] sets case aside and [`Unaccented`] is what an index stores
//! and walks; changing either invalidates every index.

use std::borrow::Cow;

/// Turns text into the form that is stored and searched.
pub trait Folder: Send + Sync {
    fn fold_into(&self, src: &str, dest: &mut String);

    fn fold(&self, src: &str) -> String {
        let mut s = String::with_capacity(src.len());
        self.fold_into(src, &mut s);
        s
    }

    /// Folded text, plus a map from each folded byte offset back to a source offset.
    /// Folding changes byte length — `İ` is two bytes and folds to one — so a
    /// highlight cuts on the map. It has `folded.len() + 1` entries, ending at `len`.
    fn fold_indexed(&self, src: &str) -> (String, Vec<u32>) {
        let mut folded = String::with_capacity(src.len());
        let mut map: Vec<u32> = Vec::with_capacity(src.len() + 1);
        let mut piece = String::new();
        for (i, c) in src.char_indices() {
            piece.clear();
            let mut buf = [0u8; 4];
            self.fold_into(c.encode_utf8(&mut buf), &mut piece);
            folded.push_str(&piece);
            // Every byte of one folded character points at the same source offset.
            for _ in 0..piece.len() {
                map.push(i as u32);
            }
        }
        map.push(src.len() as u32);
        (folded, map)
    }
}

/// The canonical fold: Unicode lowercase with the `i ı I İ` family collapsed onto
/// one `i`, so `ISTANBUL`, `İSTANBUL`, `ıstanbul` and `istanbul` are one word.
/// Plain Unicode lowercasing sends them to three places instead.
#[derive(Debug, Clone, Copy, Default)]
pub struct DefaultFolder;

impl Folder for DefaultFolder {
    fn fold_into(&self, src: &str, dest: &mut String) {
        for c in src.chars() {
            for lc in c.to_lowercase() {
                match lc {
                    '\u{0307}' => {}       // combining dot above, from İ
                    'ı' => dest.push('i'), // dotless ı
                    other => dest.push(other),
                }
            }
        }
    }
}

impl DefaultFolder {
    /// Convenience for callers that have a `&str` and no folder instance.
    pub fn of(src: &str) -> String {
        DefaultFolder.fold(src)
    }
}

/// What a name is searched as: [`DefaultFolder`], then every letter without its
/// mark — `é` is `e`, `ş` is `s`, `ß` is `ss`, `Ａ` is `a` — and combining marks
/// gone. A term typed *with* a mark is compared against the [`DefaultFolder`]
/// form instead, which is how `şirket` finds only `şirket`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Unaccented;

impl Folder for Unaccented {
    fn fold_into(&self, src: &str, dest: &mut String) {
        for c in src.chars() {
            for lc in c.to_lowercase() {
                push_plain(lc, dest);
            }
        }
    }
}

impl Unaccented {
    pub fn of(src: &str) -> String {
        Unaccented.fold(src)
    }

    /// The same for text [`DefaultFolder`] already folded, borrowed when no
    /// letter in it carries a mark — which is how a term is asked whether one does.
    pub fn of_folded(folded: &str) -> Cow<'_, str> {
        if folded.is_ascii() || folded.chars().all(|c| plain(c) == Plain::Same) {
            return Cow::Borrowed(folded);
        }
        let mut out = String::with_capacity(folded.len());
        for c in folded.chars() {
            push_plain(c, &mut out);
        }
        Cow::Owned(out)
    }
}

fn push_plain(c: char, dest: &mut String) {
    match plain(c) {
        Plain::Same => dest.push(c),
        Plain::One(base) => dest.push(base),
        Plain::Two(a, b) => {
            dest.push(a as char);
            dest.push(b as char);
        }
        Plain::Gone => {}
    }
}

/// What a lowercase character is once marks are set aside.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Plain {
    Same,
    One(char),
    /// Two ASCII letters: `ß` is `ss`, `æ` is `ae`.
    Two(u8, u8),
    /// A combining mark: nothing.
    Gone,
}

/// One character of [`Unaccented`], for a caller folding into its own buffer.
/// `c` is lowercase already.
pub fn plain(c: char) -> Plain {
    use crate::accents::{BASES, MARKS, TWO};
    let cp = c as u32;
    if cp < 0xdf {
        return Plain::Same;
    }
    // Every base is below U+0500 or in U+1E00–1FFF: CJK searches no table.
    if (cp < 0x500 || (0x1e00..0x2000).contains(&cp))
        && let Ok(i) = BASES.binary_search_by_key(&cp, |&(from, _)| from)
    {
        let to = BASES[i].1;
        return if to & TWO != 0 {
            Plain::Two((to >> 8) as u8, to as u8)
        } else {
            char::from_u32(to).map_or(Plain::Same, Plain::One)
        };
    }
    // A full-width form is the ASCII it was made to sit beside.
    if (0xff01..=0xff5e).contains(&cp) {
        return Plain::One(((cp - 0xfee0) as u8).to_ascii_lowercase() as char);
    }
    if cp == 0x3000 {
        return Plain::One(' ');
    }
    let near_marks = (0x300..0x700).contains(&cp)
        || (0x1ab0..0x1e00).contains(&cp)
        || (0x20d0..0x2100).contains(&cp)
        || (0xfe20..0xfe30).contains(&cp);
    if near_marks && MARKS.iter().any(|&(lo, hi)| (lo..=hi).contains(&cp)) {
        return Plain::Gone;
    }
    Plain::Same
}

/// Uppercase by Turkish rules: `i → İ`, `ı → I`, where `str::to_uppercase` spells
/// "DEĞIŞTIRME". Presentation only; nothing is indexed in uppercase.
pub fn upper_tr(src: &str) -> String {
    let mut out = String::with_capacity(src.len() + 2);
    for c in src.chars() {
        match c {
            'i' => out.push('İ'),
            'ı' => out.push('I'),
            other => out.extend(other.to_uppercase()),
        }
    }
    out
}

/// Where translated text comes from. Keys are English message ids, so a missing
/// entry degrades to correct English rather than to a bare key.
pub trait Catalog: Send + Sync {
    /// BCP-47 tag of the language this catalogue serves.
    fn locale(&self) -> &str;

    fn get<'a>(&'a self, msgid: &'a str) -> Cow<'a, str>;
}

/// A catalogue that translates nothing. The English source is the answer.
#[derive(Debug, Clone, Copy, Default)]
pub struct SourceCatalog;

impl Catalog for SourceCatalog {
    fn locale(&self) -> &str {
        "en"
    }

    fn get<'a>(&'a self, msgid: &'a str) -> Cow<'a, str> {
        Cow::Borrowed(msgid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turkish_i_family_collapses() {
        let f = DefaultFolder;
        assert_eq!(f.fold("ISTHUKUK"), "isthukuk");
        assert_eq!(f.fold("İSTHUKUK"), "isthukuk");
        assert_eq!(f.fold("ısthukuk"), "isthukuk");
        assert_eq!(f.fold("isthukuk"), "isthukuk");
    }

    #[test]
    fn other_turkish_letters_survive() {
        let f = DefaultFolder;
        assert_eq!(f.fold("ŞEFİK"), "şefik");
        assert_eq!(f.fold("ÇALIŞKAN"), "çalişkan");
        assert_eq!(f.fold("Öğüt"), "öğüt");
        assert_eq!(f.fold("Hello.TXT"), "hello.txt");
    }

    #[test]
    fn a_letter_is_searched_without_its_mark() {
        for (name, want) in [
            ("ŞİRKET Öğüt", "sirket ogut"),
            ("Café Señor Ünïcode", "cafe senor unicode"),
            (
                "Straße Æther Øl Łódź Đà Œuvre Þór",
                "strasse aether ol lodz da oeuvre thor",
            ),
            ("ẞ", "ss"),
            ("Ёлка Й", "елка и"),
            ("Ελληνικά ΆΈΉ", "ελληνικα αεη"),
            ("Việt Nam ế", "viet nam e"),
            ("cafe\u{0301}", "cafe"),
            ("שָׁלוֹם", "שלום"),
            ("مُحَمَّد", "محمد"),
            ("ＡＢＣ１２３．ｔｘｔ", "abc123.txt"),
            ("報告\u{3000}書", "報告 書"),
            // A voicing mark is another sound, and a vowel sign is a letter.
            ("が ガ", "が ガ"),
            ("हिंदी", "हिंदी"),
            ("한글", "한글"),
        ] {
            assert_eq!(Unaccented.fold(name), want, "{name:?}");
            assert_eq!(Unaccented::of_folded(&DefaultFolder.fold(name)), want);
        }
    }

    #[test]
    fn folding_a_fold_changes_nothing() {
        for cp in 0..0x30000u32 {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            let once = Unaccented.fold(c.encode_utf8(&mut [0; 4]));
            assert_eq!(Unaccented.fold(&once), once, "U+{cp:04X}");
            assert_eq!(Unaccented::of_folded(&once), once, "U+{cp:04X}");
        }
    }

    #[test]
    fn a_term_without_a_mark_is_borrowed() {
        assert!(matches!(Unaccented::of_folded("rapor"), Cow::Borrowed(_)));
        assert!(matches!(Unaccented::of_folded("報告"), Cow::Borrowed(_)));
        assert!(matches!(Unaccented::of_folded("şirket"), Cow::Owned(_)));
    }

    #[test]
    fn indexed_fold_maps_back_to_the_source_spelling() {
        // 'İ' is two bytes and folds to one, so the cut must use the map.
        let src = "İSTANBUL.txt";
        let (folded, map) = DefaultFolder.fold_indexed(src);
        assert_eq!(folded, "istanbul.txt");
        let at = folded.find("stan").unwrap();
        assert_eq!(&src[map[at] as usize..map[at + 4] as usize], "STAN");
        assert_eq!(map.len(), folded.len() + 1);
        assert_eq!(*map.last().unwrap() as usize, src.len());
    }

    #[test]
    fn turkish_uppercase() {
        assert_eq!(upper_tr("Değiştirme"), "DEĞİŞTİRME");
        assert_eq!(upper_tr("ışık"), "IŞIK");
        assert_eq!(upper_tr("Boyut"), "BOYUT");
    }

    #[test]
    fn the_null_catalogue_returns_english() {
        assert_eq!(SourceCatalog.get("Folder"), "Folder");
        assert_eq!(SourceCatalog.locale(), "en");
    }
}
