use super::*;

/// The fewest edits between the term and a part of the name that begins
/// where one of its words begins, by the table and nothing else: every such
/// start tried, the name's characters before it never paid for.
fn from_words(term: &str, name: &str) -> usize {
    let t: Vec<char> = term.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let mut best = usize::MAX;
    for start in (0..n.len()).filter(|&j| word_starts(&n, j)) {
        let n = &n[start..];
        let mut d = vec![vec![0usize; n.len() + 1]; t.len() + 1];
        for (i, row) in d.iter_mut().enumerate() {
            row[0] = i;
        }
        for cell in d[0].iter_mut().skip(1) {
            *cell = usize::MAX / 2;
        }
        for i in 1..=t.len() {
            for j in 1..=n.len() {
                let cost = usize::from(t[i - 1] != n[j - 1]);
                let mut b = (d[i - 1][j] + 1)
                    .min(d[i][j - 1] + 1)
                    .min(d[i - 1][j - 1] + cost);
                if i > 1 && j > 1 && t[i - 1] == n[j - 2] && t[i - 2] == n[j - 1] {
                    b = b.min(d[i - 2][j - 2] + 1);
                }
                d[i][j] = b;
            }
        }
        best = best.min(d[t.len()].iter().copied().min().unwrap_or(usize::MAX));
    }
    best
}

#[test]
fn a_mistake_is_forgiven_only_where_a_word_begins() {
    let near = |term: &str, rule| Near::new(term, 1, rule).expect("fits");
    // The noise one mistake anywhere let in, for `salih`. (`halihazir` is
    // not noise by this rule: `halih` is a letter off, where a word begins.)
    for name in ["universal_hash", "neslihan", "3lmpslihunqk"] {
        assert!(
            !near("salih", Rule::WordStart).found_in(name.as_bytes()),
            "{name}"
        );
    }
    for name in ["hasan salihoglu", "sarihular.doc", "eski_salh", "2026salik"] {
        assert!(
            near("salih", Rule::WordStart).found_in(name.as_bytes()),
            "{name}"
        );
    }
    // Spelled right, it is found anywhere, as a word without forgiveness is.
    assert!(near("salih", Rule::WordStart).found_in(b"musasalihx"));
    // A mistake early in the word: forgiven from a word's start, not strictly.
    assert!(near("hassan", Rule::WordStart).found_in(b"hasan yaman"));
    assert!(!near("hassan", Rule::Strict).found_in(b"hasan yaman"));
    assert!(near("hassan", Rule::Strict).found_in(b"hassn yaman"));
}

/// The strict rule's promise: with every word as the engine reads it — four
/// letters or fewer spelled exactly, longer ones forgiven by their length —
/// a word with one more letter never finds a name the shorter did not.
#[test]
fn strictly_a_longer_word_never_finds_more() {
    let alphabet = ['a', 'b', 'r', 's', 'ş', ' ', '-', '1'];
    let mut state: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = |n: usize| -> usize {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        ((state >> 33) as usize) % n
    };
    let found = |term: &str, name: &str| match scour_core::mistakes_allowed(term) {
        0 => name.contains(term),
        e => Near::new(term, e, Rule::Strict)
            .expect("fits")
            .found_in(name.as_bytes()),
    };
    let mut checked = 0;
    for _ in 0..20_000 {
        let term: String = (0..next(12) + 1).map(|_| alphabet[next(5)]).collect();
        let longer = format!("{term}{}", alphabet[next(5)]);
        let name: String = (0..next(30) + 1)
            .map(|_| alphabet[next(alphabet.len())])
            .collect();
        if found(&longer, &name) {
            checked += 1;
            assert!(
                found(&term, &name),
                "{longer:?} found {name:?}, {term:?} did not"
            );
        }
    }
    assert!(checked > 200, "{checked}");
}

#[test]
fn a_wrong_a_missing_an_extra_and_a_swapped_letter_are_one_mistake_each() {
    let near = |term: &str, edits| Near::new(term, edits, Rule::WordStart).expect("fits");
    for name in [
        "rapor.pdf",
        "2026-rapot.pdf",
        "rpor",
        "rappor",
        "raopr",
        "eski_raporu",
    ] {
        assert!(near("rapor", 1).found_in(name.as_bytes()), "{name}");
    }
    for name in ["rpr.pdf", "roapr2", "tabor", ""] {
        assert!(!near("rapor", 1).found_in(name.as_bytes()), "{name}");
    }
    assert!(near("muhasebe", 2).found_in(b"muhsaebe-2026.xlsx"));
    assert!(!near("muhasebe", 1).found_in(b"muhsaebi-2026.xlsx"));
    assert!(near("rapor", 0).found_in(b"rapor"));
    assert!(!near("rapor", 0).found_in(b"rapo"));
    assert!(Near::new("", 1, Rule::WordStart).is_none());
    assert!(Near::new(&"a".repeat(65), 2, Rule::WordStart).is_none());
    assert!(near(&"a".repeat(64), 2).found_in("a".repeat(62).as_bytes()));
}

