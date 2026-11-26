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
fn every_piece_but_the_edited_one_is_a_capsule() {
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
        ]
    );
    // The caret's piece stays letters, its `!` in its own colour.
    assert_eq!(
        l.runs,
        vec![
            Run {
                text: "!".into(),
                x: l.xs[32],
                role: NOT_OP
            },
            Run {
                text: "taslak".into(),
                x: l.xs[33],
                role: PLAIN
            },
        ]
    );
    // "and" between capsules, a plain space before the letters.
    let ands = l.bits.iter().filter(|b| b.text == "and").count();
    assert_eq!(ands, 3);
    assert!((l.xs[32] - l.xs[31] - M.adv).abs() < 0.01);
}

#[test]
fn the_caret_in_a_piece_gives_it_back_its_letters() {
    // In `~rapor`: the tilde is an operator there, and the rest are capsules.
    let l = lay(SAMPLE, 22, true, M, &same);
    let heads: Vec<String> = l.caps.iter().map(|c| c.head.clone()).collect();
    assert_eq!(heads, ["", "exact phrase", "either", "without"]);
    let r: Vec<(&str, i32)> = l.runs.iter().map(|r| (r.text.as_str(), r.role)).collect();
    assert_eq!(r, [("~", TILDE), ("rapor", PLAIN)]);
    // The letters are as wide as letters: the caret walks them one by one.
    for c in 20..26 {
        assert!((l.xs[c + 1] - l.xs[c] - M.adv).abs() < 0.01);
    }
    let not = l.caps.last().unwrap();
    assert!(not.not);
    assert_eq!(read(&l)[3].1, s(&["taslak"]));
}

#[test]
fn a_press_on_a_capsule_lands_at_its_end_and_between_on_a_letter() {
    let l = lay(SAMPLE, cols(SAMPLE), true, M, &same);
    let phrase = &l.caps[1];
    assert_eq!(hit(&l, phrase.x + phrase.w / 2.0), 19);
    // Past everything: the end.
    assert_eq!(hit(&l, 1e6), cols(SAMPLE));
    // Inside the letters: the nearest edge.
    let t = l.xs[35];
    assert_eq!(hit(&l, t + 2.0), 35);
    // Columns never go back.
    assert!(l.xs.windows(2).all(|w| w[1] >= w[0]));
}

#[test]
fn the_pointer_finds_the_alternative_it_is_on() {
    let l = lay("a|~b x", 6, true, M, &same);
    let c = &l.caps[0];
    let (a0, a1) = (c.alts[0], c.alts[1]);
    assert_eq!(over(&l, (a0.0 + a0.1) / 2.0), Some((0, 0)));
    assert_eq!(over(&l, (a1.0 + a1.1) / 2.0), Some((0, 1)));
    assert_eq!(over(&l, l.xs[6] + 5.0), None);
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
    assert_eq!(quiet, ["ya da", "ve"]);
    assert_eq!(l.caps[0].head, "ya");
    // A regex is letters, every one.
    let r = lay("a|b c", 5, false, M, &same);
    assert!(r.caps.is_empty());
    assert!((r.xs[5] - 5.0 * M.adv).abs() < 0.01);
}
