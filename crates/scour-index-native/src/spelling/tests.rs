use super::*;
use crate::names::NameWriter;

const NAMES: &[&str] = &[
    "main.rs",
    "README.md",
    "Makefile",
    "IMG_0042.JPG",
    "İSTANBUL.txt",
    "ısparta.md",
    "ÇALIŞKAN",
    "Öğüt.docx",
    "ÜÖÇ.PDF",
    "",
    "ẞ.txt",
    "Привет.TXT",
    "KENAN PİLEVNELİ.docx",
    "para isteme yazısı.docx",
    "ŞIRNAK MERKEZ (İMAR İŞLEMLERİ).docx",
    "Ğ",
    "hi",
    "a",
    "A",
];

/// Enough rows for several blocks, capitals at every place in a block.
fn corpus() -> Vec<String> {
    (0..333)
        .map(|i| {
            let base = NAMES[(i * 7 + i / 5) % NAMES.len()];
            if i % 3 == 0 {
                base.to_owned()
            } else {
                format!("{base}{i}")
            }
        })
        .collect()
}

fn build(names: &[String]) -> (Vec<u8>, Vec<u8>) {
    let mut w = NameWriter::new();
    for n in names {
        w.push(n);
    }
    w.finish_both()
}

#[test]
fn every_spelling_comes_back_by_row() {
    let names = corpus();
    let (spell, folded) = build(&names);
    let arena = NameArena::open(&folded).expect("fold");
    let s = Spelling::open(arena, &spell).expect("open");
    assert!(s.is_whole());
    assert_eq!(s.rows(), names.len());
    for (row, want) in names.iter().enumerate() {
        assert_eq!(s.get(row).as_deref(), Some(want.as_str()), "row {row}");
    }
    assert_eq!(s.get(names.len()), None);
}

#[test]
fn a_name_with_its_marks_is_what_folding_its_spelling_gives() {
    use scour_core::text::{DefaultFolder, Folder};
    let mut names = corpus();
    for extra in [
        "Йод-йод.TXT",
        "Ѡѡ.md",
        "Çağ Öğüt ŞİŞ ışık",
        "Résumé Ñandú Â",
        "Łódź ẞ",
        "Ёлка",
    ] {
        names.push(extra.to_owned());
    }
    let (spell, folded) = build(&names);
    let s = Spelling::open(NameArena::open(&folded).expect("fold"), &spell).expect("open");
    for (row, name) in names.iter().enumerate() {
        assert_eq!(
            s.cased(row).map(|b| String::from_utf8(b).expect("text")),
            Some(DefaultFolder.fold(name)),
            "{name:?}"
        );
    }
    assert_eq!(s.cased(names.len()), None);
}

#[test]
fn a_reader_answers_whatever_get_would_have_answered() {
    let names = corpus();
    let (spell, folded) = build(&names);
    let s = Spelling::open(NameArena::open(&folded).expect("fold"), &spell).expect("open");
    let n = names.len();
    let orders: Vec<Vec<usize>> = vec![
        (0..n).collect(),
        (0..n).rev().collect(),
        (0..n).step_by(32).collect(),
        (0..n).step_by(33).collect(),
        (0..n).step_by(5).collect(),
        (30..40).chain(62..72).chain((30..40).rev()).collect(),
        vec![7, 7, 7, n - 1, 0, n - 1, 0, 32, 31, 32],
        vec![5, n, 6, 4_000, 7],
    ];
    for order in orders {
        let mut r = Reader::default();
        for row in order {
            let want = s.get(row).unwrap_or_default();
            assert_eq!(r.at(&s, row), want, "row {row}");
        }
    }
}

#[test]
fn lowercase_names_cost_one_offset_a_block() {
    let names: Vec<String> = (0..320).map(|i| format!("dosya_{i}.txt")).collect();
    let (spell, _) = build(&names);
    assert_eq!(spell.len(), 8 + 11 * 4);
}

