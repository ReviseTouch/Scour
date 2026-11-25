use super::super::machine::{HOME, SYSTEM};
use super::super::{term_at, without_places};
use super::*;

fn add(q: &str, path: &str) -> String {
    place_add(q, HOME, path, None).0
}

#[test]
fn add_joins_the_group_and_says_where_it_wrote() {
    let (q, at) = place_add("rapor under:/mnt/depo ext:pdf", HOME, "/usr", None);
    assert_eq!(q, "rapor under:/mnt/depo|under:/usr ext:pdf");
    assert_eq!(&q[..at], "rapor under:/mnt/depo|under:/usr");
    assert_eq!(
        add("rapor", "/home/hasan/Belgeler"),
        "rapor under:~/Belgeler"
    );
    assert_eq!(add("", "/"), "under:/");
}

#[test]
fn add_never_takes_a_folder_out() {
    let q = "rapor under:/mnt/depo|under:/usr";
    assert_eq!(place_add(q, HOME, "/usr/", None), (q.into(), q.len()));
    assert_eq!(add(&add(q, "/opt"), "/opt"), add(q, "/opt"));
}

#[test]
fn a_folder_inside_a_member_narrows_it_where_it_stands() {
    let (q, at) = place_add("rapor under:~|under:/mnt/depo", HOME, "~/Belgeler", None);
    assert_eq!(q, "rapor under:~/Belgeler|under:/mnt/depo");
    assert_eq!(&q[..at], "rapor under:~/Belgeler");
    let q = format!("rapor {SYSTEM}");
    assert_eq!(
        add(&q, "/home/hasan/Projeler/Scour"),
        "rapor under:/etc|under:~/Projeler/Scour|under:/opt|under:/usr|under:/var"
    );
}

#[test]
fn a_wider_folder_takes_every_member_it_holds() {
    let q = "rapor under:~/a|under:/mnt/depo|under:~/b ext:pdf";
    assert_eq!(add(q, "~"), "rapor under:~|under:/mnt/depo ext:pdf");
    assert_eq!(add(&format!("x {SYSTEM}"), "/"), "x under:/");
    // `/home/hasan2` is not inside `~`.
    assert_eq!(add("under:~", "/home/hasan2"), "under:~|under:/home/hasan2");
}

#[test]
fn an_empty_place_or_one_being_typed_gives_way() {
    assert_eq!(
        add("rapor under: ext:pdf", "/mnt/depo"),
        "rapor under:/mnt/depo ext:pdf"
    );
    let typed = "rapor under:/mnt/depo|under:~/gel";
    let t = term_at(typed, typed.len());
    assert_eq!(
        place_add(typed, HOME, "~/Belgeler", t.as_ref()).0,
        "rapor under:/mnt/depo|under:~/Belgeler"
    );
    // A finished folder, or one walked into, is not being typed.
    let walked = "rapor under:~/";
    let t = term_at(walked, walked.len());
    assert_eq!(
        place_add(walked, HOME, "~/Belgeler", t.as_ref()).0,
        "rapor under:~/Belgeler"
    );
    // Only a place term can be typed into one.
    let other = "rapor under:/mnt/depo ext:pd";
    let t = term_at(other, other.len());
    assert_eq!(
        place_add(other, HOME, "/usr", t.as_ref()).0,
        "rapor under:/mnt/depo|under:/usr ext:pd"
    );
}

#[test]
fn add_takes_away_the_same_folders_exclusion() {
    let (q, at) = place_add("!under:~/x rapor under:/usr", HOME, "~/x", None);
    assert_eq!(q, "rapor under:/usr|under:~/x");
    assert_eq!(&q[..at], "rapor under:/usr|under:~/x");
    assert_eq!(
        add("rapor !under:~/x/y", "~/x"),
        "rapor !under:~/x/y under:~/x"
    );
}