#[test]
fn narrowing_never_loses_a_name_the_columns_would_find() {
    use crate::trigram::TrigramWriter;
    use scour_core::text::{Folder, Unaccented};
    let words = [
        "rapor", "raporu", "sirket", "muhasebe", "istanbul", "дом", "ёлка", "ab",
    ];
    let mut state: u64 = 0x1234_5678_9abc_def1;
    let mut next = |n: usize| -> usize {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        ((state >> 33) as usize) % n
    };
    // Names that are a word with a mistake or two in it, among others.
    let mut names: Vec<String> = Vec::new();
    for i in 0..40_000 {
        // Most names are noise: a word in every block narrows nothing.
        if i % 16 != 0 {
            let noise: String = (0..9).map(|_| (b'a' + next(26) as u8) as char).collect();
            names.push(format!("{i}-{noise}.txt"));
            continue;
        }
        let mut name: Vec<char> = words[next(words.len())].chars().collect();
        for _ in 0..next(3) {
            let at = next(name.len());
            match next(4) {
                0 => name[at] = 'x',
                1 => name.insert(at, 'q'),
                2 if name.len() > 1 => {
                    name.remove(at);
                }
                _ if at + 1 < name.len() => name.swap(at, at + 1),
                _ => {}
            }
        }
        names.push(format!("{i}-{}.txt", name.into_iter().collect::<String>()));
    }
    let mut writer = TrigramWriter::new();
    for name in &names {
        writer.push(name.as_bytes());
    }
    let (dict, post) = writer.finish();
    let tri = TrigramIndex::open(&dict, &post).expect("open");
    let (mut narrowed, mut found) = (0, 0);
    for term in [
        "rapor",
        "raporu",
        "sirket",
        "muhasebe",
        "istanbul",
        "ёлкаа",
        "muhasebeci",
    ] {
        for edits in 1..=2u8 {
            let near = Near::new(&Unaccented.fold(term), edits, Rule::WordStart).expect("fits");
            let Some(blocks) = near.blocks(&tri) else {
                continue;
            };
            narrowed += 1;
            for (row, name) in names.iter().enumerate() {
                if near.found_in(Unaccented.fold(name).as_bytes()) {
                    found += 1;
                    let block = (row / crate::columns::BLOCK) as u32;
                    assert!(blocks.contains(&block), "{term} ~{edits} lost {name}");
                }
            }
        }
    }
    assert!(narrowed >= 6 && found > 1_000, "{narrowed} {found}");
}

#[test]
fn the_bit_columns_agree_with_the_table_on_every_mixture() {
    let alphabet = ["a", "b", "c", "r", "ş", "д", "中", "🙂", ".", "o"];
    let mut state: u64 = 0x243f_6a88_85a3_08d3;
    let mut next = |n: usize| -> usize {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        ((state >> 33) as usize) % n
    };
    let word = |next: &mut dyn FnMut(usize) -> usize, most: usize| -> String {
        (0..next(most) + 1)
            .map(|_| alphabet[next(alphabet.len())])
            .collect()
    };
    let mut found = [0usize; 2];
    for _ in 0..40_000 {
        let term = word(&mut next, 9);
        let name = word(&mut next, 24);
        let want = from_words(&term, &name);
        for edits in 0..=3u8 {
            let got = Near::new(&term, edits, Rule::WordStart)
                .expect("fits")
                .found_in(name.as_bytes());
            assert_eq!(
                got,
                name.contains(term.as_str()) || want <= usize::from(edits),
                "{term:?} in {name:?}, {edits}"
            );
            found[usize::from(got)] += 1;
        }
    }
    assert!(found[0] > 10_000 && found[1] > 10_000, "{found:?}");
}

#[test]
fn bytes_that_are_not_text_are_read_without_a_panic() {
    let near = Near::new("rapor", 1, Rule::WordStart).expect("fits");
    assert!(near.found_in(b"\xff\xfe rapo\xc3"));
    assert!(!near.found_in(b"\xe4\xb8"));
    assert!(!Letters::new("ab").found_in(b"\xff\xfea"));
}

#[test]
fn letters_are_found_in_order_with_anything_between() {
    for (term, name, want) in [
        ("scrgui", "scour-gui", true),
        ("scrgui", "scour-tui", false),
        ("sg", "gs", false),
        ("aa", "a", false),
        ("aa", "aba", true),
        ("", "anything", true),
        ("şk", "şirket", true),
        // The second byte of `ş` is not a letter of its own.
        ("\u{15f}", "\u{11f}", false),
        ("中文", "中x文", true),
    ] {
        assert_eq!(
            Letters::new(term).found_in(name.as_bytes()),
            want,
            "{term} {name}"
        );
    }
}
