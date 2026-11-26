use super::*;

const M: Metrics = Metrics {
    adv: 10.0,
    small: 6.0,
};
const SAMPLE: &str = r#"icra "dava dosyası" ~rapor a|~b !taslak"#;

fn same(s: &str) -> String {
    s.to_string()
}

fn cols(s: &str) -> usize {
    s.chars().count()
}

/// What each capsule says, in order: its head and the text of its bits.
fn read(l: &Lay) -> Vec<(String, Vec<String>)> {
    l.caps
        .iter()
        .map(|c| {
            let inside = l
                .bits
                .iter()
                .filter(|b| b.x >= c.x && b.x < c.x + c.w && b.kind != CHIP)
                .map(|b| b.text.clone())
                .collect();
            (c.head.clone(), inside)
        })
        .collect()
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|t| t.to_string()).collect()
}

#[test]
fn every_piece_is_a_capsule_the_one_being_written_too() {
    let l = lay(SAMPLE, cols(SAMPLE), true, M, &same);
    assert_eq!(
        read(&l),
        vec![
            (String::new(), s(&["icra"])),
            (
                "exact phrase".into(),
                s(&["\u{201c}", "dava dosyası", "\u{201d}"])
            ),
            ("similar".into(), s(&["rapor"])),
            ("either".into(), s(&["a", "or", "≈", "b"])),
            ("without".into(), s(&["taslak"])),
        ]
    );
    // No letters outside a capsule, and the caret's one is ringed.
    assert!(l.runs.is_empty());
    let live: Vec<bool> = l.caps.iter().map(|c| c.live).collect();
    assert_eq!(live, [false, false, false, false, true]);
    let ands = l.bits.iter().filter(|b| b.text == "and").count();
    assert_eq!(ands, 4);
}

#[test]
fn nothing_moves_when_the_caret_comes_or_goes() {
    let n = cols(SAMPLE);
    let at = |c: usize| lay(SAMPLE, c, true, M, &same);
    let (end, start, inside) = (at(n), at(0), at(22));
    let boxes = |l: &Lay| l.caps.iter().map(|c| (c.x, c.w)).collect::<Vec<_>>();
    assert_eq!(boxes(&end), boxes(&start));
    assert_eq!(boxes(&end), boxes(&inside));
    assert_eq!(end.xs, start.xs);
    assert_eq!(end.xs, inside.xs);
    // Columns never go back.
    assert!(end.xs.windows(2).all(|w| w[1] >= w[0]));
}

#[test]
fn the_caret_walks_the_letters_and_an_operator_is_its_art() {
    let l = lay(SAMPLE, 33, true, M, &same);
    let not = l.caps.last().unwrap();
    assert!(not.not && not.live);
    // `!` stands at the head's start, the word after the head, a letter apart.
    assert!((l.xs[32] - (not.x + 2.0)).abs() < 0.01);
    assert!(l.xs[33] > not.x + not.hw);
    for c in 33..39 {
        assert!((l.xs[c + 1] - l.xs[c] - M.adv).abs() < 0.01);
    }
    // The phrase's quotes are its marks: inside them, the words.
    let phrase = &l.caps[1];
    assert!(l.xs[5] >= phrase.x + phrase.hw);
    assert!((l.xs[7] - l.xs[6] - M.adv).abs() < 0.01);
    assert!(l.xs[19] <= phrase.x + phrase.w);
}

#[test]
fn an_operator_opens_a_capsule_with_room_for_its_word() {
    // `!` after a word: a "without" capsule, the caret in its slot.
    let l = lay("ara !", 5, true, M, &same);
    assert_eq!(l.caps.len(), 2);
    let not = &l.caps[1];
    assert_eq!(not.head, "without");
    assert!(not.live && not.not);
    assert!(l.xs[5] > not.x + not.hw && l.xs[5] < not.x + not.w);
    assert!(l.bits.iter().any(|b| b.kind == SLOT));
    // `;` after a word: "either", the word and an empty inner capsule.
    let l = lay("ara;", 4, true, M, &same);
    assert_eq!(read(&l), vec![("either".into(), s(&["ara", "or", ""]))]);
    let hole = l.caps[0].alts[1];
    assert!(l.xs[4] > hole.0 && l.xs[4] < hole.1);
    // Two quotes: the caret between the marks.
    let l = lay("\"\"", 1, true, M, &same);
    assert_eq!(read(&l)[0].0, "exact phrase");
    let marks: Vec<f32> = l
        .bits
        .iter()
        .filter(|b| b.kind == MARK)
        .map(|b| b.x)
        .collect();
    assert!(marks.len() == 2 && l.xs[1] > marks[0] && l.xs[1] < marks[1]);
}

#[test]
fn the_and_keeps_its_room_and_is_written_when_the_caret_starts_a_piece() {
    let wrote = |l: &Lay| l.bits.iter().any(|b| b.text == "and");
    let typing = lay("ara ", 4, true, M, &same);
    let away = lay("ara ", 1, true, M, &same);
    assert!(wrote(&typing));
    assert!(!wrote(&away));
    assert_eq!(typing.xs, away.xs);
    // The next piece starts where the caret was.
    let next = lay("ara x", 5, true, M, &same);
    assert!((next.caps[1].x - typing.xs[4]).abs() < M.adv);
}

#[test]
fn a_press_lands_on_the_nearest_column_in_a_capsule_too() {
    let l = lay(SAMPLE, cols(SAMPLE), true, M, &same);
    let phrase = &l.caps[1];
    let c = hit(&l, phrase.x + phrase.w / 2.0);
    assert!((7..18).contains(&c), "{c}");
    // Past everything: the end.
    assert_eq!(hit(&l, 1e6), cols(SAMPLE));
    // Inside the letters: the nearest edge.
    let t = l.xs[35];
    assert_eq!(hit(&l, t + 2.0), 35);
}

#[test]
fn the_pointer_finds_the_alternative_it_is_on() {
    let l = lay("a|~b x", 6, true, M, &same);
    let c = &l.caps[0];
    let (a0, a1) = (c.alts[0], c.alts[1]);
    assert_eq!(over(&l, (a0.0 + a0.1) / 2.0), Some((0, 0)));
    assert_eq!(over(&l, (a1.0 + a1.1) / 2.0), Some((0, 1)));
    let x = &l.caps[1];
    assert_eq!(over(&l, x.x + x.w / 2.0), Some((1, 0)));
    assert_eq!(over(&l, x.x + x.w + 5.0), None);
}

#[test]
fn words_are_translated_and_other_modes_are_letters() {
    let tr = |m: &str| match m {
        "and" => "ve".to_string(),
        "either" => "ya".to_string(),
        "or" => "ya da".to_string(),
        m => m.to_string(),
    };
    let l = lay("x a|b y", 0, true, M, &tr);
    let quiet: Vec<&str> = l
        .bits
        .iter()
        .filter(|b| b.kind == QUIET)
        .map(|b| b.text.as_str())
        .collect();
    assert_eq!(quiet, ["ve", "ya da", "ve"]);
    assert_eq!(l.caps[1].head, "ya");
    // A regex is letters, every one.
    let r = lay("a|b c", 5, false, M, &same);
    assert!(r.caps.is_empty());
    assert!((r.xs[5] - 5.0 * M.adv).abs() < 0.01);
}
