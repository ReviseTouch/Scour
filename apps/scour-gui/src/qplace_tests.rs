use super::*;
use crate::qlens::{Chosen, Lenses};

const HOME: &str = "/home/hasan";

fn reply(json: &str) -> Response {
    serde_json::from_str(json).expect("a reply the service sends")
}

/// The owner's machine: the system disk indexed through two sources, a
/// second disk configured whole, and a stick just plugged in.
fn volumes() -> Response {
    reply(
        r#"{"result":"volumes","disks":[
 {"volume":{"id":"uuid:cachy","roots":["/"],"label":"CACHY","kind":"system","fs":"btrfs","size":700000000000},
  "state":"manual","seen":{"walked":null,"live":false,"changed":null,"walking":false}},
 {"volume":{"id":"uuid:depo","roots":["/mnt/depo"],"label":"Depo","kind":"fixed","fs":"ntfs3","size":945000000000},
  "state":"configured","seen":{"walked":null,"live":true,"changed":null,"walking":false}},
 {"volume":{"id":"uuid:stick","roots":["/run/media/hasan/STICK"],"label":"STICK","kind":"removable","fs":"vfat","size":64000000},
  "state":"asking","seen":{"walked":null,"live":false,"changed":null,"walking":false}}],
 "folders":[]}"#,
    )
}

fn sources() -> Response {
    reply(
        r#"{"result":"sources","sources":[
 {"id":0,"name":"home","kind":"local","roots":["/home/hasan"],"caps":"WATCH | CASE_SENSITIVE"},
 {"id":1,"name":"depo","kind":"local","roots":["/mnt/depo"],"caps":"WATCH | CASE_SENSITIVE"},
 {"id":2,"name":"sistem","kind":"local","roots":["/usr","/etc"],"caps":"CASE_SENSITIVE"}]}"#,
    )
}

fn count(n: u64) -> Response {
    reply(&format!(
        r#"{{"result":"count","total":{n},"capped":false}}"#
    ))
}

fn with_env<R>(f: impl FnOnce(&Env) -> R) -> R {
    let tr = |m: &str| m.to_string();
    let kind_name = |k: &str| k.to_string();
    let places = vec![("Documents".to_string(), format!("{HOME}/Documents"))];
    f(&Env {
        tr: &tr,
        kind_name: &kind_name,
        home: HOME,
        places: &places,
    })
}

fn slots(wants: &[(u32, Request)]) -> Vec<u32> {
    let mut s: Vec<u32> = wants.iter().map(|(s, _)| *s).collect();
    s.sort_unstable();
    s
}

/// Open the place lens on `q` and answer the disks; what it asks next.
fn opened(lenses: &mut Lenses, q: &str) -> Vec<(u32, Request)> {
    with_env(|env| {
        let first = lenses.sync(q, q.len(), true, env);
        assert_eq!(slots(&first), [VOLUMES, USAGE, TREE, SOURCES]);
        let g = lenses.generation;
        assert!(lenses.answer(g, VOLUMES, volumes(), q, env).is_empty());
        lenses.answer(g, SOURCES, sources(), q, env)
    })
}

#[test]
fn the_disks_are_counted_once_both_lists_are_in_and_only_those_with_files() {
    let mut lenses = Lenses::default();
    let wants = opened(&mut lenses, "rapor under:");
    // The stick is waiting for an answer: nothing on it to count.
    assert_eq!(slots(&wants), [DISK, DISK + 1]);
    let Request::Count { query, cap } = &wants.iter().find(|w| w.0 == DISK).unwrap().1 else {
        panic!("a count");
    };
    assert_eq!(*cap, u32::MAX);
    assert_eq!(query, "file: under:/etc|under:/home/hasan|under:/usr");
    let q = "rapor under:";
    with_env(|env| {
        let g = lenses.generation;
        assert!(lenses.answer(g, DISK, count(4_800_000), q, env).is_empty());
        lenses.answer(g, DISK + 1, count(1_700_000), q, env);
        // Drawn again in the same opening: the disks are not asked twice.
        assert_eq!(slots(&lenses.sync(q, q.len(), true, env)), [USAGE, TREE]);
    });
    let o = lenses.open.as_ref().unwrap();
    let files: Vec<Option<u64>> = o.tiles.iter().map(|t| t.files).collect();
    assert_eq!(files, [Some(4_800_000), Some(1_700_000), None]);
}

