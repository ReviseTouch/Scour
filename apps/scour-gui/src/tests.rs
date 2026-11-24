/// What may be added as a folder: a real one, absolute or from `~`, not
/// twice. Each refusal is a sentence the catalogue knows.
#[test]
fn an_added_folder_must_be_a_real_one() {
    let tmp = std::env::temp_dir();
    let path = scour_core::path::from_path(&tmp);
    let path = path.trim_end_matches('/').to_owned();
    assert_eq!(
        super::folder_to_add(&format!("  {path}/ "), &[]),
        Ok(path.clone())
    );
    assert_eq!(
        super::folder_to_add(&path, std::slice::from_ref(&path)),
        Err("That folder is already in the list.")
    );
    assert_eq!(
        super::folder_to_add("relative/dir", &[]),
        Err("A folder's full path, from the root.")
    );
    assert_eq!(
        super::folder_to_add("/no/such/folder/here", &[]),
        Err("There is no folder there.")
    );
    assert_eq!(
        super::folder_to_add("   ", &[]),
        Err("Type a folder's path first.")
    );
    assert!(super::folder_to_add("~", &[]).is_ok_and(|p| p.starts_with('/')));
}

/// The preview is bounded whatever the service sends, and the cut is checked
/// for being a real string.
#[test]
fn a_preview_is_bounded_however_large_the_file_is() {
    let short = "fn main() {}\n";
    assert_eq!(
        super::peek_head(short, 1.0),
        short,
        "a small file is untouched"
    );

    // A quarter megabyte, which is what the service actually sends.
    let big: String = "lorem ipsum dolor sit amet\n".repeat(10_000);
    let cut = super::peek_head(&big, 1.0);
    assert!(cut.len() < big.len() / 4, "{} of {}", cut.len(), big.len());
    assert!(cut.ends_with('…'), "the reader is told it was cut");
    assert!(cut.lines().count() <= 402);

    // One enormous line: the line cap cannot help and the character cap must.
    let one: String = "x".repeat(300_000);
    assert!(super::peek_head(&one, 1.0).chars().count() <= 20_002);
    // Three times the scale, a third of the text.
    assert!(super::peek_head(&one, 3.0).chars().count() <= 6_669);
}

/// A cut that lands inside a multi-byte character must not panic.
#[test]
fn the_cut_falls_on_a_character_boundary() {
    let turkish: String = "çğıöşü ".repeat(9_000);
    let cut = super::peek_head(&turkish, 1.0);
    assert!(cut.chars().count() <= 20_002);
    assert!(!cut.is_empty());
}

use super::*;

/// The window's palette is the browser page's. What this checks is the
/// conversion: Slint takes alpha first and the browser takes it last.
#[test]
fn the_window_wears_the_shared_palette() {
    let dark = scheme(&scour_ui::DARK);
    let want = |c: &scour_ui::Rgba| {
        let (a, r, g, b) = c.argb();
        slint::Brush::SolidColor(slint::Color::from_argb_u8(a, r, g, b))
    };
    assert_eq!(dark.ground, want(&scour_ui::DARK.ground));
    assert_eq!(dark.q_key, want(&scour_ui::DARK.q_key));
    // The translucent one, which is the case a channel swap survives.
    assert_eq!(dark.pick, want(&scour_ui::DARK.pick));
    assert_eq!(dark.mark, want(&scour_ui::DARK.mark));

    let light = scheme(&scour_ui::LIGHT);
    assert_ne!(light.ground, dark.ground, "both schemes came out the same");
}

#[test]
fn only_plain_words_are_highlighted() {
    // `ext:pdf` matched a column, not a run of the name.
    assert_eq!(terms_of("rapor ext:pdf"), vec!["rapor".to_string()]);
    assert_eq!(terms_of("!eski rapor"), vec!["rapor".to_string()]);
    // Quotes hold a phrase together: one mark, not two.
    assert_eq!(terms_of("\"iki kelime\""), vec!["iki kelime".to_string()]);
    assert_eq!(
        terms_of("\"a:b\""),
        vec!["a:b".to_string()],
        "quoted, so not a field"
    );
    assert!(terms_of("kind:image").is_empty());
}

