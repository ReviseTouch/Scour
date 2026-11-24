use super::*;

fn one(q: &str) -> Vec<(bool, Match)> {
    parse_at(q, 0)
        .groups
        .into_iter()
        .flat_map(|g| g.alts)
        .collect()
}

fn exts(q: &str) -> Vec<String> {
    match one(q).into_iter().next() {
        Some((_, Match::Ext(e))) => e,
        other => panic!("{q} parsed to {other:?}"),
    }
}

#[test]
fn a_separator_may_be_written_with_spaces_around_it() {
    // The three spellings a person actually types, all one filter.
    let want = vec!["rs".to_owned(), "toml".to_owned()];
    assert_eq!(exts("ext:rs;toml"), want);
    assert_eq!(exts("ext:rs ; toml"), want);
    assert_eq!(exts("ext:rs; toml"), want);
    assert_eq!(exts("ext:rs ;toml"), want);
    assert_eq!(exts("ext:rs  ;  toml"), want, "and more than one space");
}

#[test]
fn a_longer_list_survives_the_same_treatment() {
    assert_eq!(
        exts("ext:rs ; toml ; md"),
        vec!["rs".to_owned(), "toml".to_owned(), "md".to_owned()]
    );
}

#[test]
fn the_rest_of_the_query_is_untouched() {
    let got = parse_at("rapor ext:rs ; toml dm:7d", 0);
    assert_eq!(got.groups.len(), 3, "name, extension, date");
}

#[test]
fn a_semicolon_between_words_is_and() {
    // Between terms `;` is a space typed without pressing space: both.
    // Inside a field's value it stays "any of these".
    let want = vec![
        vec![(false, Match::NameContains("rapor".into()))],
        vec![(false, Match::NameContains("pdf".into()))],
    ];
    for q in ["rapor ; pdf", "rapor;pdf", "rapor pdf"] {
        assert_eq!(
            parse_at(q, 0)
                .groups
                .iter()
                .map(|g| g.alts.clone())
                .collect::<Vec<_>>(),
            want,
            "{q:?}"
        );
    }
}

/// `|` is "either of these"; `;` is "and also".
#[test]
fn a_pipe_is_either_and_a_semicolon_is_both() {
    let either = |q: &str| {
        let ast = parse_at(q, 0);
        (ast.groups.len(), ast.groups[0].alts.len())
    };
    assert_eq!(either("hasan|genel"), (1, 2), "`|` is either of them");
    assert_eq!(either("hasan;genel"), (2, 1), "`;` is both of them");
    assert_eq!(either("hasan genel"), (2, 1), "and a space is the same");
    // Written with spaces around it, or without, or several at once.
    for q in ["a;b;c", "a ; b ; c", "a b c"] {
        assert_eq!(parse_at(q, 0).groups.len(), 3, "{q:?}");
    }
    // A field's own list is untouched: two values are not two terms.
    assert_eq!(
        one("ext:rs;toml"),
        vec![(false, Match::Ext(vec!["rs".into(), "toml".into()]))]
    );
    assert_eq!(parse_at("ext:rs;toml", 0).groups.len(), 1);
    // Beside a `|` it says nothing more: the operator decides how the
    // terms combine, the separator only where one ends.
    assert_eq!(either("a|;c"), (1, 2));
    assert_eq!(either("a | ;c"), (1, 2));
}

#[test]
fn a_separator_inside_quotes_is_an_ordinary_character() {
    assert_eq!(
        one("\"a ; b\""),
        vec![(false, Match::NameContains("a ; b".into()))]
    );
}

#[test]
fn trailing_and_inter_field_semicolons_do_not_corrupt_terms() {
    use scour_core::Kind;

    for q in [";", ";;", " ; ; ", " ; | ! "] {
        assert!(parse_at(q, 0).is_empty(), "{q:?}");
    }
    let doc_docx = vec![
        (false, Match::NameGlob("*.docx".into())),
        (false, Match::Kind(vec![Kind::Doc])),
    ];
    for q in [
        "*.docx kind:doc;",
        "*.docx kind:doc ;",
        "*.docx kind:doc ; ;",
        "*.docx;kind:doc;",
        "; *.docx ; kind:doc ;",
    ] {
        assert_eq!(one(q), doc_docx, "{q:?}");
    }
    assert_eq!(
        one("kind:doc ; *.docx"),
        vec![
            (false, Match::Kind(vec![Kind::Doc])),
            (false, Match::NameGlob("*.docx".into())),
        ]
    );
    assert_eq!(
        one("kind:doc;image ;"),
        vec![(false, Match::Kind(vec![Kind::Doc, Kind::Image]))]
    );
    assert_eq!(
        one("size:>1mb;dm:7d;"),
        vec![
            (false, Match::Size(Cmp::Gt, 1_048_576)),
            (
                false,
                Match::Time(TimeField::Modified, Cmp::Ge, -7 * 86_400)
            ),
        ]
    );
    assert_eq!(
        one("folder: ; *.rs ;"),
        vec![
            (false, Match::IsDir(true)),
            (false, Match::NameGlob("*.rs".into())),
        ]
    );
    assert_eq!(
        one("\"annual report\";ext:pdf;"),
        vec![
            (false, Match::NameContains("annual report".into())),
            (false, Match::Ext(vec!["pdf".into()])),
        ]
    );
    assert_eq!(
        one("ext:rs;!tmp;"),
        vec![
            (false, Match::Ext(vec!["rs".into()])),
            (true, Match::NameContains("tmp".into())),
        ]
    );
}
