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
    let editing = lens::Caret::editing(typed.len());
    let (q, _) = lens::place_add_at(typed, HOME, "/home/hasan/Belgeler", editing);
    assert_eq!(q, "rapor under:/mnt/depo|under:~/Belgeler");
    let resting = lens::Caret::resting(q.len());
    assert_eq!(lens::place_add_at(&q, HOME, "~/Belgeler", resting).0, q);
    // It rests at the group's end; on an exclusion when the lens was on one.
    let (q2, caret) = lens::place_settle(&q, group_at(&q, HOME, 6), false);
    assert_eq!(&q2[..caret], q);
    let x = "rapor !under:/a ext:pdf";
    assert_eq!(&x[..lens::place_settle(x, 6, true).1], "rapor !under:/a");
}

#[test]
fn a_path_is_quoted_when_a_space_a_bar_or_a_semicolon_would_cut_it() {
    assert_eq!(quote("~/a b".into()), "\"~/a b\"");
    assert_eq!(quote("/mnt/x|y".into()), "\"/mnt/x|y\"");
    assert_eq!(quote("~/a;b".into()), "\"~/a;b\"");
    assert_eq!(quote("~/ab".into()), "~/ab");
}
