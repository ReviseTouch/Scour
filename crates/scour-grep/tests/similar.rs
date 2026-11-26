//! `~word`: the word, or one with a letter off where a word begins, by the
//! name query's own rule of how many mistakes a word is forgiven.

mod support;

use scour_grep::{Budget, Expr, Ground, Medium, Needle, Options, Outcome};
use support::{put, utf16le};

/// What each find of `pattern` in a file holding `bytes` marks, or None when
/// the file does not answer it.
fn marked_in(pattern: &str, bytes: &[u8], exact_case: bool) -> Option<Vec<String>> {
    let expr = Expr::forgiving(
        &Needle::Text(pattern.into()),
        exact_case,
        scour_core::mistakes_allowed,
    )
    .unwrap();
    marked_by(&expr, bytes)
}

fn marked_by(expr: &Expr, bytes: &[u8]) -> Option<Vec<String>> {
    let dir = tempfile::tempdir().unwrap();
    let files = [put(dir.path(), "a.txt", bytes)];
    let ground = Ground::of([(dir.path().to_path_buf(), Some("disk".into()), Medium::Solid)]);
    let got = expr.scan(
        &Options::default(),
        &ground,
        None,
        None,
        &files,
        &Budget::unlimited(),
    );
    match &got[0] {
        Outcome::Found { hits, .. } => Some(
            hits.iter()
                .flat_map(|h| {
                    h.marks
                        .iter()
                        .map(|&(a, l)| h.text[a as usize..(a + l) as usize].to_owned())
                })
                .collect(),
        ),
        Outcome::Clean => None,
        other => panic!("{pattern:?}: {other:?}", pattern = expr),
    }
}

fn marked(pattern: &str, text: &str) -> Option<Vec<String>> {
    marked_in(pattern, text.as_bytes(), false)
}

#[test]
fn one_mistake_of_each_kind_is_forgiven() {
    assert_eq!(marked("~rapor", "bu rapar burada").unwrap(), ["rapar"]);
    assert_eq!(marked("~rapor", "bu rapr burada").unwrap(), ["rapr"]);
    assert_eq!(marked("~rapor", "bu rapxor burada").unwrap(), ["rapxor"]);
    assert_eq!(marked("~rapor", "bu rpaor burada").unwrap(), ["rpaor"]);
    assert_eq!(marked("~rapor", "bu apor burada").unwrap(), ["apor"]);
    assert_eq!(marked("~rapor", "(rapar)").unwrap(), ["rapar"]);
    assert_eq!(marked("~rapor", "rapar başta").unwrap(), ["rapar"]);
}

#[test]
fn two_mistakes_are_not_forgiven_a_word_of_ten_or_fewer() {
    assert_eq!(marked("~rapor", "bu rbpbr burada"), None);
    assert_eq!(marked("~rapor", "bu rpaxr burada"), None);
    assert_eq!(marked("~dilekçeler", "dilkçelr"), None);
}

#[test]
fn a_longer_word_is_forgiven_two_and_not_three() {
    assert_eq!(
        marked("~muhasebeciler", "eski muhasbecilr dosyası").unwrap(),
        ["muhasbecilr"]
    );
    assert_eq!(marked("~muhasebeciler", "eski mhasbecilr dosyası"), None);
}

#[test]
fn a_word_of_four_letters_is_forgiven_nothing() {
    assert_eq!(marked("~kira", "kora sözleşmesi"), None);
    assert_eq!(marked("~kira", "kira sözleşmesi").unwrap(), ["kira"]);
}

#[test]
fn spelled_right_it_is_found_anywhere_misspelt_only_where_a_word_begins() {
    assert_eq!(marked("~rapor", "xrapor").unwrap(), ["rapor"]);
    assert_eq!(marked("~rapor", "bu xrapar burada"), None);
    assert_eq!(marked("~rapor", "şrapar"), None);
    assert_eq!(marked("~rapor", "2026rapar").unwrap(), ["rapar"]);
    assert_eq!(marked("~rapor", "bir raporlar").unwrap(), ["rapor"]);
}

#[test]
fn a_similar_word_is_one_alternative_of_several() {
    assert_eq!(marked("~rapor|dilekçe", "bir rapar").unwrap(), ["rapar"]);
    assert_eq!(
        marked("~rapor|dilekçe", "bir dilekçe").unwrap(),
        ["dilekçe"]
    );
    assert_eq!(marked("~rapor|dilekçe", "bir dilekce"), None);
    assert_eq!(
        marked("(dilekçe | ~rapor)", "bir rapar").unwrap(),
        ["rapar"]
    );
}

