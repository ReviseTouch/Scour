use super::*;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[test]
fn a_disk_chosen_again_moves_between_the_lists() {
    let mut s = Settings::default();
    s.choose("uuid:a", true);
    s.choose("uuid:b", false);
    s.choose("uuid:a", false);
    s.choose("uuid:a", false);
    assert!(s.volumes_on.is_empty());
    assert_eq!(s.volumes_off, ["uuid:b", "uuid:a"]);
}

#[test]
fn a_face_opens_newest_first_in_every_tab_and_keeps_the_rest() {
    let tab = |q: &str, sort: &str| SearchTab {
        query: q.into(),
        sort: sort.into(),
        descending: Some(false),
    };
    let s = Settings {
        sort: "size".into(),
        descending: Some(false),
        tabs: vec![tab("a", "name"), tab("b", "path")],
        tab: 1,
        ..Default::default()
    }
    .opened();
    assert_eq!((s.sort.as_str(), s.descending), ("", None));
    for t in &s.tabs {
        assert_eq!((t.sort.as_str(), t.descending), ("", None));
    }
    assert_eq!(
        s.tabs.iter().map(|t| t.query.as_str()).collect::<Vec<_>>(),
        ["a", "b"]
    );
    assert_eq!(s.tab, 1);
}

fn dir(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("scour-settings-{name}"));
    let _ = std::fs::remove_dir_all(&p);
    p
}

#[test]
fn what_was_saved_comes_back() {
    let d = dir("roundtrip");
    let mut s = Settings {
        columns: vec!["name".into(), "atime".into()],
        sort: "accessed".into(),
        descending: Some(false),
        ..Settings::default()
    };
    s.widths.insert("name".into(), 240);
    s.remember("rapor ext:pdf");
    s.save(&d).expect("save");
    assert_eq!(Settings::load(&d), s);
    let _ = std::fs::remove_dir_all(&d);
}

/// **Defaults rather than a refusal.** The worst thing this could do is
/// stop the service because somebody's file has a stray comma in it.
#[test]
fn a_file_that_makes_no_sense_is_not_a_failure() {
    let d = dir("broken");
    std::fs::create_dir_all(&d).expect("mkdir");
    std::fs::write(Settings::path_in(&d), b"{ this is not json").expect("write");
    assert_eq!(Settings::load(&d), Settings::default());
    // And a directory nobody has written yet.
    assert_eq!(
        Settings::load(Path::new("/nonexistent")),
        Settings::default()
    );
    let _ = std::fs::remove_dir_all(&d);
}

