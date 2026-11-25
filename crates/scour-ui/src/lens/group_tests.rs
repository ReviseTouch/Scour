use super::*;

const HOME: &str = "/home/hasan";

fn also(q: &str, root: &str) -> String {
    place_toggle(q, HOME, root, Pick::Also).0
}

fn only(q: &str, root: &str) -> String {
    place_toggle(q, HOME, root, Pick::Only).0
}

#[test]
fn the_first_disk_is_one_term() {
    assert_eq!(
        place_toggle("rapor", HOME, "/mnt/depo", Pick::Only),
        ("rapor under:/mnt/depo".into(), 21)
    );
    assert_eq!(also("", "/mnt/depo"), "under:/mnt/depo");
    assert_eq!(also("rapor ", "/"), "rapor under:/");
}

#[test]
fn an_empty_place_term_is_where_the_disk_goes() {
    let q = "rapor under: ext:pdf";
    assert_eq!(
        place_toggle(q, HOME, "/mnt/depo", Pick::Also),
        ("rapor under:/mnt/depo ext:pdf".into(), 21)
    );
    assert_eq!(only(q, "/mnt/depo"), "rapor under:/mnt/depo ext:pdf");
}

#[test]
fn a_second_disk_joins_the_group_without_spaces() {
    let q = "rapor under:/mnt/depo ext:pdf";
    assert_eq!(
        place_toggle(q, HOME, "/", Pick::Also),
        ("rapor under:/mnt/depo|under:/ ext:pdf".into(), 29)
    );
    assert_eq!(
        also("rapor under:/a|under:/b", "/c"),
        "rapor under:/a|under:/b|under:/c"
    );
}

#[test]
fn taking_one_out_leaves_no_bar_behind() {
    let q = "rapor under:/a|under:/b ext:pdf";
    assert_eq!(
        place_toggle(q, HOME, "/a", Pick::Also),
        ("rapor under:/b ext:pdf".into(), 6)
    );
    assert_eq!(also(q, "/b"), "rapor under:/a ext:pdf");
    assert_eq!(also("under:/a | under:/b rapor", "/b"), "under:/a rapor");
    assert_eq!(
        also("rapor under:/a|under:/b|under:/c", "/b"),
        "rapor under:/a|under:/c"
    );
}

#[test]
fn the_last_one_out_takes_the_term_and_a_space() {
    assert_eq!(
        place_toggle("rapor under:/a ext:pdf", HOME, "/a/", Pick::Also),
        ("rapor ext:pdf".into(), 6)
    );
    assert_eq!(only("rapor under:/a", "/a"), "rapor");
    assert_eq!(only("under:/a/|under:/a", "/a"), "");
}

#[test]
fn only_leaves_one_disk() {
    let q = "rapor under:/a|under:/b|under:/c ext:pdf";
    assert_eq!(
        place_toggle(q, HOME, "/b", Pick::Only),
        ("rapor under:/b ext:pdf".into(), 14)
    );
    assert_eq!(only(q, "/d"), "rapor under:/d ext:pdf");
    assert_eq!(only("under:/a", "/d"), "under:/d");
}

