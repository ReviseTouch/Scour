use super::*;
use crate::app::Panel;
use crate::v2::filters::tests::{cell_of, click, key, opened, reply, screen};
use ratatui::crossterm::event::KeyModifiers;

fn kinds() -> FacetResponse {
    reply(
        vec![
            (
                FacetBy::Kind,
                vec![("doc", 60), ("code", 30), ("image", 10)],
            ),
            (FacetBy::Age { edges: vec![0] }, vec![]),
        ],
        100,
    )
}

fn exts() -> FacetResponse {
    reply(
        vec![
            (FacetBy::Ext { top: EXT_TOP }, vec![("rs", 20), ("md", 5)]),
            (FacetBy::Age { edges: vec![0] }, vec![]),
        ],
        25,
    )
}

/// The lens on `query`, both lists answered.
fn answered(query: &str) -> App {
    let mut app = opened(query);
    for ask in app.take_lens_asks() {
        let r = if ask.slot == Slot::Kinds {
            kinds()
        } else {
            exts()
        };
        app.lens_faceted(ask.tag, &r);
    }
    app
}

fn texts(app: &App) -> Vec<String> {
    app.lens_panel_lines(60)
        .1
        .into_iter()
        .map(|l| format!("{}|{}", l.text, l.key))
        .collect()
}

fn slots(app: &mut App) -> Vec<Slot> {
    app.take_lens_asks().into_iter().map(|a| a.slot).collect()
}

#[test]
fn the_kinds_are_counted_with_a_share_and_the_extensions_under_them() {
    let mut app = opened("rapor kind:");
    let asks = app.take_lens_asks();
    let queries: Vec<(Slot, String)> = asks
        .iter()
        .map(|a| match &a.asked {
            Asked::Facets { query, .. } => (a.slot, query.clone()),
            Asked::Count { .. } => panic!("facets"),
        })
        .collect();
    assert_eq!(
        queries,
        [
            (Slot::Kinds, "rapor".to_string()),
            (Slot::Exts, "rapor kind:".to_string())
        ]
    );
    assert_eq!(app.lens_len(), KIND_ROWS, "no extension before the answer");
    for ask in asks {
        let r = if ask.slot == Slot::Kinds {
            kinds()
        } else {
            exts()
        };
        app.lens_faceted(ask.tag, &r);
    }
    let t = texts(&app);
    assert_eq!(t[0], format!("  {:<13}  {}|0  !", "Folder", " ".repeat(10)));
    assert_eq!(
        t[2],
        format!("  {:<13}  {}|60   ", "Document", "██████    ")
    );
    assert_eq!(t[3], format!("  {:<13}  {}|10   ", "Image", "█         "));
    assert_eq!(
        t[KIND_ROWS],
        format!("  {:<13}  {}|20   ", ".rs", "████████  ")
    );
    let (_, lines) = app.lens_panel_lines(60);
    assert!(lines[0].dimmed, "nothing of that kind");
    assert_eq!(lines[KIND_ROWS].head, "Extension");
}

#[test]
fn a_press_puts_a_value_in_and_bang_leaves_it_out() {
    let mut app = answered("rapor kind:");
    app.panel_at = 2;
    key(&mut app, KeyCode::Char(' '));
    assert_eq!(app.query, "rapor kind:doc");
    assert_eq!(app.panel, Panel::Lens, "the panel stays for the next");
    assert_eq!(
        slots(&mut app),
        [Slot::Exts],
        "the kinds' query is the same"
    );
    assert!(texts(&app)[2].starts_with("✓ Document"));

    app.panel_at = 11;
    key(&mut app, KeyCode::Char('!'));
    assert_eq!(app.query, "rapor kind:doc !kind:build");
    assert_eq!(slots(&mut app), [Slot::Exts]);
    let (_, lines) = app.lens_panel_lines(60);
    assert!(lines[11].text.starts_with("✗ Build output") && lines[11].careful);

    app.panel_at = KIND_ROWS;
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.query, "rapor kind:doc !kind:build ext:rs");
    assert_eq!(
        slots(&mut app),
        [Slot::Kinds],
        "the extensions' query is the same"
    );
    assert!(texts(&app)[KIND_ROWS].starts_with("✓ .rs"));
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.query, "rapor kind:doc !kind:build", "and out again");
}

#[test]
fn an_answer_to_a_list_asked_again_is_dropped() {
    let mut app = opened("rapor kind:");
    let first = app.take_lens_asks();
    app.panel_at = 2;
    key(&mut app, KeyCode::Enter);
    let again = app.take_lens_asks();
    app.lens_faceted(first[1].tag, &exts());
    assert_eq!(app.v2.filters.exts, None, "asked over rapor kind:, now old");
    app.lens_faceted(again[0].tag, &exts());
    assert!(app.v2.filters.exts.is_some());
    app.lens_faceted(first[0].tag, &kinds());
    assert!(
        app.v2.filters.kinds.is_some(),
        "the kinds were not asked again"
    );
}

#[test]
fn ext_opens_the_same_panel_on_its_extensions() {
    let mut app = opened("rapor ext:");
    assert_eq!(app.v2.filters.view, Some(crate::v2::filters::View::Type));
    for ask in app.take_lens_asks() {
        let r = if ask.slot == Slot::Kinds {
            kinds()
        } else {
            exts()
        };
        app.lens_faceted(ask.tag, &r);
    }
    assert_eq!(app.panel_at, KIND_ROWS, "on the first extension");
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.query, "rapor ext:rs");
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.panel_at, 0, "Tab goes to the kinds");
    key(&mut app, KeyCode::Tab);
    assert_eq!(app.panel_at, KIND_ROWS);
}

#[test]
fn a_click_toggles_the_value_drawn_under_it() {
    for size in [(100, 34), (80, 34)] {
        let mut app = answered("rapor kind:doc");
        let (c, r) = cell_of(&app, size, KIND_ROWS, 2);
        click(&mut app, c, r, size, KeyModifiers::NONE);
        assert_eq!(app.query, "rapor kind:doc ext:rs", "{size:?}");
        let shot = screen(&app, size.0, size.1);
        assert!(shot.iter().any(|l| l.contains("✓ Document")), "{size:?}");
        assert!(shot.iter().any(|l| l.contains("✓ .rs")), "{size:?}");
        assert!(shot.iter().any(|l| l.contains("Extension")), "{size:?}");
    }
}

#[test]
fn a_share_is_drawn_to_the_eighth() {
    assert_eq!(share(0, 100), " ".repeat(10));
    assert_eq!(
        share(1, 1000),
        format!("▏{}", " ".repeat(9)),
        "never nothing"
    );
    assert_eq!(share(55, 100), "█████▌    ");
    assert_eq!(share(5, 0), " ".repeat(10));
}