/// A file from an older version keeps what it has and gains the rest.
#[test]
fn a_field_added_later_does_not_reject_the_file() {
    let d = dir("older");
    std::fs::create_dir_all(&d).expect("mkdir");
    std::fs::write(Settings::path_in(&d), br#"{"columns":["name","size"]}"#).expect("write");
    let s = Settings::load(&d);
    assert_eq!(s.columns, ["name", "size"]);
    assert!(s.history.is_empty());
    assert_eq!(s.descending, None);
    let _ = std::fs::remove_dir_all(&d);
}

/// Which face opens is a preference, not a launcher's guess. The empty string stays
/// a distinct answer, which is what makes the window the default.
#[test]
fn the_face_somebody_switched_to_is_the_one_that_opens() {
    let mut s = Settings::default();
    assert_eq!(s.face, "", "nobody has chosen, to start with");

    Change {
        face: Some("tui".into()),
        ..Change::default()
    }
    .apply(&mut s);
    assert_eq!(s.face, "tui");

    // A frontend saying something else says nothing about this.
    Change {
        language: Some("tr".into()),
        ..Change::default()
    }
    .apply(&mut s);
    assert_eq!(s.face, "tui", "a language change is not a face change");

    Change {
        face: Some(String::new()),
        ..Change::default()
    }
    .apply(&mut s);
    assert_eq!(s.face, "", "and it can be handed back");
}

/// The shape of the list survives a write from a frontend that has never heard of
/// it, and `""` stays distinct from `detail`.
#[test]
fn the_list_shape_outlives_a_frontend_that_does_not_know_it() {
    let mut s = Settings::default();
    assert_eq!(s.layout, "", "nobody has chosen, to start with");

    Change {
        layout: Some("large".into()),
        ..Change::default()
    }
    .apply(&mut s);
    assert_eq!(s.layout, "large");

    // A terminal saving its sort order says nothing about the shape.
    Change {
        sort: Some("size".into()),
        ..Change::default()
    }
    .apply(&mut s);
    assert_eq!(s.layout, "large");

    // And it can be handed back.
    Change {
        layout: Some(String::new()),
        ..Change::default()
    }
    .apply(&mut s);
    assert_eq!(s.layout, "");
}

#[test]
fn the_same_search_twice_moves_up_rather_than_repeating() {
    let mut s = Settings::default();
    s.remember("bir");
    s.remember("iki");
    s.remember("bir");
    assert_eq!(s.history, ["bir", "iki"]);
}

#[test]
fn nothing_is_not_a_search() {
    let mut s = Settings::default();
    s.remember("");
    s.remember("   ");
    assert!(s.history.is_empty());
}

/// A terminal that has never heard of column widths saves what it does know, and
/// the window's layout is still there afterwards.
#[test]
fn one_frontend_saving_does_not_erase_another() {
    let mut s = Settings::default();
    Change {
        columns: Some(vec!["name".into(), "size".into()]),
        widths: Some(BTreeMap::from([("name".to_owned(), 240)])),
        view: BTreeMap::from([("web".to_owned(), serde_json::json!({ "icons": true }))]),
        ..Change::default()
    }
    .apply(&mut s);

    // The terminal knows about sort and history and nothing else.
    Change {
        sort: Some("size".into()),
        remember: Some("rapor".into()),
        ..Change::default()
    }
    .apply(&mut s);

    assert_eq!(s.columns, ["name", "size"], "columns survived");
    assert_eq!(s.widths.get("name"), Some(&240), "widths survived");
    assert_eq!(s.view["web"]["icons"], serde_json::json!(true));
    assert_eq!(s.sort, "size");
    assert_eq!(s.history, ["rapor"]);
}

/// A language chosen in one window survives another window saving — the field most
/// likely to be written by a frontend that knows nothing else.
#[test]
fn a_language_survives_another_frontend_saving() {
    let mut s = Settings::default();
    Change {
        language: Some("en".into()),
        ..Change::default()
    }
    .apply(&mut s);
    assert_eq!(s.language, "en");

    // Somebody else, writing a field that has nothing to do with language.
    Change {
        sort: Some("size".into()),
        remember: Some("rapor".into()),
        ..Change::default()
    }
    .apply(&mut s);
    assert_eq!(s.language, "en", "still English");

    // And back to "nobody has chosen", which is a choice a menu can offer
    // and is not the same as choosing English.
    Change {
        language: Some(String::new()),
        ..Change::default()
    }
    .apply(&mut s);
    assert!(s.language.is_empty());
}

/// A settings file written before this field existed still loads, and the
/// language it does not mention is unset rather than wrong.
#[test]
fn a_file_from_before_the_language_existed_is_not_in_english() {
    let s: Settings =
        serde_json::from_str(r#"{"columns":["name"],"sort":"modified"}"#).expect("parse");
    assert!(
        s.language.is_empty(),
        "unset, so the environment still decides"
    );
}

/// Frontend-local state merges key by key, and `null` takes one away.
#[test]
fn a_window_setting_one_thing_says_nothing_about_the_rest() {
    let mut s = Settings::default();
    Change {
        view: BTreeMap::from([(
            "web".to_owned(),
            serde_json::json!({ "icons": true, "side": false }),
        )]),
        ..Change::default()
    }
    .apply(&mut s);
    Change {
        view: BTreeMap::from([("web".to_owned(), serde_json::json!({ "side": true }))]),
        ..Change::default()
    }
    .apply(&mut s);
    assert_eq!(s.view["web"]["icons"], serde_json::json!(true), "untouched");
    assert_eq!(s.view["web"]["side"], serde_json::json!(true), "changed");

    Change {
        view: BTreeMap::from([("web".to_owned(), serde_json::json!({ "icons": null }))]),
        ..Change::default()
    }
    .apply(&mut s);
    assert!(
        !s.view["web"]
            .as_object()
            .expect("object")
            .contains_key("icons")
    );
    assert_eq!(
        s.view["web"]["side"],
        serde_json::json!(true),
        "still there"
    );

    // And one frontend's corner is not another's.
    Change {
        view: BTreeMap::from([("tui".to_owned(), serde_json::json!({ "side": false }))]),
        ..Change::default()
    }
    .apply(&mut s);
    assert_eq!(s.view["web"]["side"], serde_json::json!(true));
    assert_eq!(s.view["tui"]["side"], serde_json::json!(false));
}

/// An empty change is a change to nothing, not a reset.
#[test]
fn saying_nothing_changes_nothing() {
    let mut s = Settings {
        columns: vec!["name".into()],
        sort: "modified".into(),
        history: vec!["bir".into()],
        ..Settings::default()
    };
    let before = s.clone();
    assert!(Change::default().is_empty());
    Change::default().apply(&mut s);
    assert_eq!(s, before);
}

/// The cap is the service's job now, whatever a frontend sends.
#[test]
fn the_service_caps_the_history_whoever_writes_it() {
    let mut s = Settings::default();
    Change {
        history: Some((0..HISTORY * 3).map(|i| format!("q{i}")).collect()),
        ..Change::default()
    }
    .apply(&mut s);
    assert_eq!(s.history.len(), HISTORY);

    let mut s = Settings::default();
    for i in 0..HISTORY * 2 {
        Change {
            remember: Some(format!("q{i}")),
            ..Change::default()
        }
        .apply(&mut s);
    }
    assert_eq!(s.history.len(), HISTORY);
}

#[test]
fn the_list_stops_growing() {
    let mut s = Settings::default();
    for i in 0..HISTORY * 2 {
        s.remember(&format!("q{i}"));
    }
    assert_eq!(s.history.len(), HISTORY);
    assert_eq!(
        s.history[0],
        format!("q{}", HISTORY * 2 - 1),
        "newest first"
    );
}

/// Tabs are replaced whole and capped; an old file without them reads as none.
#[test]
fn tabs_are_kept_whole_and_capped() {
    let old: Settings = serde_json::from_str(r#"{"sort":"size"}"#).expect("parse");
    assert!(old.tabs.is_empty());
    assert_eq!(old.tab, 0);

    let mut s = Settings::default();
    let one = SearchTab {
        query: "rapor".into(),
        sort: "size".into(),
        descending: Some(true),
    };
    Change {
        tabs: Some(vec![one.clone(); TABS + 5]),
        tab: Some(3),
        ..Default::default()
    }
    .apply(&mut s);
    assert_eq!(s.tabs.len(), TABS);
    assert_eq!(s.tabs[0], one);
    assert_eq!(s.tab, 3);

    // Saying nothing about tabs leaves them.
    Change {
        sort: Some("name".into()),
        ..Default::default()
    }
    .apply(&mut s);
    assert_eq!(s.tabs.len(), TABS);
}

#[test]
fn memory_is_given_back_at_once_on_a_small_machine_unless_somebody_chose() {
    use std::time::Duration;
    let mut s = Settings::default();
    assert_eq!(s.release_after(4 << 30), Duration::from_secs(3));
    assert_eq!(s.release_after(32 << 30), Duration::from_secs(60));
    // Unknown memory is not a small machine.
    assert_eq!(s.release_after(0), Duration::from_secs(60));
    s.memory = "5m".into();
    assert_eq!(s.release_after(4 << 30), Duration::from_secs(300));
    s.memory = "now".into();
    assert_eq!(s.release_after(32 << 30), Duration::from_secs(3));
    let change = Change {
        memory: Some(String::new()),
        ..Change::default()
    };
    assert!(change.tunes() && !change.moves_sources());
    change.apply(&mut s);
    assert_eq!(s.release_after(32 << 30), Duration::from_secs(60));
}
