use super::*;
use crate::app::Panel;
use crate::v2::filters::tests::{NOW, cell_of, click, key, opened, press, reply, screen};
use ratatui::crossterm::event::{KeyCode, KeyModifiers};

/// The presets' walk and the day bars' walk, as the service answers them:
/// ten rows today, four six days back, two at the far end, a hundred older.
fn day_answer() -> FacetResponse {
    let ages = vec![("0", 10), ("6", 4), ("29", 2), ("older", 100)];
    reply(
        vec![
            (
                FacetBy::Age {
                    edges: PRESET_EDGES.to_vec(),
                },
                ages.clone(),
            ),
            (
                FacetBy::Age {
                    edges: lens::Scale::Day.edges(),
                },
                ages,
            ),
        ],
        116,
    )
}

/// The lens opened on `query`, answered for the day scale.
fn answered(query: &str) -> App {
    let mut app = opened(query);
    let ask = app.take_lens_asks().remove(0);
    app.lens_faceted(ask.tag, &day_answer());
    app
}

fn texts(app: &App) -> Vec<String> {
    app.lens_panel_lines(60)
        .1
        .into_iter()
        .map(|l| format!("{}|{}", l.text, l.key))
        .collect()
}

fn down(app: &mut App, n: usize) {
    for _ in 0..n {
        key(app, KeyCode::Down);
    }
}

fn shift(app: &mut App, code: KeyCode, n: usize) {
    for _ in 0..n {
        press(app, code, KeyModifiers::SHIFT);
    }
}

#[test]
fn the_presets_are_counted_and_the_bars_drawn_between_their_ends() {
    let mut app = opened("rapor dm:");
    let asks = app.take_lens_asks();
    let Asked::Facets { query, by } = &asks[0].asked else {
        panic!("a facet walk");
    };
    assert_eq!(query, "rapor", "counted over the query less its dm:");
    assert_eq!(by.len(), 2, "the presets and the scale, one walk");
    assert_eq!(texts(&app)[0], "  today|", "nothing before the answer");
    assert_eq!(
        texts(&app)[BARS_LINE],
        format!("26 Oct {} 25 Nov|← → ⇧", "·".repeat(30))
    );

    app.lens_faceted(asks[0].tag, &day_answer());
    let t = texts(&app);
    assert_eq!(
        t[..5],
        [
            "  today|10",
            "  yesterday|10",
            "  last 7 days|14",
            "  last 30 days|16",
            "  last year|16"
        ]
    );
    assert_eq!(t[SCALE_LINE], "Day  Week  Month  Year|← →");
    assert_eq!(t[MODE_LINE], "Between  Before  After|← →");
    let bars = format!("▂{}▄{}█", "·".repeat(22), "·".repeat(5));
    assert_eq!(t[BARS_LINE], format!("26 Oct {bars} 25 Nov|← → ⇧"));
    assert_eq!(t[SAID_LINE], "·|");
    let (_, lines) = app.lens_panel_lines(60);
    assert_eq!(lines[SCALE_LINE].lit, Some((0, 3)), "Day is chosen");
    assert_eq!(lines[BARS_LINE].lit, None, "no bars yet");
}

#[test]
fn the_arrows_choose_a_run_of_bars_and_enter_writes_it() {
    let mut app = answered("rapor dm:");
    down(&mut app, BARS_LINE);
    key(&mut app, KeyCode::Left);
    assert_eq!(
        app.v2.filters.sel,
        Some((29, 29)),
        "the first press: today's bar"
    );
    assert_eq!(texts(&app)[SAID_LINE], "today|10");
    shift(&mut app, KeyCode::Left, 6);
    assert_eq!(app.v2.filters.sel, Some((23, 29)));
    assert_eq!(texts(&app)[SAID_LINE], "last 7 days|14");
    let (_, lines) = app.lens_panel_lines(60);
    assert_eq!(lines[BARS_LINE].lit, Some((7 + 23, 7 + 30)));
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.query, "rapor dm:7d");
    assert_eq!(app.panel, Panel::None);
}

#[test]
fn a_band_reads_as_its_dates_and_the_modes_write_one_end() {
    let mut app = answered("rapor dm:");
    down(&mut app, BARS_LINE);
    key(&mut app, KeyCode::Left);
    for _ in 0..7 {
        key(&mut app, KeyCode::Left);
    }
    shift(&mut app, KeyCode::Left, 22);
    assert_eq!(app.v2.filters.sel, Some((0, 22)));
    assert_eq!(texts(&app)[SAID_LINE], "26 Oct – 18 Nov|2");

    key(&mut app, KeyCode::Up);
    key(&mut app, KeyCode::Right);
    assert_eq!(app.v2.filters.mode, Mode::Before);
    assert_eq!(
        texts(&app)[SAID_LINE],
        "before 18 Nov|102",
        "and every older row"
    );
    key(&mut app, KeyCode::Right);
    assert_eq!(texts(&app)[SAID_LINE], "last 30 days|16");
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.query, "rapor dm:>30d");

    let mut app = answered("rapor dm:30d..7d");
    assert_eq!(app.v2.filters.sel, Some((0, 22)), "reopened on the band");
    app.panel_at = MODE_LINE;
    key(&mut app, KeyCode::Right);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.query, "rapor dm:<7d");
}