/// The coloured layer spells what is in the box, never what was.
#[test]
fn a_deleted_character_is_not_drawn_by_the_spans_that_still_hold_it() {
    use scour_core::{Role, Span as Run};
    // What `rapor pdf` came back as, now laid over `rapor p`.
    let spans = [
        Run::new(0, 5, Role::Text),
        Run::new(5, 1, Role::Space),
        Run::new(6, 3, Role::Text),
    ];
    let runs = painted("rapor p", &spans);
    let whole: String = runs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(whole, "rapor p", "the layer spelled something else");
    assert_eq!(runs.last().unwrap().at, 6, "the last run is misplaced");
}

/// And what was typed since is drawn rather than left out.
#[test]
fn a_character_typed_since_the_spans_were_asked_for_is_still_drawn() {
    use scour_core::{Role, Span as Run};
    let spans = [Run::new(0, 5, Role::Text)];
    let runs = painted("rapors", &spans);
    let whole: String = runs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(whole, "rapors");
}

/// Offsets are bytes on the wire and characters on the screen.
#[test]
fn a_run_after_a_turkish_word_is_placed_by_characters() {
    use scour_core::{Role, Span as Run};
    // `değiştirme` is ten characters and twelve bytes.
    let q = "değiştirme rapor";
    let spans = [
        Run::new(0, 12, Role::Text),
        Run::new(12, 1, Role::Space),
        Run::new(13, 5, Role::Text),
    ];
    let runs = painted(q, &spans);
    assert_eq!(runs[2].at, 11, "the run after it is placed by bytes");
    let whole: String = runs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(whole, q);
}

/// A span the query no longer reaches at all.
#[test]
fn spans_past_the_end_are_dropped_rather_than_panicking() {
    use scour_core::{Role, Span as Run};
    let spans = [Run::new(0, 5, Role::Text), Run::new(40, 9, Role::Value)];
    let runs = painted("rap", &spans);
    let whole: String = runs.iter().map(|r| r.text.as_str()).collect();
    assert_eq!(whole, "rap");
}

#[test]
fn expensive_refresh_waits_but_a_new_scroll_page_does_not() {
    use std::time::Duration;
    let rows = rows::Rows::default();
    rows.put(0, vec![Row::default(); rows::SPAN], Vec::new(), 10_000);
    rows.mark(1);
    let early = refresh_ready(Some(Duration::from_secs(5)), 1_000_000);
    assert!(!early);
    assert_eq!(rows.next_page(0, 40, false, early), None);
    assert_eq!(rows.next_page(400, 440, false, early), Some(2));
    let rested = refresh_ready(Some(Duration::from_secs(10)), 1_000_000);
    assert_eq!(rows.next_page(0, 40, false, rested), Some(0));
    assert!(!refresh_ready(Some(Duration::from_millis(499)), 1_000));
    assert!(refresh_ready(Some(Duration::from_millis(500)), 1_000));
}

#[test]
fn a_layout_miss_from_the_previous_viewport_cannot_redirect_a_scroll() {
    assert_eq!(fetch_from(10_000, 40, Some(400)), 10_000);
    assert_eq!(fetch_from(10_000, 40, Some(10_005)), 10_005);
    assert_eq!(fetch_from(0, 40, Some(10_005)), 0);
    assert_eq!(fetch_from(10_000, 40, None), 10_000);
}

