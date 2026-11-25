use super::*;
use crate::lens::{runs, term_at};

fn raw(q: &str) -> Vec<&str> {
    terms(q).iter().map(|t| &q[t.start..t.end]).collect()
}

/// Each line is what `scour explain` reads, checked against the service.
#[test]
fn a_list_goes_on_across_a_spaced_semicolon_as_the_service_reads_it() {
    for q in [
        "ext:pdf ; odt",
        "ext:pdf ;odt",
        "ext:pdf; odt",
        "ext:pdf ; ; odt",
        "ext:pdf; ;odt",
    ] {
        let ts = terms(q);
        assert_eq!(ts.len(), 1, "{q}");
        assert_eq!((ts[0].start, ts[0].end), (0, q.len()), "{q}");
        assert_eq!(ts[0].field, Some("ext"), "{q}");
        assert_eq!(
            ts[0]
                .value
                .split(';')
                .filter(|v| !v.is_empty())
                .collect::<Vec<_>>(),
            ["pdf", "odt"],
            "{q}"
        );
    }
    let t = &terms("tür:doc ; code")[0];
    assert_eq!((t.field, t.value.as_str()), (Some("kind"), "doc;code"));
    let t = &terms("!ext:pdf ; odt")[0];
    assert!(t.not);
    assert_eq!(t.value, "pdf;odt");
    assert_eq!(raw("ext:pdf;odt ; rs"), ["ext:pdf;odt ; rs"]);
}

#[test]
fn a_list_ends_where_its_next_piece_cannot_be_a_value() {
    assert_eq!(raw("ext:pdf odt"), ["ext:pdf", "odt"]);
    assert_eq!(raw("ext:pdf ; dm:7d"), ["ext:pdf ;", "dm:7d"]);
    assert_eq!(raw("ext:pdf;dm:7d"), ["ext:pdf", "dm:7d"]);
    assert_eq!(raw("kind:doc;!code"), ["kind:doc", "!code"]);
    assert_eq!(raw("ext:pdf ; !odt"), ["ext:pdf ;", "!odt"]);
    assert_eq!(raw("ext:pdf ; \"odt\""), ["ext:pdf ;", "\"odt\""]);
    assert_eq!(raw("ext:pdf ; odt ; dm:7d"), ["ext:pdf ; odt ;", "dm:7d"]);
    assert_eq!(raw("ext: ; odt"), ["ext: ; odt"]);
    assert_eq!(terms("ext:pdf ; dm:7d")[1].field, Some("dm"));
}

#[test]
fn a_bar_still_cuts_and_a_list_after_one_is_the_service_s() {
    let q = "ext:pdf ; odt|rs";
    assert_eq!(raw(q), ["ext:pdf ; odt", "rs"]);
    assert_eq!(runs(q).len(), 1, "one run: (pdf or odt) or rs");
    // `x|ext:pdf` is no list to the service: `odt` is a word of its own.
    assert_eq!(raw("x|ext:pdf ; odt"), ["x", "ext:pdf", "odt"]);
    assert_eq!(raw("x | ext:pdf ; odt"), ["x", "ext:pdf ; odt"]);
    assert_eq!(raw("ext:pdf|x ; odt"), ["ext:pdf", "x", "odt"]);
}

#[test]
fn a_lone_semicolon_is_a_separator_and_other_words_are_as_they_were() {
    assert_eq!(raw("rapor ; fatura"), ["rapor", "fatura"]);
    assert_eq!(raw("under:/a ; under:/b"), ["under:/a", "under:/b"]);
    assert_eq!(raw("rapor;fatura"), ["rapor", "fatura"]);
    assert_eq!(raw("under:/a;b"), ["under:/a", "b"]);
    assert_eq!(
        raw(r#"under:"/a;b" "x;y""#),
        [r#"under:"/a;b""#, r#""x;y""#]
    );
    assert_eq!(raw("under:/a;"), ["under:/a"]);
    assert_eq!(
        raw(r#"a|!b "x ; y" under:"/m y""#),
        ["a", "!b", "\"x ; y\"", "under:\"/m y\""]
    );
    assert!(terms("").is_empty() && terms(" ; ").is_empty());
}

/// Each case as `scour explain` reads it: `;` outside a list is a space.
#[test]
fn a_semicolon_outside_a_list_cuts_as_the_service_cuts() {
    assert_eq!(raw("under:/a;b|under:/c"), ["under:/a", "b", "under:/c"]);
    let r: Vec<usize> = runs("under:/a;b|under:/c").iter().map(Vec::len).collect();
    assert_eq!(r, [1, 2], "under a, and (b or under c)");
    assert_eq!(raw("dm:7d;rapor"), ["dm:7d", "rapor"]);
    assert_eq!(raw("fuzzy:ab;cd"), ["fuzzy:ab", "cd"]);
    assert_eq!(
        raw("ext:pdf ; odt;rapor x;y"),
        ["ext:pdf ; odt;rapor", "x", "y"]
    );
    assert_eq!(raw("ext:pdf;dm:7d;x"), ["ext:pdf", "dm:7d", "x"]);
    assert_eq!(terms("rapor;fatura")[1].start, 6);
}

#[test]
fn the_caret_in_a_spaced_list_is_in_that_list() {
    let q = "rapor ext:pdf ; odt";
    for c in [13, 14, 15, 19] {
        assert_eq!(term_at(q, c).unwrap().field, Some("ext"), "{c}");
    }
}