#[test]
fn an_answer_from_an_older_opening_is_not_taken() {
    let mut lenses = Lenses::default();
    opened(&mut lenses, "under:");
    let old = lenses.generation;
    with_env(|env| {
        lenses.sync("under:/", 7, true, env);
        assert!(
            lenses
                .answer(old, DISK, count(9), "under:/", env)
                .is_empty()
        );
    });
    assert!(lenses.open.as_ref().unwrap().tiles[0].files.is_none());
}

#[test]
fn the_keys_move_over_what_is_drawn_in_the_order_it_is_drawn() {
    let mut lenses = Lenses::default();
    opened(&mut lenses, "under:");
    let o = lenses.open.as_ref().unwrap();
    let at = lines(o);
    assert_eq!((at.places, at.here), (3, 5));
    let labels: Vec<&str> = o.items.iter().map(|i| i.label.as_str()).collect();
    assert_eq!(
        labels,
        ["CACHY", "Depo", "STICK", "~", "Documents", "this folder"]
    );
    // The stick is passed over: nothing on it can be chosen yet.
    assert!(o.items[2].wait);
    assert_eq!(o.items[0].tile, Some(0));
    assert_eq!(o.items[4].term, "under:~/Documents");
}

#[test]
fn enter_on_a_tile_is_a_click_on_it() {
    let mut lenses = Lenses::default();
    opened(&mut lenses, "under:");
    assert!(matches!(
        lenses.choose(1, false, "under:"),
        Some(Chosen::Tile(1))
    ));
    // Opened, the lens rests on this folder: Enter chooses it, as it did.
    assert_eq!(lenses.open.as_ref().unwrap().sel, 5);
    lenses.step(-1);
    lenses.step(-1);
    lenses.step(-1);
    // From home up to Depo: the stick between them is passed over.
    assert_eq!(lenses.open.as_ref().unwrap().sel, 1);
}

#[test]
fn a_folder_a_click_pinned_is_shown_until_the_text_changes() {
    let pin = Some(("under:/mnt/depo".to_string(), "/mnt/depo".to_string()));
    assert_eq!(pinned(&pin, "under:/mnt/depo"), Some("/mnt/depo"));
    assert_eq!(pinned(&pin, "under:/mnt/dep"), None);
    assert_eq!(
        browsed("/mnt/depo", HOME, None),
        ("/mnt".to_string(), "depo".to_string())
    );
    assert_eq!(
        browsed("/mnt/depo", HOME, Some("/mnt/depo")),
        ("/mnt/depo".to_string(), String::new())
    );
    assert_eq!(browsed("", HOME, None), (HOME.to_string(), String::new()));
    assert_eq!(
        browsed("~/Bel", HOME, None),
        (HOME.to_string(), "Bel".to_string())
    );
}

#[test]
fn a_folder_is_weighed_against_the_query_less_the_whole_place_group() {
    let q = "rapor under:/a|under:/b ext:pdf";
    let t = lens::term_at(q, 14).unwrap();
    assert_eq!(scoped(q, &t), "rapor ext:pdf");
}

#[test]
fn typing_puts_the_line_back_on_the_first_folder_that_holds_what_is_typed() {
    let mut lenses = Lenses::default();
    opened(&mut lenses, "under:");
    lenses.step(-1);
    let usage = reply(
        r#"{"result":"usage","root":{"path":"/home/hasan","bytes":0,"disk":0,"files":0,"age":[0,0,0,0,0,0]},
 "children":[{"path":"/home/hasan/Belgeler","bytes":9,"disk":9,"files":3,"age":[0,0,0,0,0,0]}],
 "child_count":1,"took_us":1}"#,
    );
    with_env(|env| {
        let q = "under:~/Bel";
        lenses.sync(q, q.len(), false, env);
        let g = lenses.generation;
        lenses.answer(g, USAGE, usage, q, env);
    });
    let o = lenses.open.as_ref().unwrap();
    assert_eq!(o.items[o.sel].label, "Belgeler/");
}
