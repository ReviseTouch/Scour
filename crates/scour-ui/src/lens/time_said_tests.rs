use super::*;

const OFFSET: i64 = 3 * 3_600;

fn day(y: i64, m: i64, d: i64) -> i64 {
    days_from_civil(y, m, d)
}

/// 2026-11-25 12:00 UTC, 15:00 here.
fn now() -> i64 {
    day(2026, 11, 25) * DAY + 12 * 3_600
}

fn said(v: &str) -> Option<TimeSaid> {
    time_said(v, now(), OFFSET)
}

fn on(y: i64, m: i64, d: i64, form: DateForm) -> Date {
    Date {
        day: day(y, m, d),
        form,
    }
}

#[test]
fn a_window_is_its_preset_or_a_count_of_its_unit() {
    assert_eq!(said("7d"), Some(TimeSaid::Label("last 7 days")));
    assert_eq!(said("24h"), Some(TimeSaid::Label("today")));
    assert_eq!(said("hafta"), Some(TimeSaid::Label("last 7 days")));
    assert_eq!(said(">1y"), Some(TimeSaid::Label("last year")));
    assert_eq!(said("30d.."), Some(TimeSaid::Label("last 30 days")));
    assert_eq!(said("3w"), Some(TimeSaid::Count("last {n} weeks", 3)));
    assert_eq!(said("2YIL"), Some(TimeSaid::Count("last {n} years", 2)));
    assert_eq!(said("12h"), Some(TimeSaid::Count("last {n} hours", 12)));
    assert_eq!(said("1h"), Some(TimeSaid::Count("last hour", 1)));
}

#[test]
fn a_relative_band_is_said_as_the_days_it_covers_now() {
    assert_eq!(
        said("30d..7d"),
        Some(TimeSaid::Range(
            on(2026, 10, 26, DateForm::DayMonth),
            on(2026, 11, 18, DateForm::DayMonth)
        ))
    );
    assert_eq!(
        said("13d..0d"),
        Some(TimeSaid::Range(
            on(2026, 11, 12, DateForm::Day),
            on(2026, 11, 25, DateForm::DayMonth)
        )),
        "12–25 Nov"
    );
    assert_eq!(
        said("<30d"),
        Some(TimeSaid::Bound(
            SAID_BEFORE,
            on(2026, 10, 26, DateForm::DayMonth)
        ))
    );
    assert_eq!(said("7d..30d"), None, "newer first holds nothing");
}

#[test]
fn dates_say_as_little_as_is_true() {
    assert_eq!(
        said("2026-11-12..2026-11-25"),
        Some(TimeSaid::Range(
            on(2026, 11, 12, DateForm::Day),
            on(2026, 11, 25, DateForm::DayMonth)
        ))
    );
    assert_eq!(
        said("2025-12-26..2026-01-03"),
        Some(TimeSaid::Range(
            on(2025, 12, 26, DateForm::DayMonthYear),
            on(2026, 1, 3, DateForm::DayMonth)
        ))
    );
    assert_eq!(
        said("2025-02-03..2025-03-12"),
        Some(TimeSaid::Range(
            on(2025, 2, 3, DateForm::DayMonth),
            on(2025, 3, 12, DateForm::DayMonthYear)
        ))
    );
    let alone = |y, m, d, f| Some(TimeSaid::Date(on(y, m, d, f)));
    assert_eq!(said("2026-11-03"), alone(2026, 11, 3, DateForm::DayMonth));
    assert_eq!(
        said("=2024-11-03"),
        alone(2024, 11, 3, DateForm::DayMonthYear)
    );
    assert_eq!(
        said("2026-03-01..2026-03-31"),
        alone(2026, 3, 1, DateForm::MonthYear)
    );
    assert_eq!(
        said("2028-02-01..2028-02-29"),
        alone(2028, 2, 1, DateForm::MonthYear)
    );
    assert_eq!(
        said("2025-01-01..2025-12-31"),
        alone(2025, 1, 1, DateForm::Year)
    );
    assert_eq!(
        said("2026-11-03..2026-11-03"),
        alone(2026, 11, 3, DateForm::DayMonth)
    );
}

#[test]
fn a_bound_is_said_with_its_year_or_month_when_it_falls_on_one() {
    let b = |m, y, mo, d, f| Some(TimeSaid::Bound(m, on(y, mo, d, f)));
    assert_eq!(
        said("<2025-01-01"),
        b(SAID_BEFORE, 2025, 1, 1, DateForm::Year)
    );
    assert_eq!(
        said(">=2026-03-01"),
        b(SAID_SINCE, 2026, 3, 1, DateForm::MonthYear)
    );
    assert_eq!(
        said("2026-03-01.."),
        b(SAID_SINCE, 2026, 3, 1, DateForm::MonthYear)
    );
    assert_eq!(
        said(">2024-12-31"),
        b(SAID_AFTER, 2024, 12, 31, DateForm::Year)
    );
    assert_eq!(
        said("..2026-04-30"),
        b(SAID_UNTIL, 2026, 4, 30, DateForm::MonthYear)
    );
    assert_eq!(
        said("<=2026-11-03"),
        b(SAID_UNTIL, 2026, 11, 3, DateForm::DayMonth)
    );
    assert_eq!(
        said(">2025-01-01"),
        b(SAID_AFTER, 2025, 1, 1, DateForm::DayMonthYear)
    );
}

#[test]
fn what_is_not_a_time_says_nothing() {
    for v in [
        "",
        "rapor",
        "2025",
        "2025-03",
        "..",
        "=7d",
        "2026-13-01",
        "7x",
    ] {
        assert_eq!(said(v), None, "{v}");
    }
    assert_eq!(on(2026, 11, 25, DateForm::Day).ymd(), (2026, 11, 25));
}
