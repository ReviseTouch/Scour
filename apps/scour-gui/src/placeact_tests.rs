use super::*;

const HOME: &str = "/home/hasan";

fn typing(text: &str, at: usize) -> lens::Term {
    lens::term_at(text, at).expect("a term there")
}

#[test]
fn add_joins_the_group_and_never_takes_a_folder_out() {
    let q = add("rapor under:/mnt/depo", HOME, "/usr", None);
    assert_eq!(q, "rapor under:/mnt/depo|under:/usr");
    // Already there: a second press of Add leaves it.
    assert_eq!(add(&q, HOME, "/usr", None), q);
}

#[test]
fn add_narrows_a_member_it_lies_inside() {
    let q = add(
        "rapor under:/mnt/depo|under:/usr",
        HOME,
        "/mnt/depo/PROJELER",
        None,
    );
    assert_eq!(q, "rapor under:/usr|under:/mnt/depo/PROJELER");
}

#[test]
fn add_takes_the_place_of_the_term_being_walked_or_typed() {
    let walked = "rapor under:~/";
    let q = add(
        walked,
        HOME,
        "/home/hasan/Belgeler",
        Some(&typing(walked, 14)),
    );
    assert_eq!(q, "rapor under:~/Belgeler");
    let typed = "rapor under:~/Bel";
    let q = add(
        typed,
        HOME,
        "/home/hasan/Belgeler",
        Some(&typing(typed, 17)),
    );
    assert_eq!(q, "rapor under:~/Belgeler");
    let empty = "rapor under:";
    let q = add(empty, HOME, "/mnt/depo/My Docs", Some(&typing(empty, 12)));
    assert_eq!(q, "rapor under:\"/mnt/depo/My Docs\"");
}

#[test]
fn exclude_is_a_term_of_its_own_and_written_once() {
    let q = exclude("rapor under:/mnt/depo|under:/usr", HOME, "/home/hasan/x y");
    assert_eq!(q, "rapor under:/mnt/depo|under:/usr !under:\"~/x y\"");
    assert_eq!(exclude(&q, HOME, "/home/hasan/x y"), q);
}

#[test]
fn the_caret_settles_on_the_groups_first_member_or_an_empty_place() {
    let (q, at) = settle("rapor under:/a|under:/b".into(), HOME);
    assert_eq!(&q[..at], "rapor under:/a");
    let (q, at) = settle("rapor".into(), HOME);
    assert_eq!((q.as_str(), at), ("rapor under:", 12));
    let (q, at) = settle("under: rapor".into(), HOME);
    assert_eq!((q.as_str(), at), ("under: rapor", 6));
}
