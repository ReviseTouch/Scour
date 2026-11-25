use super::*;
use crate::app::Panel;
use crate::v2::filters::tests::{cell_of, click, key, opened, screen};
use crate::v2::filters::{Asked, Slot};
use ratatui::crossterm::event::{KeyCode, KeyModifiers};

fn texts(app: &App) -> Vec<String> {
    app.lens_panel_lines(60)
        .1
        .into_iter()
        .map(|l| format!("{}|{}", l.text, l.key))
        .collect()
}

/// Opened on `query`, preset `i` counted `i * 10`.
fn answered(query: &str) -> App {
    let mut app = opened(query);
    for (i, ask) in app.take_lens_asks().into_iter().enumerate() {
        app.lens_counted(ask.tag, i as u64 * 10);
    }
    app
}

#[test]
fn the_presets_are_counted_one_count_each() {
    let mut app = opened("rapor size:");
    let asks = app.take_lens_asks();
    let queries: Vec<String> = asks
        .iter()
        .map(|a| match &a.asked {
            Asked::Count { query } => query.clone(),
            Asked::Facets { .. } => panic!("counts"),
        })
        .collect();
    assert_eq!(
        queries,
        [
            "rapor empty:",
            "rapor size:<100kb",
            "rapor size:100kb..10mb",
            "rapor size:10mb..1gb",
            "rapor size:>1gb"
        ]
    );
    assert_eq!(asks[3].slot, Slot::Size(3));
    for (i, ask) in asks.iter().enumerate() {
        app.lens_counted(ask.tag, i as u64 * 1000);
    }
    assert_eq!(
        texts(&app),
        [
            "  Empty|0",
            "  under 100 KB|1,000",
            "  100 KB to 10 MB|2,000",
            "  10 MB to 1 GB|3,000",
            "  over 1 GB|4,000"
        ]
    );
    let (_, lines) = app.lens_panel_lines(60);
    assert!(lines[0].dimmed, "nothing empty");
}

#[test]
fn a_preset_replaces_the_term_and_reopening_shows_it() {
    let mut app = answered("rapor size:");
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.query, "rapor empty:");
    assert_eq!(app.panel, Panel::None);

    let app = answered("rapor empty:");
    assert_eq!(
        app.v2.filters.view,
        Some(crate::v2::filters::View::Size),
        "empty: is the size panel's"
    );
    assert_eq!(texts(&app)[0], "✓ Empty|0");

    let mut app = answered("!size:>1gb");
    assert_eq!(texts(&app)[4], "✗ over 1 GB|40");
    app.panel_at = 1;
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.query, "!size:<100kb", "its ! kept");
}

#[test]
fn an_older_openings_count_is_dropped() {
    let mut app = opened("size:");
    let first = app.take_lens_asks();
    app.panel = Panel::None;
    app.open_context_lens();
    app.lens_counted(first[1].tag, 99);
    assert_eq!(app.v2.filters.sizes[1], None);
}

#[test]
fn a_click_on_a_preset_writes_it() {
    for size in [(100, 30), (80, 30)] {
        let mut app = answered("rapor size:");
        let shot = screen(&app, size.0, size.1);
        assert!(
            shot.iter()
                .any(|l| l.contains("under 100 KB") && l.contains("10"))
        );
        let (c, r) = cell_of(&app, size, 4, 3);
        click(&mut app, c, r, size, KeyModifiers::NONE);
        assert_eq!(app.query, "rapor size:>1gb", "{size:?}");
    }
}

#[test]
fn the_size_rail_offers_empty_not_everything() {
    let app = App::default();
    let terms: Vec<String> = app.rail_lines().into_iter().map(|(_, t)| t).collect();
    assert!(terms.iter().any(|t| t == "empty:"), "{terms:?}");
    assert!(!terms.iter().any(|t| t == "size:0"), "size:0 is every size");
}
