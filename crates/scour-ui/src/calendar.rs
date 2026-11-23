//! A month as a calendar draws it, for picking a range of days: six weeks of
//! seven, Monday first, each day counted from the epoch so two of them compare
//! and a range is two numbers. And the term a range is written as.

/// The month names and the days' initials, as msgids: a face translates them.
pub const MONTHS: [&str; 12] = [
    "January",
    "February",
    "March",
    "April",
    "May",
    "June",
    "July",
    "August",
    "September",
    "October",
    "November",
    "December",
];
pub const WEEKDAYS: [&str; 7] = ["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"];

/// One square of the grid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Day {
    /// Days since 1970-01-01.
    pub n: i64,
    /// Its day of the month.
    pub day: u32,
    /// In the month shown, rather than the end of the one before or the start
    /// of the one after.
    pub here: bool,
}

/// The six weeks that show `year`-`month` (1 to 12), Monday first.
pub fn month(year: i64, month: u32) -> Vec<Day> {
    let first = days_from_civil(year, i64::from(month), 1);
    // 1970-01-01 was a Thursday: Monday is 0.
    let weekday = (first + 3).rem_euclid(7);
    let start = first - weekday;
    (0..42)
        .map(|i| {
            let n = start + i;
            let (_, m, d) = civil(n);
            Day {
                n,
                day: d as u32,
                here: m == i64::from(month),
            }
        })
        .collect()
}

/// The month `delta` months from `year`-`month`.
pub fn shift(year: i64, month: u32, delta: i64) -> (i64, u32) {
    let at = year * 12 + i64::from(month) - 1 + delta;
    (at.div_euclid(12), (at.rem_euclid(12) + 1) as u32)
}

/// Today here, as days since the epoch, and its year and month.
pub fn today(now: i64) -> (i64, i64, u32) {
    let n = (now + crate::format::local_offset(now)).div_euclid(86_400);
    let (y, m, _) = civil(n);
    (n, y, m as u32)
}

/// A day as the query language writes one: `2026-11-23`.
pub fn ymd(n: i64) -> String {
    let (y, m, d) = civil(n);
    format!("{y:04}-{m:02}-{d:02}")
}

/// The term for a range of days, either way round: one day is that day,
/// two are everything from the first to the last, both included.
pub fn range_term(field: &str, not: bool, a: i64, b: i64) -> String {
    let (from, to) = (a.min(b), a.max(b));
    let neg = if not { "!" } else { "" };
    if from == to {
        format!("{neg}{field}:{}", ymd(from))
    } else {
        format!("{neg}{field}:{}..{}", ymd(from), ymd(to))
    }
}

/// Howard Hinnant's `civil_from_days`.
fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// And `days_from_civil`, its inverse.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_month_starts_on_its_monday_and_holds_every_day_once() {
        // November 2026 begins on a Sunday: six days of October come first.
        let g = month(2026, 11);
        assert_eq!(g.len(), 42);
        assert_eq!(ymd(g[0].n), "2026-10-26");
        assert!(!g[5].here && g[6].here && g[6].day == 1);
        assert_eq!(g.iter().filter(|d| d.here).count(), 30);
        assert!(g.windows(2).all(|w| w[1].n == w[0].n + 1));
        // February in a leap year, and a month that begins on a Monday.
        assert_eq!(month(2028, 2).iter().filter(|d| d.here).count(), 29);
        assert!(month(2026, 6)[0].here, "June 2026 begins on a Monday");
    }

    #[test]
    fn months_shift_across_years() {
        assert_eq!(shift(2026, 1, -1), (2025, 12));
        assert_eq!(shift(2026, 12, 1), (2027, 1));
        assert_eq!(shift(2026, 11, -23), (2024, 12));
    }

    #[test]
    fn a_range_is_written_from_its_first_day_whichever_was_picked_first() {
        let a = days_from_civil(2026, 11, 3);
        let b = days_from_civil(2026, 11, 17);
        assert_eq!(range_term("dm", false, b, a), "dm:2026-11-03..2026-11-17");
        assert_eq!(range_term("dm", true, a, a), "!dm:2026-11-03");
        assert_eq!(civil(a), (2026, 11, 3));
    }
}