#[test]
fn a_similar_word_can_be_ruled_out() {
    assert_eq!(marked("!~taslak", "taslak metin"), None);
    assert_eq!(marked("!~taslak", "taslk metin"), None);
    assert!(marked("!~taslak", "nihai metin").is_some());
    assert_eq!(marked("nihai !~taslak", "nihai taslık"), None);
    assert_eq!(marked("nihai !~taslak", "nihai metin").unwrap(), ["nihai"]);
}

#[test]
fn a_quoted_or_escaped_tilde_is_a_tilde() {
    assert_eq!(marked("\"~rapor\"", "bir rapar"), None);
    assert_eq!(marked("\"~rapor\"", "a~rapor").unwrap(), ["~rapor"]);
    assert_eq!(marked("\\~rapor", "bir rapar"), None);
    assert_eq!(marked("\\~rapor", "a~rapor").unwrap(), ["~rapor"]);
    assert_eq!(marked("\"~\"", "a ~ b").unwrap(), ["~"]);
    assert_eq!(marked("\\~", "a ~ b").unwrap(), ["~"]);
}

/// A bare `~` is a similar word not written yet, as a bare `!` is a word not
/// yet ruled out: the field shows it as a capsule waiting for its word, so it
/// looks for nothing.
#[test]
fn a_bare_tilde_looks_for_nothing() {
    let empty = Expr::new(&Needle::Text("~".into()), false);
    assert!(matches!(empty, Err(scour_grep::BadPattern::Empty)));
    assert_eq!(marked("~|yok", "a ~ b"), None);
    assert_eq!(marked("yok|~", "a ~ yok").unwrap(), ["yok"]);
    assert_eq!(marked("a ~", "a ~ b").unwrap(), ["a"]);
}

#[test]
fn case_and_the_turkish_i_fold_as_plain_text_does() {
    assert_eq!(
        marked("~istanbul", "İSTANBL ADLİYESİ").unwrap(),
        ["İSTANBL"]
    );
    assert_eq!(marked("~kırmızı", "KIRMIZ kalem").unwrap(), ["KIRMIZ"]);
    assert_eq!(marked("~DİLEKÇE", "dilekçr").unwrap(), ["dilekçr"]);
    let exact = |p: &str, t: &str| marked_in(p, t.as_bytes(), true);
    assert_eq!(exact("~istanbul", "İstanbl"), None);
    assert_eq!(exact("~istanbul", "istanbl").unwrap(), ["istanbl"]);
    assert_eq!(exact("~Rapor", "rapar"), None);
}

#[test]
fn every_word_of_the_line_is_marked_misspelt_or_not() {
    assert_eq!(
        marked("~rapor dilekçe", "bu rapar ve dilekçe ve rapr").unwrap(),
        ["rapar", "dilekçe", "rapr"]
    );
}

#[test]
fn a_phrase_after_the_tilde_is_forgiven_as_one() {
    assert_eq!(
        marked("~\"iş kazası\"", "bir iş kazsı davası").unwrap(),
        ["iş kazsı"]
    );
}

#[test]
fn utf16_text_is_forgiven_alike() {
    let mut bytes = vec![0xFF, 0xFE];
    bytes.extend(utf16le("bir rapar burada"));
    assert_eq!(marked_in("~rapor", &bytes, false).unwrap(), ["rapar"]);
}

#[test]
fn clauses_handed_over_read_the_tilde_alike() {
    let clauses = |piece: &str| {
        Expr::from_clauses(
            &[vec![piece.to_owned()]],
            &[],
            false,
            scour_core::mistakes_allowed,
        )
        .unwrap()
    };
    assert_eq!(
        marked_by(&clauses("~rapor"), b"bir rapar").unwrap(),
        ["rapar"]
    );
    assert_eq!(marked_by(&clauses("\\~rapor"), b"bir rapar"), None);
}

#[test]
fn without_a_rule_the_tilde_forgives_nothing() {
    let expr = Expr::new(&Needle::Text("~rapor".into()), false).unwrap();
    assert_eq!(marked_by(&expr, b"bir rapar"), None);
    assert_eq!(marked_by(&expr, b"bir rapor").unwrap(), ["rapor"]);
}

#[test]
fn the_blanks_inside_parentheses_are_not_looked_for() {
    assert_eq!(marked("(dilekçe | rapor)", "bir rapor").unwrap(), ["rapor"]);
    assert_eq!(
        marked("(dilekçe | rapor)", "bir dilekçe").unwrap(),
        ["dilekçe"]
    );
    assert_eq!(marked("(\"a b \" | x)", "a b c").unwrap(), ["a b "]);
}