#[test]
fn the_rail_composes_with_the_text_rather_than_replacing_it() {
    let mut s = State {
        qb: Default::default(),
        grep: Default::default(),
        similar: String::new(),
        mounts: Vec::new(),
        indexed: 0,
        stirred: std::time::Instant::now(),
        refresh: std::time::Duration::from_secs(2),
        dozing: false,
        generation: 0,
        query_revision: 0,
        past: Vec::new(),
        spans: Vec::new(),
        background_query: None,
        count_query: None,
        exact_count: None,
        row_limit: 20,
        page_offset: 0,
        page_sent: None,
        page_landed_at: None,
        page_cost_us: 0,
        asking_pictures: false,
        peek_path: String::new(),
        asked_pictures: std::collections::VecDeque::new(),
        rewind: false,
        search_out: false,
        typed_at: None,
        shown: 0,
        query: "rapor".into(),
        sort: "relevance".into(),
        descending: true,
        added_paths: Vec::new(),
        added_dirs: Vec::new(),
        added_files: Vec::new(),
        sent_off: None,
        disks_asking: None,
        asked_disk: String::new(),
        rules_shown: Default::default(),
        exclude_off: Vec::new(),
        scope: String::new(),
        hits: Vec::new(),
        revision: 0,
        down: false,
    };
    assert_eq!(full_query(&s), "rapor");
    // A pressed term is in the text, so the engine is asked the text.
    s.query = scour_ui::query::toggle(&s.query, "dm:38d");
    assert_eq!(full_query(&s), "rapor dm:38d");
    // The rail is counted over the text; `scour_query::without` takes the
    // rail's own field out before it is asked.
    assert_eq!(facet_query(&s), "rapor dm:38d");
}

/// **The doze is a clock, and the clock is the whole feature.** Both edges.
#[test]
fn a_window_dozes_only_after_a_minute_untouched_and_wakes_on_a_touch() {
    let long_ago = std::time::Instant::now() - AWAKE_FOR - std::time::Duration::from_secs(1);
    let just_now = std::time::Instant::now();

    // Untouched for longer than the window stays awake: it dozes.
    assert!(long_ago.elapsed() >= AWAKE_FOR, "the test's own premise");
    // Touched within it: it does not.
    assert!(just_now.elapsed() < AWAKE_FOR);
    // A doze asks far less often than an awake window, and not never.
    assert!(
        DOZE_AGAIN > AWAIT_AGAIN * 8,
        "a doze that asks as often is not a doze"
    );
    assert!(
        DOZE_AGAIN < AWAKE_FOR,
        "a doze must refresh before it could wake"
    );
}

#[test]
fn sorting_reuses_query_scoped_sidebar_and_count_work() {
    let mut s = State {
        qb: Default::default(),
        grep: Default::default(),
        similar: String::new(),
        mounts: Vec::new(),
        stirred: std::time::Instant::now(),
        refresh: std::time::Duration::from_secs(2),
        dozing: false,
        sent_off: None,
        disks_asking: None,
        asked_disk: String::new(),
        rules_shown: Default::default(),
        indexed: 0,
        generation: 4,
        query_revision: 2,
        past: Vec::new(),
        spans: Vec::new(),
        background_query: None,
        count_query: None,
        exact_count: Some(ExactCount {
            query_revision: 2,
            total: 45,
            capped: false,
        }),
        row_limit: 40,
        page_offset: 0,
        page_sent: None,
        page_landed_at: None,
        page_cost_us: 0,
        asking_pictures: false,
        peek_path: String::new(),
        asked_pictures: std::collections::VecDeque::new(),
        rewind: false,
        search_out: false,
        typed_at: None,
        shown: 4,
        query: "rapor".into(),
        sort: "modified".into(),
        descending: true,
        added_paths: Vec::new(),
        added_dirs: Vec::new(),
        added_files: Vec::new(),
        exclude_off: Vec::new(),
        scope: String::new(),
        hits: Vec::new(),
        revision: 0,
        down: false,
    };

    assert!(s.start_background());
    assert!(s.start_count());
    assert!(
        !s.start_count(),
        "one capped facet answer gets one fallback"
    );
    s.advance_order();
    assert!(!s.start_background(), "a sort did not change the matches");
    assert_eq!(s.query_revision, 2);
    assert_eq!(s.exact_count.map(|c| c.total), Some(45));

    s.advance_query();
    assert!(s.start_background(), "a new query needs new facets");
    assert!(
        s.start_count(),
        "a new query may need its own fallback count"
    );
    assert!(s.exact_count.is_none());
}

