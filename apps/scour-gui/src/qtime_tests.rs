use super::*;

fn t(q: &str) -> Term {
    lens::terms(q).remove(0)
}

#[test]
fn a_term_opens_the_panel_on_its_own_scale_and_mode() {
    assert_eq!(Look::of("4w..1w").scale, Scale::Week);
    assert_eq!(Look::of("<7d").mode, Mode::Before);
    assert_eq!(Look::of("2026-11-03"), Look::default());
    assert_eq!(Look::of(""), Look::default());
}

#[test]
fn a_run_of_bars_writes_relative_and_a_before_reaching_now_writes_nothing() {
    let day = Look::default();
    assert_eq!(bars_term(&t("dm:"), &day, 29, 23), "dm:7d");
    assert_eq!(bars_term(&t("!dm:7d"), &day, 0, 22), "!dm:30d..7d");
    let before = Look {
        mode: Mode::Before,
        ..day
    };
    assert_eq!(bars_term(&t("dm:7d"), &before, 3, 29), "");
    let month = Look {
        scale: Scale::Month,
        ..day
    };
    assert_eq!(bars_term(&t("dm:"), &month, 20, 21), "dm:4m..2m");
}

#[test]
fn the_arrows_move_the_end_on_their_side_and_shift_the_other() {
    let day = Look::default();
    // dm:7d is bars 23..=29 on the day scale.
    assert_eq!(step("7d", &day, -1, false), Some((22, 29)));
    assert_eq!(
        step("7d", &day, 1, false),
        Some((23, 29)),
        "now is the edge"
    );
    assert_eq!(step("7d", &day, -1, true), Some((23, 28)));
    assert_eq!(step("7d", &day, 1, true), Some((24, 29)));
    assert_eq!(
        step("1d", &day, 1, true),
        Some((29, 29)),
        "a run keeps one bar"
    );
    // Before has one end, the newer.
    assert_eq!(step("<7d", &day, -1, false), Some((0, 21)));
    // A term not lined up with the scale shown moves nothing.
    let week = Look {
        scale: Scale::Week,
        ..day
    };
    // dm:7d is one bar of the week scale; dm:3d lines up with none.
    assert_eq!(step("7d", &week, -1, false), Some((24, 25)));
    assert_eq!(step("3d", &week, -1, false), None);
    assert_eq!(step("2026-11-03", &day, -1, false), None);
}

#[test]
fn the_calendar_lights_the_days_a_date_term_covers_and_only_those() {
    let d = |y: i64, m: u32, d: i64| {
        let first = scour_ui::calendar::month(y, m)
            .into_iter()
            .find(|x| x.here)
            .unwrap();
        first.n + d - 1
    };
    assert_eq!(
        days("2026-11-03..2026-11-17"),
        Some((d(2026, 11, 3), d(2026, 11, 17)))
    );
    assert_eq!(days("2026-11-03"), Some((d(2026, 11, 3), d(2026, 11, 3))));
    assert_eq!(
        days("2026-02-01..2026-02-28"),
        Some((d(2026, 2, 1), d(2026, 2, 28)))
    );
    assert_eq!(days("7d"), None, "the bars show it");
    assert_eq!(days("<2025-01-01"), None);
}

#[test]
fn a_date_is_said_with_the_month_s_name_from_the_catalogue() {
    let cat = Catalogue::for_language("en");
    let day = |s: &str| {
        let n = scour_ui::calendar::month(2026, 11)
            .into_iter()
            .find(|x| x.here)
            .unwrap()
            .n;
        Date {
            day: n + 11,
            form: match s {
                "dm" => DateForm::DayMonth,
                "y" => DateForm::Year,
                "my" => DateForm::MonthYear,
                _ => DateForm::DayMonthYear,
            },
        }
    };
    assert_eq!(date(day("dm"), &cat), "12 November");
    assert_eq!(date(day("dmy"), &cat), "12 November 2026");
    assert_eq!(date(day("my"), &cat), "November 2026");
    assert_eq!(date(day("y"), &cat), "2026");
    assert_eq!(words("7d", &cat).as_deref(), Some("last 7 days"));
    assert_eq!(words("3w", &cat).as_deref(), Some("last 3 weeks"));
    assert_eq!(
        words("2025-03-01..2025-03-31", &cat).as_deref(),
        Some("March 2025")
    );
    assert_eq!(words("<2025-01-01", &cat).as_deref(), Some("before 2025"));
    assert_eq!(words("pdf", &cat), None);
}