#[test]
fn reopened_on_a_term_the_panel_shows_it() {
    let app = answered("rapor dm:7d");
    assert_eq!(texts(&app)[2], "✓ last 7 days|14");
    assert_eq!(app.v2.filters.sel, Some((23, 29)));
    assert_eq!(texts(&app)[SAID_LINE], "last 7 days|14");

    let app = opened("dm:3m..1m");
    assert_eq!(app.v2.filters.scale, lens::Scale::Month);
    assert_eq!(app.v2.filters.sel, Some((21, 22)));
    assert_eq!(texts(&app)[SAID_LINE], "27 Aug – 26 Oct|");
}

#[test]
fn another_scale_is_asked_once_and_the_old_answer_dropped() {
    let mut app = opened("dm:");
    let day = app.take_lens_asks().remove(0);
    down(&mut app, SCALE_LINE);
    key(&mut app, KeyCode::Right);
    key(&mut app, KeyCode::Right);
    assert_eq!(app.v2.filters.scale, lens::Scale::Month);
    let asks = app.take_lens_asks();
    assert_eq!(asks.len(), 2, "one walk per scale passed through");
    let Asked::Facets { by, .. } = &asks[1].asked else {
        panic!("a facet walk");
    };
    assert_eq!(
        by[1],
        FacetBy::Age {
            edges: lens::Scale::Month.edges()
        }
    );

    app.lens_faceted(day.tag, &day_answer());
    app.lens_faceted(asks[0].tag, &day_answer());
    assert!(
        app.v2.filters.hist.is_none(),
        "the day and week answers are old"
    );
    let month = reply(
        vec![
            (
                FacetBy::Age {
                    edges: PRESET_EDGES.to_vec(),
                },
                vec![],
            ),
            (
                FacetBy::Age {
                    edges: lens::Scale::Month.edges(),
                },
                vec![("29", 3)],
            ),
        ],
        3,
    );
    app.lens_faceted(asks[1].tag, &month);
    assert_eq!(app.v2.filters.hist.as_ref().map(|h| h.bars.len()), Some(24));
    assert!(
        texts(&app)[BARS_LINE].starts_with("Dec 2024 "),
        "{}",
        texts(&app)[BARS_LINE]
    );

    down(&mut app, 2);
    key(&mut app, KeyCode::Left);
    key(&mut app, KeyCode::Left);
    shift(&mut app, KeyCode::Left, 1);
    key(&mut app, KeyCode::Enter);
    assert_eq!(app.query, "dm:3m..1m");
}

/// A click lands on the bar drawn under it, at 100 and at 80 columns; the
/// second click ends the run, a click on a scale's word chooses it.
#[test]
fn a_click_chooses_the_bars_and_the_scale_drawn_under_it() {
    for size in [(100, 30), (80, 30)] {
        let mut app = answered("rapor dm:");
        let lead = 7;
        let (c, r) = cell_of(&app, size, BARS_LINE, lead + 23);
        click(&mut app, c, r, size, KeyModifiers::NONE);
        assert_eq!(app.v2.filters.sel, Some((23, 23)), "{size:?}");
        let (c, r) = cell_of(&app, size, BARS_LINE, lead + 29);
        click(&mut app, c, r, size, KeyModifiers::NONE);
        assert_eq!(app.v2.filters.sel, Some((23, 29)), "{size:?}");
        let shot = screen(&app, size.0, size.1);
        assert!(
            shot.iter()
                .any(|l| l.contains("last 7 days") && l.contains("14"))
        );
        assert!(
            shot.iter()
                .any(|l| l.contains("26 Oct ▂") && l.contains("25 Nov"))
        );

        let (c, r) = cell_of(&app, size, SCALE_LINE, "Day  Week  ".len());
        click(&mut app, c, r, size, KeyModifiers::NONE);
        assert_eq!(app.v2.filters.scale, lens::Scale::Month, "{size:?}");
        let (c, r) = cell_of(&app, size, 2, 2);
        click(&mut app, c, r, size, KeyModifiers::NONE);
        assert_eq!(app.query, "rapor dm:7d", "a preset is a press");
    }
}

#[test]
fn a_date_is_said_with_its_month_shortened_as_the_language_does() {
    let mut app = App::default();
    let day = Date {
        day: NOW / DAY,
        form: DateForm::DayMonthYear,
    };
    assert_eq!(app.date_text(day), "25 Nov 2026");
    let form = |form| Date { form, ..day };
    assert_eq!(app.date_text(form(DateForm::DayMonth)), "25 Nov");
    assert_eq!(app.date_text(form(DateForm::MonthYear)), "Nov 2026");
    assert_eq!(app.date_text(form(DateForm::Year)), "2026");
    assert_eq!(app.date_text(form(DateForm::Day)), "25");
    // A year that is not this one is said, on both ends.
    assert_eq!(
        app.time_words("2025-10-21..2026-11-25", NOW, 0).as_deref(),
        Some("21 Oct 2025 – 25 Nov")
    );
    assert_eq!(
        app.time_words("2026-11-12..2026-11-25", NOW, 0).as_deref(),
        Some("12 – 25 Nov")
    );
    app.words = scour_i18n::Catalogue::for_language("tr");
    assert_eq!(app.date_text(form(DateForm::DayMonth)), "25 Kas");
    assert_eq!(
        app.time_words("30d..7d", NOW, 0).as_deref(),
        Some("26 Eki – 18 Kas")
    );
}