#[test]
fn every_heading_carries_the_sort_key_the_shared_crate_names() {
    let cat = Catalogue::for_language("en");
    // Every column the table can offer, not only the ones showing.
    let table = Table {
        shown: scour_ui::COLUMNS.iter().collect(),
        widths: Default::default(),
        need: Default::default(),
    };
    let heads = heads_of(&table, &cat);
    assert_eq!(heads.len(), scour_ui::COLUMNS.len());
    for (h, c) in heads.iter().zip(scour_ui::COLUMNS) {
        assert_eq!(h.id, c.id);
        assert_eq!(h.sort, c.sort, "`{}` asks the wrong sort key", c.id);
        // The word comes from the crate rather than from this file.
        assert!(!h.label.is_empty(), "`{}` has no heading word", c.id);
    }
}

#[test]
fn a_column_switched_on_lands_where_the_table_says_and_moves_nothing() {
    let col = |id: &str| scour_ui::column(id).unwrap();
    let ids = |t: &Table| t.ids().join(",");

    let mut t = Table {
        // Deliberately not the table's own order.
        shown: vec![col("size"), col("name")],
        widths: Default::default(),
        need: Default::default(),
    };
    // `path` outranks `size` in `COLUMNS`, so it lands in front of it.
    t.toggle("path");
    assert_eq!(ids(&t), "path,size,name");

    // Off is off, and the width it was dragged to stays behind.
    t.toggle("size");
    assert_eq!(ids(&t), "path,name");

    // **The last one standing cannot be turned off**: a table of no columns is
    // broken, not smaller.
    t.toggle("path");
    assert_eq!(ids(&t), "name");
    t.toggle("name");
    assert_eq!(ids(&t), "name");

    // An id from a newer version, or a typo, changes nothing at all.
    t.toggle("zurna");
    assert_eq!(ids(&t), "name");
}

#[test]
fn a_saved_column_list_is_read_back_and_a_broken_one_is_not_obeyed() {
    let read = |cols: &[&str]| {
        Table::read(&scour_settings::Settings {
            columns: cols.iter().map(|s| (*s).to_string()).collect(),
            ..Default::default()
        })
        .ids()
        .join(",")
    };
    assert_eq!(read(&["kind", "name"]), "kind,name");
    // A column this build lacks is dropped rather than the whole list refused.
    assert_eq!(read(&["kind", "zurna", "name"]), "kind,name");
    // A list with nothing left cannot be right, so the default stands.
    assert_eq!(read(&["zurna"]), COLUMN_DEFAULT.join(","));
    assert_eq!(read(&[]), COLUMN_DEFAULT.join(","));
}

#[test]
fn a_page_that_comes_back_short_is_the_end_of_the_result() {
    // A full page in the middle of a long result: the count says how long.
    assert_eq!(list_length(2_500_000, 400, 200, 200, 200), 2_500_000);
    // A result that shrank while somebody scrolled to it: believing the count
    // leaves six rows asked for for ever.
    assert_eq!(list_length(979, 779, 194, 200, 200), 973);
    // The first page of a new query is short, and is not an ending.
    assert_eq!(list_length(2_500_000, 0, 24, 24, 200), 2_500_000);
    // Nor is a service whose own page ceiling is below the request.
    assert_eq!(list_length(5_000, 0, 200, 256, 200), 5_000);
    // A count shorter than what is in hand cannot hide the loaded rows.
    assert_eq!(list_length(10, 400, 200, 200, 200), 600);
}

#[test]
fn a_term_too_short_to_narrow_is_not_ranked() {
    // At `t` relevance walks a million rows for an ordering nobody can read.
    assert_eq!(order_for("t", "relevance"), "modified");
    assert_eq!(order_for("to", "relevance"), "modified");
    assert_eq!(order_for("tok", "relevance"), "relevance");
    // The shortest term decides.
    assert_eq!(order_for("rapor t", "relevance"), "modified");
    // A field term is not a name term and does not count either way.
    assert_eq!(order_for("kind:image", "relevance"), "relevance");
    // And a sort somebody asked for out loud is left alone.
    assert_eq!(order_for("t", "size"), "size");
}

