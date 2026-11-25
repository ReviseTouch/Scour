use super::super::machine::{HOME, SYSTEM, machine};
use super::*;

fn names(q: &str) -> Vec<(String, bool)> {
    spots(q, HOME, &machine())
        .into_iter()
        .map(|s| (s.name, s.not))
        .collect()
}

#[test]
fn a_disk_held_whole_is_one_entry() {
    let q = format!("rapor {SYSTEM}|under:/mnt/depo");
    let s = spots(&q, HOME, &machine());
    assert_eq!(s.len(), 2);
    assert_eq!(
        (s[0].name.as_str(), s[0].kind, s[0].tile),
        ("CACHY", Some("system"), Some(0))
    );
    assert_eq!(s[0].terms.len(), 5);
    assert_eq!((s[1].name.as_str(), s[1].tile), ("Depo", Some(1)));
    assert_eq!(s[0].remove(&q, HOME).0, "rapor under:/mnt/depo");
    assert_eq!(s[1].remove(&q, HOME).0, format!("rapor {SYSTEM}"));
}

#[test]
fn a_disk_held_in_part_is_its_folders() {
    let n = names("rapor under:~|under:/usr|under:/mnt/depo/PROJELER !under:~/tmp");
    let want = [
        ("~", false),
        ("/usr", false),
        ("/mnt/depo/PROJELER", false),
        ("~/tmp", true),
    ];
    let want: Vec<(String, bool)> = want.iter().map(|(n, x)| (n.to_string(), *x)).collect();
    assert_eq!(n, want);
}

#[test]
fn entries_follow_the_text_and_each_goes_alone() {
    let q = r#"under:"/mnt/depo/My Docs"|under:/mnt/depo !under:/mnt/depo/tmp rapor"#;
    let s = spots(q, HOME, &machine());
    let n: Vec<&str> = s.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(n, ["/mnt/depo/My Docs", "Depo", "/mnt/depo/tmp"]);
    assert_eq!(s[0].short, "My Docs");
    assert_eq!(
        s[0].remove(q, HOME).0,
        "under:/mnt/depo !under:/mnt/depo/tmp rapor"
    );
    assert_eq!(
        s[1].remove(q, HOME).0,
        r#"under:"/mnt/depo/My Docs" !under:/mnt/depo/tmp rapor"#
    );
    assert_eq!(
        s[2].remove(q, HOME).0,
        r#"under:"/mnt/depo/My Docs"|under:/mnt/depo rapor"#
    );
}

#[test]
fn the_chip_says_names_and_cuts_at_max() {
    let say = |q: &str, max| chip_label(&spots(q, HOME, &machine()), max, "Everywhere");
    assert_eq!(say("rapor", 3), "Everywhere");
    assert_eq!(say("rapor !under:/tmp", 3), "Everywhere");
    assert_eq!(say(&format!("{SYSTEM}|under:/mnt/depo"), 3), "CACHY + Depo");
    assert_eq!(
        say("under:~|under:/usr|under:/mnt/depo/PROJELER", 3),
        "~ + usr + PROJELER"
    );
    assert_eq!(
        say("under:~|under:/usr|under:/mnt/depo/PROJELER", 1),
        "~ +2"
    );
    assert_eq!(say("under:~/Belgeler|under:/", 0), "Belgeler +1");
    assert_eq!(say("under:/", 2), "/");
}

#[test]
fn the_dots_light_the_disks_the_query_can_reach() {
    let t = machine();
    let dots = |q: &str| {
        chip_dots(&t, &scope(q, HOME))
            .iter()
            .map(|d| (d.kind, d.asking, d.lit))
            .collect::<Vec<_>>()
    };
    let all = [
        ("system", false, true),
        ("fixed", false, true),
        ("removable", true, false),
    ];
    assert_eq!(dots("rapor"), all);
    assert_eq!(
        dots("under:/mnt/depo/x")[..2],
        [("system", false, false), ("fixed", false, true)]
    );
    // A folder above a disk searches it too.
    assert_eq!(
        dots("under:/")[..2],
        [("system", false, true), ("fixed", false, true)]
    );
    assert!(tile_held(&t[1], &scope("under:/mnt/depo/", HOME)));
    assert!(!tile_held(&t[0], &scope("under:~|under:/usr", HOME)));
    assert!(tile_held(&t[0], &scope(SYSTEM, HOME)));
}

#[test]
fn a_spot_of_any_run_is_taken_out_of_its_own_run() {
    let q = "under:/a rapor under:/mnt/depo|under:/c";
    let caps = crate::lens::place_caps(q, HOME, &machine(), None);
    let depo = &caps[1].parts[0].spot;
    assert_eq!(depo.name, "Depo");
    assert_eq!(depo.remove(q, HOME), ("under:/a rapor under:/c".into(), 15));
    let c = &caps[1].parts[1].spot;
    assert_eq!(c.remove(q, HOME).0, "under:/a rapor under:/mnt/depo");
}

#[test]
fn a_disk_left_out_whole_is_one_negated_entry() {
    let none = "!under:/etc !under:~ !under:/opt !under:/usr !under:/var";
    let q = format!("rapor {none} !under:/mnt/depo/tmp");
    let s = spots(&q, HOME, &machine());
    let n: Vec<(&str, bool)> = s.iter().map(|s| (s.name.as_str(), s.not)).collect();
    assert_eq!(n, [("CACHY", true), ("/mnt/depo/tmp", true)]);
    assert_eq!(
        s[0].remove(&q, HOME),
        ("rapor !under:/mnt/depo/tmp".into(), 6)
    );
    assert_eq!(chip_label(&s, 3, "Everywhere"), "Everywhere");
}
