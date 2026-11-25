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

/// The launcher's labels as the Turkish catalogue says them.
fn turkish(m: &str) -> String {
    match m {
        "Location" => "Konum",
        "Modified" => "Değişme",
        "Type" => "Tür",
        "Extension" => "Uzantı",
        "Size" => "Boyut",
        "Similar spelling" => "Benzer yazım",
        "Inside" => "İçinde",
        other => other,
    }
    .into()
}

#[test]
fn the_launcher_narrows_by_field_or_by_name() {
    let all = launch("", ":", |m| m.to_string());
    assert_eq!(all.len(), LAUNCH.len());
    let d = launch("", ":d", |m| m.to_string());
    assert_eq!(
        d.iter().map(|c| c.choice.term.as_str()).collect::<Vec<_>>(),
        ["dm:"]
    );
    let tr = launch("", ":kon", turkish);
    assert_eq!(tr[0].choice.term, "under:");
    // A capital İ is the i a person types.
    let tr = launch("", ":iç", turkish);
    assert_eq!(tr.len(), 1);
    assert_eq!(tr[0].choice.term, "content:");
}

#[test]
fn the_launcher_lists_narrowing_first_then_how_it_searches() {
    let all = launch("", ":", |m| m.to_string());
    let fields: Vec<&str> = all.iter().map(|s| s.choice.term.as_str()).collect();
    assert_eq!(
        fields,
        [
            "under:", "dm:", "kind:", "ext:", "size:", "fuzzy:", "content:"
        ]
    );
    let groups: Vec<Group> = all.iter().map(|s| s.group).collect();
    assert_eq!(groups[..5], [Group::Narrow; 5]);
    assert_eq!(groups[5..], [Group::How; 2]);
    let icons: Vec<&str> = all.iter().map(|s| s.icon).collect();
    assert_eq!(
        icons,
        ["folder", "clock", "shapes", "ext", "size", "approx", "read"]
    );
    assert_eq!(Group::Narrow.heading(), "Narrow");
    assert_eq!(filter_of("parent").map(|f| f.icon), Some("folder"));
}

#[test]
fn a_shortcut_is_the_fewest_letters_that_leave_its_line_alone() {
    let keys = |name: fn(&str) -> String| -> Vec<String> {
        launch("", ":", name).into_iter().map(|s| s.key).collect()
    };
    assert_eq!(
        keys(|m| m.to_string()),
        [":l", ":m", ":t", ":e", ":siz", ":f", ":i"]
    );
    // `k` is Konum and `kind:` both, `u` is `under:` and Uzantı both.
    assert_eq!(keys(turkish), [":ko", ":d", ":t", ":e", ":s", ":f", ":i"]);
    for (s, key) in launch("", ":", turkish).iter().zip(keys(turkish)) {
        let only = launch("", &key, turkish);
        assert_eq!(only.len(), 1, "{key}");
        assert_eq!(only[0].choice.term, s.choice.term, "{key}");
    }
}

#[test]
fn a_filter_the_query_holds_is_marked_whatever_its_spelling() {
    let held_fields = |q: &str| -> Vec<String> {
        launch(q, ":", |m| m.to_string())
            .into_iter()
            .filter(|s| s.held)
            .map(|s| s.choice.term)
            .collect()
    };
    assert!(held_fields("rapor :").is_empty());
    assert_eq!(held_fields("rapor !ext:pdf :"), ["ext:"]);
    assert_eq!(held_fields("tür:doc : rapor"), ["kind:"]);
    assert_eq!(held_fields("parent:/srv :"), ["under:"]);
    assert_eq!(held_fields("İÇERİK:\"a b\" dm:7d"), ["dm:", "content:"]);
    // The launcher's own letters are not a field.
    assert!(held_fields(":kind").is_empty());
    // A second forgiving word can always be started.
    assert!(held_fields("fuzzy:rapro :").is_empty());
}

#[test]
fn choosing_a_held_filter_takes_the_caret_into_it() {
    let q = "rapor !ext:pdf :";
    let colon = term_at(q, q.len()).unwrap();
    assert_eq!(
        into_held(q, &colon, "ext"),
        Some(("rapor !ext:pdf".into(), 14))
    );
    let q = "rapor :ty tür:doc;code dm:7d";
    let colon = term_at(q, 9).unwrap();
    let (text, caret) = into_held(q, &colon, "kind").unwrap();
    assert_eq!(text, "rapor tür:doc;code dm:7d");
    assert_eq!(&text[..caret], "rapor tür:doc;code");
    assert_eq!(term_at(&text, caret).unwrap().field, Some("kind"));
    assert_eq!(
        into_held("rapor :", &term_at("rapor :", 7).unwrap(), "dm"),
        None
    );
}

#[test]
fn a_field_one_letter_off_is_offered_its_spelling() {
    assert_eq!(near_field("siz"), Some("size"));
    assert_eq!(near_field("udner"), Some("under"));
    assert_eq!(near_field("extt"), Some("ext"));
    assert_eq!(near_field("size"), None);
    assert_eq!(near_field("zzzz"), None);
}

#[test]
fn only_a_content_term_with_something_in_it_reads_files() {
    assert!(reads("rapor content:fatura"));
    assert!(reads("content:\"iki kelime\""));
    assert!(!reads("rapor content:\"\""));
    assert!(!reads("rapor content:"));
    assert!(!reads("rapor"));
}

#[test]
fn a_value_a_bar_or_semicolon_would_cut_is_written_quoted() {
    let q = r#""x;y""#;
    let t = &terms(q)[0];
    let written: Vec<String> = word(t).into_iter().map(|c| c.term).collect();
    assert_eq!(written, [r#""x;y""#, r#"content:"x;y""#, r#"!"x;y""#]);
    let q = r#"under:"/a|b""#;
    assert_eq!(place_flipped(&terms(q)[0]).term, r#"parent:"/a|b""#);
}