#[test]
fn a_spelling_as_long_as_its_fold_is_said_to_be_without_a_scan() {
    let names = corpus();
    let (spell, folded) = build(&names);
    let s = Spelling::open(NameArena::open(&folded).expect("fold"), &spell).expect("open");
    let mut r = Reader::default();
    let (mut long, mut short) = (0, 0);
    for (row, name) in names.iter().enumerate() {
        let fold = s.folded.get(row).expect("fold");
        // Capitals and nothing else: every byte the fold's, or 0x20 away.
        let flips = name.len() == fold.len()
            && name
                .bytes()
                .zip(fold.bytes())
                .all(|(a, b)| a == b || a ^ 0x20 == b);
        assert_eq!(s.is_as_long(row), flips, "{name:?}");
        if s.is_as_many_letters(row) {
            assert_eq!(name.chars().count(), fold.chars().count(), "{name:?}");
        }
        assert_eq!(
            r.folded(&s, row),
            Some((fold.as_bytes(), flips)),
            "{name:?}"
        );
        if flips { long += 1 } else { short += 1 }
    }
    assert!(long > 100 && short > 50, "{long} and {short}");
    assert!(!s.is_as_long(names.len()));
}

#[test]
fn a_capital_costs_a_bit_a_byte_and_a_turkish_one_two() {
    // The header, two offsets and the block's two bitmaps are 24 bytes.
    let size = |name: &str| build(&[name.to_owned()]).0.len() - 24;
    assert_eq!(size("README.md"), 2);
    assert_eq!(size("KENAN PİLEVNELİ.docx"), 6);
    assert_eq!(size("yazısı"), 2);
    assert_eq!(size("ŞIRNAK"), 2);
    assert_eq!(size("Öğüt Çağ.docx"), 4);
    // A letter no code names, or a fold of another length: stored whole.
    assert_eq!(size("ẞ.txt"), "ẞ.txt".len() + 1);
    assert_eq!(size("Łódź"), "Łódź".len() + 1);
}

/// What lets a search trust a row whose spelling is its fold or capitals of
/// it: no letter that lost a mark can hide behind those two codes.
#[test]
fn a_letter_that_lost_a_mark_is_never_stored_as_a_capital() {
    use scour_core::text::{DefaultFolder, Folder, Unaccented};
    let mut codes = Vec::new();
    for cp in 0x80..0x30000u32 {
        let Some(c) = char::from_u32(cp) else {
            continue;
        };
        let spelled = c.to_string();
        let folded = Unaccented.fold(&spelled);
        if folded == DefaultFolder.fold(&spelled) {
            continue;
        }
        let plain = align(spelled.as_bytes(), folded.as_bytes(), &mut codes)
            && codes.iter().all(|&code| code <= FLIPPED);
        assert!(!plain, "U+{cp:04X} {spelled:?} folds to {folded:?}");
    }
}

#[test]
fn a_file_cut_anywhere_is_refused_or_read_and_never_a_panic() {
    let names = corpus();
    let (spell, folded) = build(&names);
    let arena = NameArena::open(&folded).expect("fold");
    for cut in 0..spell.len() {
        let Some(s) = Spelling::open(arena, &spell[..cut]) else {
            continue;
        };
        assert!(!s.is_whole(), "cut at {cut} of {}", spell.len());
        let mut r = Reader::default();
        for row in 0..names.len() {
            let _ = s.get(row);
            let _ = r.at(&s, row);
        }
    }
    // Not for this fold: a row short.
    let (other, _) = build(&names[1..]);
    assert!(!Spelling::open(arena, &other).is_some_and(|s| s.is_whole()));
}

#[test]
fn bytes_that_are_not_a_spelling_are_read_without_a_panic() {
    let names = corpus();
    let (spell, folded) = build(&names);
    let arena = NameArena::open(&folded).expect("fold");
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    for _ in 0..200 {
        let mut bad = spell.clone();
        for _ in 0..8 {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let at = (state >> 33) as usize % bad.len();
            bad[at] = (state >> 13) as u8;
        }
        let Some(s) = Spelling::open(arena, &bad) else {
            continue;
        };
        let mut r = Reader::default();
        for row in 0..names.len() {
            let _ = s.get(row);
            let _ = r.at(&s, row);
        }
    }
}
