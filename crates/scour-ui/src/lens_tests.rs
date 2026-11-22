use super::*;

#[test]
fn terms_keep_quotes_and_split_on_or() {
    let q = r#"(rapor | fatura) under:"/home/u/My Files" !ext:pdf"#;
    let ts = terms(q);
    let raw: Vec<&str> = ts.iter().map(|t| &q[t.start..t.end]).collect();
    assert_eq!(
        raw,
        [
            "(rapor",
            "fatura)",
            r#"under:"/home/u/My Files""#,
            "!ext:pdf"
        ]
    );
    assert_eq!(ts[2].field, Some("under"));
    assert_eq!(ts[2].value, "/home/u/My Files");
    assert!(ts[3].not);
    assert_eq!(ts[3].value, "pdf");
}

#[test]
fn the_caret_belongs_to_the_term_it_ends() {
    let q = "rapor dm:7d";
    assert_eq!(term_at(q, 11).unwrap().field, Some("dm"));
    assert_eq!(term_at(q, 5).unwrap().value, "rapor");
    assert!(term_at(q, 6).is_none());
    assert!(term_at(q, 0).is_none());
}

#[test]
fn aliases_open_the_same_lens() {
    for (q, lens) in [
        ("altinda:~/x", Lens::Place),
        ("parent:/srv", Lens::Place),
        ("modified:7d", Lens::Time),
        ("tür:doc", Lens::Kind),
        ("ext:", Lens::Ext),
        ("içerik:\"a b\"", Lens::Content),
        ("rapor", Lens::Word),
        (":", Lens::Launch),
        (":de", Lens::Launch),
        ("sizE:>1mb", Lens::Size),
        ("ALTINDA:/srv", Lens::Place),
    ] {
        let t = term_at(q, q.len()).unwrap();
        assert_eq!(lens_of(q, &t), Some(lens), "{q}");
    }
    for q in ["perm:x", "a", "10:30"] {
        let t = term_at(q, q.len()).unwrap();
        assert_eq!(lens_of(q, &t), None, "{q}");
    }
}

#[test]
fn replacing_and_removing_a_term_keep_one_space() {
    let q = "rapor dm:7d ext:pdf";
    let t = term_at(q, 11).unwrap();
    assert_eq!(
        replace(q, &t, "dm:30d"),
        ("rapor dm:30d ext:pdf".into(), 12)
    );
    assert_eq!(replace(q, &t, ""), ("rapor ext:pdf".into(), 6));
    let last = term_at(q, q.len()).unwrap();
    assert_eq!(replace(q, &last, ""), ("rapor dm:7d".into(), 11));
    assert_eq!(without(q, &t), "rapor ext:pdf");
}

#[test]
fn a_list_value_is_toggled_and_unknown_ones_dropped() {
    let q = "ext:pdf;xy";
    let t = term_at(q, q.len()).unwrap();
    let values = [
        ("pdf".into(), ".pdf".into()),
        ("docx".into(), ".docx".into()),
    ];
    let c = list(&t, &values);
    assert_eq!(c[0].term, "ext:");
    assert_eq!(c[0].hint, "✓");
    assert_eq!(c[1].term, "ext:pdf;docx");
    assert!(c.iter().all(|c| c.stays));
}

#[test]
fn a_word_offers_its_readings_and_keeps_its_negation() {
    let q = "!sozlsme";
    let t = term_at(q, q.len()).unwrap();
    let terms: Vec<String> = word(&t).into_iter().map(|c| c.term).collect();
    assert_eq!(
        terms,
        [
            "!sozlsme",
            "!fuzzy:sozlsme",
            "!letters:sozlsme",
            "!content:\"sozlsme\"",
            "sozlsme"
        ]
    );
    let q = "\"kira süresi\"";
    let t = term_at(q, q.len()).unwrap();
    assert_eq!(word(&t)[0].term, q);
    assert_eq!(word(&t).len(), 3);
}

#[test]
fn bands_and_places_write_whole_terms() {
    let q = "!dm:";
    let t = term_at(q, q.len()).unwrap();
    assert_eq!(bands(&t, TIMES)[2].term, "!dm:7d");
    let q = "under:~/Bel";
    let t = term_at(q, q.len()).unwrap();
    assert_eq!(place(&t, "/home/u/My Files"), "under:\"/home/u/My Files\"");
    assert_eq!(place_flipped(&t).term, "parent:~/Bel");
    assert_eq!(place_parts("~/Bel"), ("~".into(), "Bel".into()));
    assert_eq!(place_parts("/"), ("/".into(), String::new()));
    assert_eq!(
        place_parts("~/Belgeler/"),
        ("~/Belgeler".into(), String::new())
    );
    assert_eq!(place_parts(""), (String::new(), String::new()));
}

#[test]
fn the_launcher_narrows_by_field_or_by_name() {
    let all = launch(":", |m| m.to_string());
    assert_eq!(all.len(), LAUNCH.len());
    let d = launch(":d", |m| m.to_string());
    assert_eq!(
        d.iter().map(|c| c.term.as_str()).collect::<Vec<_>>(),
        ["dm:"]
    );
    let tr = launch(":kon", |m| {
        if m == "Location" {
            "Konum".into()
        } else {
            m.into()
        }
    });
    assert_eq!(tr[0].term, "under:");
}

#[test]
fn a_field_one_letter_off_is_offered_its_spelling() {
    assert_eq!(near_field("siz"), Some("size"));
    assert_eq!(near_field("udner"), Some("under"));
    assert_eq!(near_field("extt"), Some("ext"));
    assert_eq!(near_field("size"), None);
    assert_eq!(near_field("zzzz"), None);
}
