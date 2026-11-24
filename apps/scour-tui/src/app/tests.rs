use super::*;

/// An app with a screen to put rows on. `App::default()` has no room, and
/// a view zero rows tall answers every scrolling question the same way.
fn app(room: usize) -> App {
    App {
        room,
        ..Default::default()
    }
}

fn hits(from: usize, n: usize) -> Vec<Hit> {
    (from..from + n)
        .map(|i| {
            let path = format!("/x/{i}");
            Hit {
                id: scour_core::EntryId::path_hash(scour_core::SourceId(0), &path),
                path,
                is_dir: false,
                kind: scour_core::Kind::File,
                meta: scour_core::Meta::UNKNOWN,
                under: None,
                found: Vec::new(),
            }
        })
        .collect()
}

/// The order chosen in another face is the one this one opens in, and a
/// column picked here is written once, not on every key after it.
#[test]
fn the_order_is_kept_for_every_face() {
    let mut a = app(10);
    let kept = scour_settings::Settings {
        sort: "size".into(),
        descending: Some(false),
        ..Default::default()
    };
    a.order_from(&kept);
    assert_eq!((a.sort, a.descending), (SortKey::Size, false));
    assert!(
        a.order_to_keep().is_none(),
        "what was read is not written back"
    );

    let name = a
        .columns
        .iter()
        .position(|c| c.sort == "name")
        .expect("the name column");
    a.sort_by(name);
    let change = a.order_to_keep().expect("a new order is kept");
    assert_eq!(change.sort.as_deref(), Some("name"));
    assert_eq!(change.descending, Some(false), "a word starts from A");
    assert!(a.order_to_keep().is_none(), "kept once");

    a.flip();
    assert_eq!(a.order_to_keep().and_then(|c| c.descending), Some(true));

    // Nothing kept, or a word from elsewhere: newest first.
    let mut b = app(10);
    b.order_from(&scour_settings::Settings {
        sort: "changed".into(),
        ..Default::default()
    });
    assert_eq!((b.sort, b.descending), (SortKey::Modified, true));
}

fn reply(hits: Vec<Hit>, total: u64) -> scour_core::SearchResponse {
    scour_core::SearchResponse {
        hits,
        total,
        ..scour_core::SearchResponse::default()
    }
}

#[test]
fn typing_asks_for_the_first_page_of_the_new_question() {
    let mut app = App::default();
    let want = app.insert('a');
    assert!(matches!(
        want,
        Want::Page {
            offset: 0,
            generation: 1,
            ..
        }
    ));
    assert_eq!(app.query, "a");
}

#[test]
fn an_answer_to_an_older_keystroke_is_dropped() {
    let mut app = App::default();
    app.insert('a');
    app.insert('b');
    // The answer to `a`, arriving after `ab` went out.
    let want = app.landed(1, 0, 200, reply(hits(0, 200), 900));
    assert_eq!(want, Want::Nothing);
    assert_eq!(app.pages.total(), 0, "and nothing of it is kept");
}

#[test]
fn the_cursor_pushes_the_view_the_least_it_can() {
    let mut app = app(10);
    app.insert('a');
    app.landed(1, 0, 200, reply(hits(0, 200), 1_000));
    app.go(0);
    assert_eq!(app.top, 0);
    app.walk(9);
    assert_eq!(app.top, 0, "still on screen");
    app.walk(1);
    assert_eq!(app.top, 1, "one row, not a page");
    app.go(0);
    assert_eq!(app.top, 0, "and back the same way");
}

#[test]
fn the_view_never_shows_past_the_end() {
    let mut app = app(10);
    app.insert('a');
    app.landed(1, 0, 200, reply(hits(0, 40), 40));
    app.go(usize::MAX);
    assert_eq!(app.cursor, 39);
    assert_eq!(app.top, 30, "the last screenful, not past it");
}

#[test]
fn a_short_page_is_the_end_of_the_result() {
    let mut app = app(10);
    app.insert('a');
    // Asked for 200, given 40: there is no more.
    app.landed(1, 0, 200, reply(hits(0, 40), 1_000));
    assert_eq!(app.pages.total(), 1_000, "the count is still the count");
    // And a page that fills is not the end.
    app.landed(1, 0, 200, reply(hits(0, 200), 5_000));
    assert_eq!(app.pages.total(), 5_000);
}

/// A row number is not an identity: the list shifts down by one and the
/// cursor has to shift with it.
#[test]
fn the_cursor_stays_on_the_file_when_the_list_moves_under_it() {
    let mut app = app(10);
    app.insert('a');
    app.landed(1, 0, 200, reply(hits(0, 200), 1_000));
    app.go(3);
    let was = app.cursor_at.clone();
    assert_eq!(was.as_deref(), Some("/x/3"));

    // A file appears at the top: every row moves down one.
    let mut shifted = hits(0, 199);
    shifted.insert(0, hits(999, 1)[0].clone());
    app.landed(1, 0, 200, reply(shifted, 1_000));

    assert_eq!(app.cursor, 4, "the cursor moved with the row");
    assert_eq!(
        app.cursor_at.as_deref(),
        Some("/x/3"),
        "and it is the same file"
    );
    // The view stays where it is, so the row slides down a line and the
    // file that arrived is drawn above it.
    assert_eq!(app.top, 0);
    assert_eq!(app.cursor - app.top, 4, "one line further down the screen");
}

/// An untouched cursor stays at the top of the list: the other half of the
/// rule, without which every file saved walks it down the screen.
#[test]
fn a_cursor_nobody_moved_belongs_to_the_list_rather_than_to_a_file() {
    let mut app = app(10);
    app.insert('a');
    app.landed(1, 0, 200, reply(hits(0, 200), 1_000));
    assert_eq!(app.cursor, 0);

    let mut shifted = hits(0, 199);
    shifted.insert(0, hits(999, 1)[0].clone());
    app.landed(1, 0, 200, reply(shifted, 1_000));

    assert_eq!(app.cursor, 0, "still the top");
    assert_eq!(app.top, 0, "and the newest row is on it");
    assert_eq!(
        app.pages.at(0).map(|h| h.path.as_str()),
        Some("/x/999"),
        "which is the file that just arrived"
    );
}

#[test]
fn backspace_cuts_characters_rather_than_bytes() {
    let mut app = App::default();
    for c in "Değ".chars() {
        app.insert(c);
    }
    app.backspace();
    assert_eq!(app.query, "De", "and did not panic on the ğ");
}