#[test]
fn quoted_paths_are_read_and_written_quoted() {
    let q = add("rapor", "/mnt/depo/My Docs");
    assert_eq!(q, r#"rapor under:"/mnt/depo/My Docs""#);
    assert_eq!(add(&q, "/mnt/depo/My Docs"), q);
    assert_eq!(
        add(&q, "/mnt/depo/My Docs/2025 Q1"),
        r#"rapor under:"/mnt/depo/My Docs/2025 Q1""#
    );
}

#[test]
fn exclude_writes_its_own_term_and_takes_it_back() {
    let q = "rapor under:/mnt/depo|under:/usr";
    let (out, at) = place_exclude(q, HOME, "/home/hasan/x y");
    assert_eq!(out, r#"rapor under:/mnt/depo|under:/usr !under:"~/x y""#);
    assert_eq!(at, out.len());
    assert_eq!(place_exclude(&out, HOME, "~/x y/"), (q.into(), q.len()));
}

#[test]
fn excluding_a_held_folder_takes_it_out_of_the_group() {
    let (q, _) = place_exclude("rapor under:/mnt/depo|under:/usr", HOME, "/usr");
    assert_eq!(q, "rapor under:/mnt/depo !under:/usr");
    assert_eq!(place_exclude("under:~", HOME, "~").0, "!under:~");
}

#[test]
fn the_caret_rests_on_the_group_or_an_empty_place() {
    let (q, at) = place_anchor("rapor under:/a|under:/b", HOME, 0);
    assert_eq!(&q[..at], "rapor under:/a");
    assert_eq!(
        place_anchor("under: rapor", HOME, 9),
        ("under: rapor".into(), 6)
    );
    assert_eq!(
        place_anchor("rapor ext:pdf", HOME, 6),
        ("rapor under: ext:pdf".into(), 12)
    );
    assert_eq!(place_anchor("rapor", HOME, 99), ("rapor under:".into(), 12));
    // Inside a term, the place goes after it.
    assert_eq!(
        place_anchor("rapor ext:pdf", HOME, 8),
        ("rapor ext:pdf under:".into(), 20)
    );
}

#[test]
fn the_list_shows_the_folder_the_value_names() {
    assert_eq!(place_at("~/Bel", HOME), (HOME.into(), "Bel".into()));
    assert_eq!(place_at("", HOME), (HOME.into(), "".into()));
    assert_eq!(place_at("", ""), ("/".into(), "".into()));
    assert_eq!(
        place_at("/mnt/depo/", HOME),
        ("/mnt/depo".into(), "".into())
    );
    assert_eq!(place_at("/usr", HOME), ("/".into(), "usr".into()));
}

#[test]
fn without_places_leaves_no_bar_in_any_arrangement() {
    let cases = [
        ("rapor under:/a|under:/b", "rapor"),
        ("under:/a|under:/b rapor", "rapor"),
        ("rapor under:/a | under:/b ext:pdf", "rapor ext:pdf"),
        (
            "rapor|fatura under:/a|under:/b ext:pdf|ext:odt",
            "rapor|fatura ext:pdf|ext:odt",
        ),
        ("under:/a|ext:pdf rapor", "rapor"),
        ("rapor !under:/a|under:/b", "rapor"),
        ("rapor under: ext:pdf", "rapor ext:pdf"),
        ("rapor parent:~/x !under:/tmp", "rapor !under:/tmp"),
        (r#"under:"/a b|c" "x y""#, r#""x y""#),
        (&format!("{SYSTEM} rapor under:/mnt/depo"), "rapor"),
        ("rapor", "rapor"),
        ("", ""),
    ];
    for (q, want) in cases {
        let got = without_places(q);
        assert_eq!(got, want, "{q}");
        assert!(
            !got.trim().starts_with('|') && !got.trim().ends_with('|'),
            "{q}"
        );
    }
}

#[test]
fn add_at_lets_only_the_member_being_edited_give_way() {
    let at = |q: &str, path, c| place_add_at(q, HOME, path, c).0;
    // Resting after a press or a click on a part: a finished path joins.
    let q = "rapor under:/mnt/depo";
    assert_eq!(
        at(q, "/usr", Caret::resting(q.len())),
        "rapor under:/mnt/depo|under:/usr"
    );
    let q = "rapor under:~/gel";
    assert_eq!(
        at(q, "~/Belgeler", Caret::resting(q.len())),
        "rapor under:~/gel|under:~/Belgeler"
    );
    // Typed into: it gives way, letters after its `/` or not.
    assert_eq!(
        at(q, "~/Belgeler", Caret::editing(q.len())),
        "rapor under:~/Belgeler"
    );
    let q = "rapor under:/mnt/depo";
    assert_eq!(at(q, "/usr", Caret::editing(q.len())), "rapor under:/usr");
    assert_eq!(
        at(q, "/usr", Caret::resting(14)),
        "rapor under:/usr",
        "inside it"
    );
    // Only a place term gives way, and an empty one always.
    let q = "rapor under:/a ext:pd";
    assert_eq!(
        at(q, "/b", Caret::editing(q.len())),
        "rapor under:/a|under:/b ext:pd"
    );
    assert_eq!(
        at("rapor under:", "/b", Caret::resting(0)),
        "rapor under:/b"
    );
}

#[test]
fn the_caret_settles_at_the_groups_end_or_stays_on_an_exclusion() {
    let q = "rapor under:/a|under:/b ext:pdf";
    let (_, c) = place_settle(q, 6, false);
    assert_eq!(&q[..c], "rapor under:/a|under:/b");
    let q = "rapor under:/a !under:/x !under:/y";
    let (_, c) = place_settle(q, 25, true);
    assert_eq!(&q[..c], q, "on the exclusion it was on");
    let (_, c) = place_settle(q, 15, true);
    assert_eq!(&q[..c], "rapor under:/a !under:/x");
    assert_eq!(&q[..place_settle(q, 25, false).1], "rapor under:/a");
    // No exclusion left: the group; no group: an empty place where it was.
    assert_eq!(place_settle("rapor under:/a", 6, true).1, 14);
    assert_eq!(
        place_settle("rapor ext:pdf", 6, true),
        ("rapor under: ext:pdf".into(), 12)
    );
    assert_eq!(place_settle("rapor under:/a|under: x", 0, false).1, 21);
}

#[test]
fn every_path_writer_quotes_a_bar_and_a_semicolon() {
    for (path, written) in [
        ("/srv/a;b", r#"under:"/srv/a;b""#),
        ("/srv/a|b", r#"under:"/srv/a|b""#),
        ("/srv/a b", r#"under:"/srv/a b""#),
    ] {
        assert_eq!(add("", path), written);
        assert_eq!(place_exclude("", HOME, path).0, format!("!{written}"));
        let t = &terms("under:")[0];
        assert_eq!(crate::lens::place(t, path), written);
        let q = add("", path);
        assert_eq!(scope(&q, HOME).within[0].path, path, "read back whole");
    }
}
