use super::*;
use crate::lens::{Lens, lens_of, size_preset_term, term_at, with_term};

fn t(q: &str, at: usize) -> Term {
    term_at(q, at).unwrap()
}

fn shown(r: (String, usize)) -> String {
    format!("{}|{}", &r.0[..r.1], &r.0[r.1..])
}

/// Nothing glued, whatever the text ends with or where the term stood.
#[test]
fn a_written_term_never_touches_its_neighbour() {
    let tog = |q: &str, v: &str, ex: bool| value_toggle(q, "kind", v, ex).0;
    assert_eq!(
        tog("kind:doc rapor ", "build", true),
        "kind:doc rapor !kind:build"
    );
    assert_eq!(tog("kind:doc rapor ", "doc", true), "rapor !kind:doc");
    assert_eq!(tog("rapor", "doc", false), "rapor kind:doc");
    assert_eq!(tog("rapor ", "doc", false), "rapor kind:doc");
    assert_eq!(tog("", "doc", false), "kind:doc");
    assert_eq!(tog("   ", "doc", true), "!kind:doc");
    assert_eq!(
        tog("kind:doc|ext:pdf rapor", "doc", true),
        "ext:pdf rapor !kind:doc"
    );
    assert_eq!(tog("a|kind:doc|b", "doc", false), "a|b");
    assert_eq!(with_term("rapor ", "size:>1gb"), "rapor size:>1gb");
    assert_eq!(with_term(" ", " "), "");
}

#[test]
fn a_choice_rests_at_the_end_of_what_it_wrote() {
    let q = "dm:7d rapor";
    assert_eq!(
        shown(choice_settle(q, &t(q, 5), "dm:30d", "dm")),
        "dm:30d| rapor"
    );
    let q = "dm:7d|rapor";
    assert_eq!(
        shown(choice_settle(q, &t(q, 5), "dm:1y", "dm")),
        "dm:1y||rapor"
    );
    let q = "rapor size:>1gb";
    let w = size_preset_term(&t(q, q.len()), 0);
    assert_eq!(
        shown(choice_settle(q, &t(q, q.len()), &w, "size")),
        "rapor empty:|"
    );
}

/// A Before reaching now writes nothing: the term goes and an empty one
/// keeps the lens where it was.
#[test]
fn a_choice_that_takes_its_term_out_leaves_an_empty_one_in_its_place() {
    let gone = |q: &str, at: usize| shown(choice_settle(q, &t(q, at), "", "dm"));
    assert_eq!(gone("rapor dm:7d ", 11), "rapor dm:|");
    assert_eq!(gone("rapor dm:7d", 11), "rapor dm:|");
    assert_eq!(gone("dm:7d rapor", 5), "dm:| rapor");
    assert_eq!(gone("dm:7d|rapor", 5), "dm:| rapor");
    assert_eq!(gone("a|dm:7d|b", 7), "a|b dm:|");
    assert_eq!(gone("dm:7d", 5), "dm:|");
}

#[test]
fn a_toggle_rests_on_the_term_of_the_side_it_changed() {
    let s = |q: &str, f: &str, v: &str, ex: bool| shown(value_settle(q, f, v, ex));
    assert_eq!(s("rapor", "kind", "doc", false), "rapor kind:doc|");
    assert_eq!(
        s("kind:doc rapor", "kind", "build", true),
        "kind:doc rapor !kind:build|"
    );
    assert_eq!(
        s("kind:doc;code", "kind", "code", true),
        "kind:doc !kind:code|"
    );
    assert_eq!(
        s("kind:doc rapor kind:code", "kind", "image", false),
        "kind:doc;image| rapor kind:code"
    );
    // That side emptied: the other side's term, else an empty one.
    assert_eq!(
        s("kind:doc !kind:build", "kind", "doc", false),
        "!kind:build|"
    );
    assert_eq!(
        s("kind:doc !kind:build", "kind", "build", true),
        "kind:doc|"
    );
    assert_eq!(
        s("rapor kind:doc dm:7d", "kind", "doc", false),
        "rapor kind:| dm:7d"
    );
    assert_eq!(s("kind:doc", "kind", "doc", false), "kind:|");
    assert_eq!(s("!ext:log", "ext", "log", true), "ext:|");
}

#[test]
fn an_extension_from_the_kind_tiles_moves_the_lens_to_its_row() {
    let (q, c) = value_settle("kind:doc", "ext", "pdf", false);
    assert_eq!(shown((q.clone(), c)), "kind:doc ext:pdf|");
    assert_eq!(lens_of(&q, &t(&q, c)), Some(Lens::Ext));
    let (q, c) = value_settle("kind:doc", "kind", "build", true);
    assert_eq!(lens_of(&q, &t(&q, c)), Some(Lens::Kind));
}
