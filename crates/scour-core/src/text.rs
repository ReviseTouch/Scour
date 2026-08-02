//! Case folding and translation.
//!
//! Folding is in the contract rather than in an implementation crate for one
//! reason: the index and the query must fold identically or nothing matches.
//! A name is folded once when it is indexed and a search term is folded again
//! when it is typed, possibly years apart, possibly by different processes. If
//! those two ever disagree the failure is silent — the file is simply not
//! found, and no error is raised anywhere.
//!
//! So [`DefaultFolder`] is the canonical rule, shipped here, and [`Folder`] is
//! the way to replace it (with ICU's locale-aware case mapping, say) knowing
//! that replacing it invalidates every existing index.

use std::borrow::Cow;

/// Turns text into the form that is stored and searched.
pub trait Folder: Send + Sync {
    fn fold_into(&self, src: &str, dest: &mut String);

    fn fold(&self, src: &str) -> String {
        let mut s = String::with_capacity(src.len());
        self.fold_into(src, &mut s);
        s
    }

    /// Folded text, plus a map from each folded byte offset back to a source
    /// byte offset.
    ///
    /// Folding can change the byte length — `İ` is two bytes and folds to one —
    /// so cutting the source at an offset found in the folded text is wrong.
    /// Highlighting a match in the original spelling is what needs this.
    ///
    /// The returned vector has `folded.len() + 1` entries; the last is the end
    /// of the source.
    fn fold_indexed(&self, src: &str) -> (String, Vec<u32>) {
        let mut folded = String::with_capacity(src.len());
        let mut map: Vec<u32> = Vec::with_capacity(src.len() + 1);
        let mut piece = String::new();
        for (i, c) in src.char_indices() {
            piece.clear();
            let mut buf = [0u8; 4];
            self.fold_into(c.encode_utf8(&mut buf), &mut piece);
            folded.push_str(&piece);
            // One source character can fold to several bytes; all of them point
            // at the same source offset, so a highlight is always cut on a
            // character boundary.
            for _ in 0..piece.len() {
                map.push(i as u32);
            }
        }
        map.push(src.len() as u32);
        (folded, map)
    }
}

/// The canonical fold: Unicode lowercase, with the Turkish `i` family
/// collapsed onto a single `i`.
///
/// Turkish has four members of that family — `i ı I İ` — and standard Unicode
/// lowercasing sends them to three different places (`I→i`, `ı→ı`, `İ→i+U+0307`).
/// Collapsing them makes `ISTANBUL`, `İSTANBUL`, `ıstanbul` and `istanbul` one
/// word, which is what a person searching for it expects. English loses
/// nothing: `I→i` was already the rule.
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

/// Uppercase by Turkish rules: `i → İ`, `ı → I`.
///
/// `str::to_uppercase` applies the English rule and spells a Turkish heading
/// "DEĞIŞTIRME" instead of "DEĞİŞTİRME". Only presentation needs this; nothing
/// is indexed in uppercase.
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

/// Where translated text comes from.
///
/// Keys are English message ids, so a missing catalogue entry degrades to
/// correct English rather than to a bare key.
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
        // 'İ' is two bytes and folds to one, so the offsets diverge and the
        // highlight has to be cut using the map, not the folded offset.
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