#[test]
fn counts_stay_narrow_enough_for_the_rail() {
    assert_eq!(compact(7), "7");
    assert_eq!(compact(9_999), "9999");
    assert_eq!(compact(12_345), "12k");
    assert_eq!(compact(2_951_074), "3.0M");
}

#[test]
fn every_offered_kind_has_a_word_in_every_translated_language() {
    // **`!c.get(k.msgid()).is_empty()` could not fail**: `get` falls back to
    // the msgid, so `has` asks the catalogue. Only translated languages.
    let translated: Vec<Catalogue> = scour_i18n::LANGUAGES
        .iter()
        .map(|(tag, _)| Catalogue::for_language(tag))
        .filter(Catalogue::is_translated)
        .collect();
    assert!(!translated.is_empty(), "no shipped language is translated");
    for c in translated {
        for k in scour_core::Kind::OFFERED {
            assert!(
                c.has(k.msgid()),
                "{} has no word for {k:?} ({})",
                c.locale(),
                k.msgid()
            );
        }
    }
}

/// What the window opens at, which is the whole of this decision: the
/// screen only ever takes room away, and never below the minimum.
#[test]
fn the_window_opens_where_it_was_left_unless_the_screen_is_smaller() {
    use super::{DEFAULT_SIZE, NO_SCREEN_IS_BIGGER, SMALLEST_SIZE, opening_size};
    let screen = Some((1920.0, 1080.0));

    assert_eq!(
        opening_size(Some((1440.0, 900.0)), screen),
        (1440.0, 900.0),
        "a size that fits is the size"
    );
    assert_eq!(
        opening_size(Some((3000.0, 2000.0)), screen),
        (1920.0, 1080.0),
        "a size kept on a wider screen is brought back inside this one"
    );
    assert_eq!(
        opening_size(None, screen),
        DEFAULT_SIZE,
        "nothing kept is the default"
    );
    assert_eq!(
        opening_size(None, Some((800.0, 600.0))),
        (800.0, 600.0),
        "the default fits the screen too"
    );
    assert_eq!(
        opening_size(Some((1440.0, 900.0)), Some((320.0, 200.0))),
        SMALLEST_SIZE,
        "a screen under the window's minimum does not shrink it further"
    );
    // No window has been made yet, so no monitor can be asked.
    assert_eq!(
        opening_size(Some((1440.0, 900.0)), None),
        (1440.0, 900.0),
        "an unknown screen takes nothing away"
    );
    assert_eq!(
        opening_size(Some((99_000.0, 99_000.0)), None),
        NO_SCREEN_IS_BIGGER,
        "but a nonsense size is still bounded"
    );
}

/// The settings are a free-form object written by another program's idea of
/// this key; everything that is not two positive numbers is no opinion.
#[test]
fn only_two_positive_numbers_count_as_a_remembered_size() {
    use super::kept_size;
    let read = |text: &str| kept_size(Some(&serde_json::from_str(text).unwrap()));

    assert_eq!(read(r#"{"size":[1180,760]}"#), Some((1180.0, 760.0)));
    assert_eq!(read(r#"{"size":[1180.5,760.5]}"#), Some((1180.5, 760.5)));
    assert_eq!(
        read(r#"{"peek":true,"size":[900,700]}"#),
        Some((900.0, 700.0)),
        "the size is one key among whatever else is kept there"
    );
    assert_eq!(kept_size(None), None, "no corner of its own yet");
    for junk in [
        r#"{}"#,
        r#"{"size":[]}"#,
        r#"{"size":[1180]}"#,
        r#"{"size":[1180,760,40]}"#,
        r#"{"size":[0,760]}"#,
        r#"{"size":[-1180,760]}"#,
        r#"{"size":["1180","760"]}"#,
        r#"{"size":{"w":1180,"h":760}}"#,
        r#"{"size":null}"#,
    ] {
        assert_eq!(read(junk), None, "{junk}");
    }
}
