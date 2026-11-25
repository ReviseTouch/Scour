use super::*;
use ratatui::crossterm::event::{KeyEvent, MouseButton, MouseEvent, MouseEventKind};
use scour_core::{Facet, FacetGroup};

/// 2026-11-25 12:00 UTC, a Wednesday.
pub(crate) const NOW: i64 = 1_795_608_000;

/// The query, the caret at its end, the lens opened as Ctrl+F opens it,
/// counted from [`NOW`] in UTC.
pub(crate) fn opened(query: &str) -> App {
    let mut app = App {
        query: query.into(),
        caret: query.len(),
        ..App::default()
    };
    app.open_context_lens();
    app.v2.filters.now = NOW;
    app.v2.filters.offset = 0;
    app
}

/// A facet answer: each group's question and its `(key, count)`s.
pub(crate) fn reply(groups: Vec<(FacetBy, Vec<(&str, u64)>)>, total: u64) -> FacetResponse {
    FacetResponse {
        groups: groups
            .into_iter()
            .map(|(by, facets)| FacetGroup {
                by,
                facets: facets
                    .into_iter()
                    .map(|(key, count)| Facet {
                        key: key.into(),
                        count,
                    })
                    .collect(),
            })
            .collect(),
        total,
        ..FacetResponse::default()
    }
}

pub(crate) fn press(app: &mut App, code: KeyCode, mods: KeyModifiers) -> Want {
    crate::keys::press(app, KeyEvent::new(code, mods))
}

pub(crate) fn key(app: &mut App, code: KeyCode) -> Want {
    press(app, code, KeyModifiers::NONE)
}

/// A press and a release at one cell of a `w`×`h` screen.
pub(crate) fn click(
    app: &mut App,
    col: u16,
    row: u16,
    size: (u16, u16),
    mods: KeyModifiers,
) -> Want {
    let down = MouseEvent {
        kind: MouseEventKind::Down(MouseButton::Left),
        column: col,
        row,
        modifiers: mods,
    };
    crate::keys::mouse(app, down, size);
    let up = MouseEvent {
        kind: MouseEventKind::Up(MouseButton::Left),
        ..down
    };
    crate::keys::mouse(app, up, size)
}

/// Where character `ch` of panel line `at` is drawn: the panel's own box,
/// then the border and the cursor's three columns.
pub(crate) fn cell_of(app: &App, size: (u16, u16), at: usize, ch: usize) -> (u16, u16) {
    let area = ratatui::layout::Rect::new(0, 0, size.0, size.1);
    let wrap = crate::draw::panel_rect(app, area, 0)
        .width
        .saturating_sub(6) as usize;
    let (_, lines) = app.lens_panel_lines(wrap);
    let rules = lines.iter().filter(|l| l.rule).count();
    let b = crate::draw::panel_rect(app, area, lines.len() + rules);
    let above = lines[..=at].iter().filter(|l| l.rule).count() as u16;
    (b.x + 4 + ch as u16, b.y + 2 + at as u16 + above)
}

/// The whole screen drawn, row by row.
pub(crate) fn screen(app: &App, w: u16, h: u16) -> Vec<String> {
    let theme = crate::theme::Theme::read(true);
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).unwrap();
    term.draw(|f| crate::draw::frame(f, app, &theme)).unwrap();
    let buf = term.backend().buffer().clone();
    (0..h)
        .map(|y| (0..w).map(|x| buf[(x, y)].symbol().to_string()).collect())
        .collect()
}

#[test]
fn each_opening_is_tagged_and_an_older_ones_answer_is_dropped() {
    let mut app = opened("rapor dm:");
    let first = app.take_lens_asks();
    assert_eq!(first.len(), 1, "one walk for the presets and the bars");
    assert_eq!(first[0].opening, 1);

    // Shut and opened again: a new opening, and the first answer is old.
    app.panel = Panel::None;
    app.open_context_lens();
    let second = app.take_lens_asks();
    assert_eq!(second[0].opening, 2);
    let presets = |n| {
        reply(
            vec![(
                FacetBy::Age {
                    edges: lens::PRESET_EDGES.to_vec(),
                },
                vec![("0", n)],
            )],
            n,
        )
    };
    app.lens_faceted(first[0].tag, &presets(5));
    assert_eq!(app.v2.filters.presets, None, "the first opening's answer");
    app.lens_faceted(second[0].tag, &presets(7));
    assert_eq!(app.v2.filters.presets.map(|p| p[0]), Some(7));
    // Answered once: the same tag again is nobody's.
    app.lens_faceted(second[0].tag, &presets(9));
    assert_eq!(app.v2.filters.presets.map(|p| p[0]), Some(7));
}

/// The lane drops what a later opening, or a later question for the same
/// numbers, has replaced; nothing else.
#[test]
fn a_queued_question_goes_when_a_later_one_replaces_it() {
    let ask = |opening, tag, slot| LensAsk {
        opening,
        tag,
        slot,
        asked: Asked::Count {
            query: String::new(),
        },
    };
    let old = ask(1, 1, Slot::Exts);
    assert!(
        old.stale_beside(&ask(2, 2, Slot::Size(0))),
        "a later opening"
    );
    assert!(old.stale_beside(&ask(1, 3, Slot::Exts)));
    assert!(
        old.stale_beside(&ask(1, 3, Slot::Type)),
        "both lists in one"
    );
    assert!(!old.stale_beside(&ask(1, 3, Slot::Kinds)), "another list");
    assert!(!old.stale_beside(&old));
    assert!(!ask(1, 4, Slot::Size(0)).stale_beside(&ask(1, 5, Slot::Size(1))));
}

/// Typing under an open panel asks it nothing: its numbers are asked when
/// it opens, and again only when a choice in it changes what they count.
#[test]
fn typing_under_a_panel_asks_it_nothing() {
    let mut app = opened("rapor dm:");
    app.take_lens_asks();
    for c in "abc".chars() {
        key(&mut app, KeyCode::Char(c));
    }
    assert!(app.take_lens_asks().is_empty());
    assert_eq!(app.panel, Panel::Lens);
}
