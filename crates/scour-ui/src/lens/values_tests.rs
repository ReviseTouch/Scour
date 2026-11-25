use super::*;
use crate::lens::term_at;

fn toggle(q: &str, field: &str, v: &str) -> String {
    value_toggle(q, field, v, false).0
}

fn exclude(q: &str, field: &str, v: &str) -> String {
    value_toggle(q, field, v, true).0
}

#[test]
fn the_tiles_are_the_kinds_in_their_order_with_the_core_s_msgids() {
    let tokens: Vec<&str> = KIND_LABELS.iter().map(|(k, _)| *k).collect();
    assert_eq!(tokens, KINDS);
    assert_eq!(kind_label("doc"), Some("Document"));
    assert_eq!(kind_label("build"), Some("Build output"));
    assert_eq!(kind_label("media"), None);
}

#[test]
fn a_value_is_keyed_as_the_facet_keys_it() {
    assert_eq!(value_key("ext", ".PDF"), "pdf");
    assert_eq!(value_key("kind", "belge"), "doc");
    assert_eq!(value_key("kind", "RESİM"), "image");
    assert_eq!(value_key("kind", "çıktı"), "build");
    assert_eq!(value_key("kind", "media"), "media");
    assert_eq!(value_key("kind", "Code"), "code");
}

#[test]
fn toggling_a_value_puts_it_in_or_takes_it_out_of_the_one_term() {
    let q = toggle("rapor", "kind", "doc");
    assert_eq!(q, "rapor kind:doc");
    let q = toggle(&q, "kind", "code");
    assert_eq!(q, "rapor kind:doc;code");
    assert_eq!(toggle(&q, "kind", "doc"), "rapor kind:code");
    assert_eq!(toggle("rapor kind:doc", "kind", "doc"), "rapor");
    assert_eq!(toggle("kind:doc rapor", "kind", "doc"), "rapor");
    assert_eq!(toggle("", "ext", "pdf"), "ext:pdf");
    assert_eq!(toggle("rapor kind:", "kind", "doc"), "rapor kind:doc");
    let (q, caret) = value_toggle("kind:doc rapor", "kind", "code", false);
    assert_eq!((&q[..caret], &q[caret..]), ("kind:doc;code", " rapor"));
}

/// A value the facet did not return — past the top eight — stays.
#[test]
fn values_the_panel_did_not_offer_are_kept() {
    assert_eq!(toggle("ext:pdf;xy", "ext", "docx"), "ext:pdf;xy;docx");
    assert_eq!(toggle("ext:pdf;xy", "ext", "pdf"), "ext:xy");
    let t = term_at("ext:pdf;xy", 10).unwrap();
    let values = [
        ("pdf".to_string(), ".pdf".to_string()),
        ("docx".to_string(), ".docx".to_string()),
    ];
    let c = list_kept(&t, &values);
    assert_eq!((c[0].term.as_str(), c[0].hint.as_str()), ("ext:xy", "✓"));
    assert_eq!(c[1].term, "ext:pdf;xy;docx");
    assert!(c.iter().all(|c| c.stays));
    let t = term_at("!ext:.PDF", 9).unwrap();
    assert_eq!(list_kept(&t, &values)[0].term, "!ext:");
}

#[test]
fn excluding_is_a_term_of_its_own_and_a_value_is_on_one_side_only() {
    let q = exclude("kind:doc", "kind", "build");
    assert_eq!(q, "kind:doc !kind:build");
    let q = exclude(&q, "kind", "exec");
    assert_eq!(q, "kind:doc !kind:build;exec");
    assert_eq!(exclude(&q, "kind", "build"), "kind:doc !kind:exec");
    assert_eq!(toggle(&q, "kind", "build"), "kind:doc;build !kind:exec");
    assert_eq!(
        exclude("kind:doc;code", "kind", "code"),
        "kind:doc !kind:code"
    );
    assert_eq!(exclude("kind:doc !kind:build", "kind", "build"), "kind:doc");
    // A `!` term in an OR run is not where an exclusion goes.
    assert_eq!(
        exclude("!kind:font|x", "kind", "build"),
        "!kind:font|x !kind:build"
    );
}

#[test]
fn the_query_says_what_is_in_and_what_is_out_whatever_its_spelling() {
    let p = picked("tür:belge;resim !tür:derleme ext:pdf", "kind");
    assert_eq!(p.on, ["doc", "image"]);
    assert_eq!(p.off, ["build"]);
    assert_eq!(picked("ext:.PDF ; odt", "ext").on, ["pdf", "odt"]);
    assert_eq!(picked("rapor", "ext"), Picked::default());
    assert_eq!(toggle("tür:belge", "kind", "code"), "tür:belge;code");
    assert_eq!(toggle("tür:belge;code", "kind", "doc"), "tür:code");
    assert_eq!(toggle("ext:pdf ; odt", "ext", "odt"), "ext:pdf");
}

#[test]
fn a_value_leaving_an_or_run_leaves_no_bar_behind() {
    assert_eq!(
        toggle("kind:doc|ext:pdf rapor", "kind", "doc"),
        "ext:pdf rapor"
    );
    assert_eq!(
        toggle("kind:doc|ext:pdf", "kind", "code"),
        "kind:doc;code|ext:pdf"
    );
}

/// Each round trip: the query to what is picked, and toggling every picked
/// value off and on again gives the query back, values in order.
#[test]
fn every_pick_round_trips() {
    for q in [
        "kind:doc;code !kind:build",
        "!ext:log;tmp",
        "tür:doc ext:pdf;odt",
    ] {
        for field in ["kind", "ext"] {
            let p = picked(q, field);
            for (vals, ex) in [(&p.on, false), (&p.off, true)] {
                let mut text = q.to_string();
                for v in vals {
                    text = value_toggle(&text, field, v, ex).0;
                }
                for v in vals {
                    text = value_toggle(&text, field, v, ex).0;
                }
                assert_eq!(picked(&text, field), p, "{q} {field}");
            }
        }
    }
}

#[test]
fn the_kinds_are_counted_without_kinds_and_the_extensions_within_them() {
    let a = type_ask("rapor kind:doc !kind:build ext:pdf");
    assert_eq!(a.kinds, "rapor ext:pdf");
    assert_eq!(a.exts, "rapor kind:doc !kind:build");
    assert!(!a.one_walk());
    let a = type_ask("rapor kind:doc|ext:pdf dm:7d");
    assert_eq!(
        (a.kinds.as_str(), a.exts.as_str()),
        ("rapor dm:7d", "rapor dm:7d")
    );
    assert!(a.one_walk());
    assert_eq!(type_ask("tür:doc ; code rapor").kinds, "rapor");
}
