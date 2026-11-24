
use super::*;
use crate::names::Folded;
use scour_core::text::{Folder, Unaccented};

#[test]
fn a_needle_finds_what_a_plain_lowercase_contains_would() {
    let mut fold = Folded::new();
    for (name, needle) in [
        ("main.rs", "main"),
        ("MAIN.RS", "main"),
        ("Rapor-2026.pdf", "rapor"),
        ("rapor", "rapor"),
        ("a", "ab"),
        ("abc", "bc"),
        ("abc", "d"),
        ("", "x"),
        ("READ[ME]", "d[m"),
        ("READ{ME}", "d[m"),
    ] {
        let n = Needle::new(needle);
        let want = name.to_ascii_lowercase().contains(needle);
        assert_eq!(
            n.found_in(fold.fold_bytes(name.as_bytes())),
            want,
            "{name:?} contains {needle:?}"
        );
    }
}

#[test]
fn a_non_ascii_name_still_goes_through_the_real_folding() {
    let mut fold = Folded::new();
    // The Turkish rule: the query is folded by the parser, the name here.
    let folded =
        |fold: &mut Folded, text: &str| -> Vec<u8> { fold.fold_bytes(text.as_bytes()).to_vec() };
    assert!(Needle::new("istanbul").found_in(&folded(&mut fold, "İSTANBUL.txt")));
    assert!(Needle::new("isparta").found_in(&folded(&mut fold, "ısparta.md")));
    assert!(Needle::new("ogut").found_in(&folded(&mut fold, "Öğüt.docx")));
    assert!(Needle::new("strasse").found_in(&folded(&mut fold, "Straße 12.pdf")));
    assert!(!Needle::new("zzz").found_in(&folded(&mut fold, "Öğüt.docx")));
}

#[test]
fn the_folded_extension_is_the_one_the_core_defines() {
    for name in [
        "main.rs",
        "a.tar.gz",
        ".bashrc",
        "noext",
        "trailing.",
        "x.averyverylongextension",
        "UPPER.PDF",
        "İstanbul.TXT",
        "Résumé.PDÉ",
    ] {
        let folded = Unaccented.fold(name);
        let want = scour_core::ext_of(name);
        assert_eq!(
            crate::extension_order::folded_extension(name.as_bytes(), folded.as_bytes()),
            want.as_bytes(),
            "{name:?}"
        );
    }
}
