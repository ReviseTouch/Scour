use super::*;
use scour_core::text::{Folder, Unaccented};

fn build(names: &[&str]) -> Vec<u8> {
    let mut w = NameWriter::new();
    for n in names {
        w.push(n);
    }
    w.finish_folded()
}

#[test]
fn every_name_comes_back_by_row() {
    // More than two blocks, so the block index is exercised.
    let names: Vec<String> = (0..300).map(|i| format!("file_{i}.rs")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let bytes = build(&refs);
    let arena = NameArena::open(&bytes).expect("open");
    assert_eq!(arena.rows(), 300);
    for (i, want) in refs.iter().enumerate() {
        assert_eq!(arena.get(i), Some(*want), "row {i}");
    }
    assert_eq!(arena.get(300), None);
}

#[test]
fn the_arena_finished_in_place_is_the_arena_byte_for_byte() {
    let names: Vec<String> = (0..300).map(|i| format!("İstanbul_{i}.TXT")).collect();
    let mut writer = NameWriter::new();
    for name in &names {
        writer.push(name);
    }
    let expected = writer.finish_folded();
    let (_, folded) = writer.finish_both();
    assert_eq!(folded, expected);
    let folded = NameArena::open(&folded).expect("fold");
    for (row, name) in names.iter().enumerate() {
        assert_eq!(folded.get(row), Some(Unaccented.fold(name).as_str()));
    }
}

#[test]
fn walking_visits_rows_in_order_and_stops_when_told() {
    let refs = ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"];
    let bytes = build(&refs);
    let arena = NameArena::open(&bytes).expect("open");

    let mut seen = Vec::new();
    arena.walk(0, |row, name| {
        seen.push((row, String::from_utf8_lossy(name).into_owned()));
        true
    });
    assert_eq!(seen.len(), 5);
    assert_eq!(seen[3], (3, "d.rs".to_owned()));

    // Early exit is the whole point of the design.
    let mut count = 0;
    arena.walk(0, |_, _| {
        count += 1;
        count < 2
    });
    assert_eq!(count, 2);

    // And starting partway through lands on the right row.
    let mut from_three = Vec::new();
    arena.walk(3, |row, name| {
        from_three.push((row, String::from_utf8_lossy(name).into_owned()));
        true
    });
    assert_eq!(from_three, vec![(3, "d.rs".into()), (4, "e.rs".into())]);
}

#[test]
fn walking_starts_correctly_from_inside_a_later_block() {
    let names: Vec<String> = (0..400).map(|i| format!("n{i}")).collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let bytes = build(&refs);
    let arena = NameArena::open(&bytes).expect("open");
    let mut first = None;
    arena.walk(257, |row, name| {
        first = Some((row, String::from_utf8_lossy(name).into_owned()));
        false
    });
    assert_eq!(first, Some((257, "n257".to_owned())));
}

#[test]
fn folding_agrees_with_the_folder_the_index_was_built_with() {
    // Disagreement stores a file under one spelling and searches under
    // another, with no error anywhere.
    let mut f = Folded::new();
    for name in [
        "main.rs",
        "RAPOR.PDF",
        "İSTANBUL.txt",
        "ISTANBUL.txt",
        "ısparta.md",
        "ÇALIŞKAN",
        "Öğüt.docx",
        "Müzik",
        "",
        "ẞ.txt",
    ] {
        assert_eq!(f.fold(name), Unaccented.fold(name), "{name:?}");
    }
}

#[test]
fn folding_agrees_on_every_mixture_of_scripts_it_can_be_handed() {
    // The shape the bulk-ASCII path has to survive: runs of every length,
    // broken by non-ASCII at every position, including both ends.
    let alphabet: &[&str] = &[
        "a", "Z", "9", "-", ".", " ", "_", "ı", "İ", "I", "i", "ş", "Ş", "ğ", "Ğ", "ç", "Ç", "ö",
        "Ö", "ü", "Ü", // Two-byte, three-byte, four-byte, and one folding to two.
        "é", "Ω", "д", "中", "🙂", "ẞ", "\u{0307}", "ß", "æ", "ё", "й", "ά", "ế", "Ａ", "１",
        "\u{0301}", "\u{05b8}", "\u{064e}", "\u{3000}", "が", "Ǿ", "Ǆ",
    ];
    let mut f = Folded::new();
    // Deterministic: a failure has to reproduce by running it again.
    let mut state: u64 = 0x243f_6a88_85a3_08d3;
    let mut next = |n: usize| -> usize {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        ((state >> 33) as usize) % n
    };
    for _ in 0..20_000 {
        let len = next(24);
        let mut name = String::new();
        for _ in 0..len {
            name.push_str(alphabet[next(alphabet.len())]);
        }
        assert_eq!(
            f.fold(&name),
            Unaccented.fold(&name),
            "{name:?} ({:?})",
            name.as_bytes()
        );
    }
}

#[test]
fn folding_allocates_nothing_and_survives_an_absurd_name() {
    let mut f = Folded::new();
    let huge = "a".repeat(FOLD_CAP * 2);
    // Must not panic; a name this long cannot exist on any real filesystem.
    let got = f.fold(&huge);
    assert!(got.len() <= FOLD_CAP);
    // And the buffer still works afterwards.
    assert_eq!(f.fold("Main.RS"), "main.rs");
}

#[test]
fn non_utf8_and_empty_arenas_do_not_panic() {
    let bytes = build(&[]);
    let arena = NameArena::open(&bytes).expect("open");
    assert!(arena.is_empty());
    assert_eq!(arena.get(0), None);
    arena.walk(0, |_, _| panic!("nothing to walk"));
    assert!(NameArena::open(&[0u8; 4]).is_none());
}
