//! Case folding and translation. Folding lives in the contract because the index
//! and the query must fold identically: a name folded when it was indexed and a
//! term folded years later disagree silently — the file is simply not found.
//!
//! [`DefaultFolder`] is the canonical rule; replacing it invalidates every index.

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
