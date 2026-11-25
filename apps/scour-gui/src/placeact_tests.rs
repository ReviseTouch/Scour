use super::*;

const HOME: &str = "/home/hasan";

#[test]
fn the_group_is_found_where_it_starts_or_the_lens_is() {
    assert_eq!(group_at("rapor under:/a|under:/b", HOME, 99), 6);
    assert_eq!(group_at("rapor under:/a|under:/b", HOME, 3), 3);
    assert_eq!(group_at("rapor ext:pdf", HOME, 6), 6);
}

#[test]
fn a_press_writes_what_the_model_writes() {
    // The lens's term while typed gives way; a second Add of a held folder leaves it.
    let typed = "rapor under:/mnt/depo|under:~/Bel";
    let t = lens::term_at(typed, typed.len());
    let (q, _) = lens::place_add(typed, HOME, "/home/hasan/Belgeler", t.as_ref());
    assert_eq!(q, "rapor under:/mnt/depo|under:~/Belgeler");
    assert_eq!(lens::place_add(&q, HOME, "~/Belgeler", None).0, q);
    let (q, caret) = lens::place_anchor(&q, HOME, group_at(&q, HOME, 6));
    assert_eq!(&q[..caret], "rapor under:/mnt/depo");
}

#[test]
fn a_path_is_quoted_when_a_space_or_a_bar_would_cut_it() {
    assert_eq!(quote("~/a b".into()), "\"~/a b\"");
    assert_eq!(quote("/mnt/x|y".into()), "\"/mnt/x|y\"");
    assert_eq!(quote("~/ab".into()), "~/ab");
}