#[test]
fn a_path_with_a_space_or_a_bar_is_quoted() {
    let q = also("rapor", "/mnt/depo/My Docs");
    assert_eq!(q, r#"rapor under:"/mnt/depo/My Docs""#);
    let q = also(&q, "/srv/a|b");
    assert_eq!(q, r#"rapor under:"/mnt/depo/My Docs"|under:"/srv/a|b""#);
    assert_eq!(also(&q, "/mnt/depo/My Docs"), r#"rapor under:"/srv/a|b""#);
}

#[test]
fn home_is_written_and_read_with_a_tilde() {
    assert_eq!(
        also("rapor", "/home/hasan/Belgeler"),
        "rapor under:~/Belgeler"
    );
    assert_eq!(
        also("rapor under:~/Belgeler", "/mnt/depo"),
        "rapor under:~/Belgeler|under:/mnt/depo"
    );
    assert_eq!(
        also(
            "rapor under:~/Belgeler/|under:/mnt/depo",
            "/home/hasan/Belgeler"
        ),
        "rapor under:/mnt/depo"
    );
}

#[test]
fn an_exclusion_elsewhere_is_left_alone() {
    let q = "rapor !under:/mnt/depo/tmp";
    assert_eq!(
        also(q, "/mnt/depo"),
        "rapor !under:/mnt/depo/tmp under:/mnt/depo"
    );
    let q = "!under:/x rapor under:/a|under:/b";
    assert_eq!(also(q, "/a"), "!under:/x rapor under:/b");
    assert_eq!(only(q, "/b"), "!under:/x rapor under:/b");
    assert_eq!(also("rapor !under:/a", "/a"), "rapor !under:/a under:/a");
}

#[test]
fn a_second_run_is_not_the_group() {
    let q = "under:/a|under:/b rapor under:/c";
    assert_eq!(also(q, "/c"), "under:/a|under:/b|under:/c rapor under:/c");
    assert_eq!(also(q, "/a"), "under:/b rapor under:/c");
}

#[test]
fn removing_a_member_keeps_the_rest_of_its_run() {
    let q = "rapor|fatura ext:pdf";
    let t = &terms(q)[1];
    assert_eq!(remove_term(q, t), ("rapor ext:pdf".into(), 5));
    let t = &terms(q)[0];
    assert_eq!(remove_term(q, t), ("fatura ext:pdf".into(), 0));
    let t = &terms(q)[2];
    assert_eq!(remove_term(q, t), ("rapor|fatura".into(), 12));
}

#[test]
fn runs_join_on_a_bar_with_or_without_spaces() {
    let q = "a|b c | d e";
    let r: Vec<usize> = runs(q).iter().map(Vec::len).collect();
    assert_eq!(r, [2, 2, 1]);
    assert_eq!(run_of(q, &terms(q)[3]).len(), 2);
}

#[test]
fn the_scope_is_read_from_the_text() {
    let q = r#"rapor under:~/Belgeler/|under:"/mnt/depo/My Docs" !under:/mnt/depo/tmp under:"#;
    let s = scope(q, HOME);
    let within: Vec<&str> = s.within.iter().map(|h| h.path.as_str()).collect();
    assert_eq!(within, ["/home/hasan/Belgeler", "/mnt/depo/My Docs"]);
    let outside: Vec<&str> = s.outside.iter().map(|h| h.path.as_str()).collect();
    assert_eq!(outside, ["/mnt/depo/tmp"]);
    assert!(s.holds("/mnt/depo/My Docs/"));
    assert!(!s.holds("/mnt/depo"));
    let (out, _) = remove_term(q, &s.outside[0].term);
    assert_eq!(scope(&out, HOME).outside, []);
}

#[test]
fn a_negated_member_is_neither_inside_nor_outside() {
    let s = scope("!under:/a|under:/b", HOME);
    assert_eq!(s.within.len(), 1);
    assert_eq!(s.within[0].path, "/b");
    assert!(s.outside.is_empty());
    assert_eq!(scope("rapor", HOME), Scope::default());
    assert!(scope("under:/", HOME).holds("/"));
}

fn tile(q: &str, roots: &[&str], pick: Pick) -> String {
    let roots: Vec<String> = roots.iter().map(|r| r.to_string()).collect();
    tile_toggle(q, HOME, &roots, pick).0
}

#[test]
fn a_tile_of_several_roots_goes_in_and_out_as_one() {
    let sys = ["/home/hasan", "/usr", "/etc"];
    let q = tile("rapor", &sys, Pick::Only);
    assert_eq!(q, "rapor under:~|under:/usr|under:/etc");
    assert_eq!(tile(&q, &sys, Pick::Only), "rapor");
    assert_eq!(tile(&q, &sys, Pick::Also), "rapor");
    // Another disk beside it, then that one alone.
    let both = tile(&q, &["/mnt/depo"], Pick::Also);
    assert_eq!(both, "rapor under:~|under:/usr|under:/etc|under:/mnt/depo");
    assert_eq!(
        tile(&both, &["/mnt/depo"], Pick::Only),
        "rapor under:/mnt/depo"
    );
    assert_eq!(tile(&both, &sys, Pick::Also), "rapor under:/mnt/depo");
}

#[test]
fn a_tile_half_held_is_completed_not_dropped() {
    let q = "rapor under:/usr ext:pdf";
    assert_eq!(
        tile(q, &["/usr", "/etc"], Pick::Also),
        "rapor under:/usr|under:/etc ext:pdf"
    );
    assert_eq!(
        tile(q, &["/usr", "/etc"], Pick::Only),
        "rapor under:/usr|under:/etc ext:pdf"
    );
    assert_eq!(tile(q, &[], Pick::Only), "rapor ext:pdf");
}

#[test]
fn dropping_an_empty_member_takes_its_bar_too() {
    for (q, at, want) in [
        ("rapor under:/a|under: ext:pdf", 2, "rapor under:/a ext:pdf"),
        ("rapor under:|under:/a", 1, "rapor under:/a"),
        ("rapor under: | under:/a", 1, "rapor under:/a"),
        ("rapor under: ext:pdf", 1, "rapor ext:pdf"),
    ] {
        let t = &terms(q)[at];
        assert_eq!(replace(q, t, "").0, want, "{q}");
        assert_eq!(crate::lens::without(q, t), want, "{q}");
    }
}
