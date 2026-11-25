use super::super::place::tests::{enter, key, machine, texts};
use super::*;
use crate::app::Panel;

/// The system disk held whole, a folder, and one left out.
fn searching(query: &str) -> App {
    let (mut app, sources) = machine();
    app.query = query.into();
    app.caret = query.len();
    app.open_place_lens();
    app.sources_came(1, sources);
    app
}

const SYSTEM: &str = "under:/etc|under:~|under:/opt|under:/usr|under:/var";

#[test]
fn the_line_says_a_disk_by_name_a_folder_in_home_form_and_what_is_left_out() {
    let app = searching(&format!("rapor {SYSTEM} !under:/tmp"));
    assert_eq!(texts(&app, 80)[0], "Searching in: 1 CACHY  ·  2 !/tmp|");

    let h = home();
    let app = searching(&format!("under:{h}/Belgeler|under:/mnt/depo"));
    assert_eq!(texts(&app, 80)[0], "Searching in: 1 ~/Belgeler  ·  2 Depo|");
    // Too long for the line: its end is given up, not wrapped.
    let narrow = &texts(&app, 30)[0];
    assert!(
        narrow.ends_with("…|") && narrow.chars().count() == 31,
        "{narrow}"
    );
}

#[test]
fn a_digit_on_the_line_takes_its_spot_out() {
    let mut app = searching(&format!("rapor {SYSTEM} !under:/tmp"));
    assert_eq!(app.panel_at, 0, "the lens opens on the line");
    assert!(matches!(key(&mut app, '1'), Want::Page { .. }));
    assert_eq!(app.query, "rapor !under:/tmp");
    assert_eq!(app.panel, Panel::Lens, "the lens stays to show it");
    assert_eq!(texts(&app, 80)[0], "Searching in: 1 !/tmp|");
    // A digit with no spot writes nothing, nor does Enter on the line.
    assert_eq!(key(&mut app, '7'), Want::Nothing);
    assert_eq!(enter(&mut app), Want::Nothing);
    assert_eq!(app.query, "rapor !under:/tmp");
    key(&mut app, '1');
    assert_eq!(app.query, "rapor");
    assert_eq!(texts(&app, 80)[0], "Searching in: Everywhere|");

    // Off the line a digit is typed into the query, as before.
    app.panel_at = 1;
    key(&mut app, '2');
    assert_eq!(app.query, "rapor2");
}

#[test]
fn taking_out_one_folder_of_several_keeps_the_rest_together() {
    let h = home();
    let mut app = searching(&format!(
        "rapor under:{h}/Belgeler|under:/mnt/depo|under:/usr"
    ));
    key(&mut app, '2');
    assert_eq!(app.query, format!("rapor under:{h}/Belgeler|under:/usr"));
    assert_eq!(&app.query[..app.caret], format!("rapor under:{h}/Belgeler"));
}
