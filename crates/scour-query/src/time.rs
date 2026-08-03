//! Dates, without a calendar dependency.
//!
//! Two forms are accepted, and they mean different things on purpose:
//!
//! * **A relative window** — `24h`, `7d`, `today`, `hafta` — means "within the
//!   last N hours or days", counted backwards from now. Not a calendar
//!   boundary: that would need the local time zone, and the answer to "changed
//!   in the last week" does not become more useful for knowing where the user
//!   is sitting.
//! * **An ISO day** — `2026-01-31` — is a calendar day in UTC. Written without
//!   an operator it means *on* that day, which is what a person typing a date
//!   into a search box is asking for.

use scour_core::{Cmp, Match, TimeField};

use crate::parse::split_cmp;

const HOUR: i64 = 3_600;
const DAY: i64 = 86_400;

/// Current unix time in seconds.
pub fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Can a time field read this value, and to what instant?
///
/// The same judgement [`parse_time`] makes, without a field to attach it to —
/// the highlighter has to decide whether `dm:soon` is a date before it knows
/// whether the term survives, and asking two different questions there is how
/// a search box ends up colouring a term green that the parser then reads as
/// text.
pub(crate) fn parse_time_value(v: &str, now: i64) -> Option<i64> {
    match parse_time(TimeField::Modified, v, now) {
        Some(Match::Time(_, _, at)) => Some(at),
        _ => None,
    }
}

pub(crate) fn parse_time(field: TimeField, v: &str, now: i64) -> Option<Match> {
    let (cmp, rest) = split_cmp(v);
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }
    if let Some(window) = relative_window(rest) {
        // The window resolves to an instant, and the comparison applies to it
        // exactly as it would to a written date. A bare `dm:7d` means "within
        // the last week", so no operator is `Ge`; `dm:<7d` means the file has
        // not been touched since then.
        //
        // The operator used to be computed and then dropped here, which made
        // `dm:<7d` and `dm:>7d` both mean `dm:7d` — a confident answer to the
        // opposite question.
        let cmp = if v.starts_with(['>', '<', '=']) {
            cmp
        } else {
            Cmp::Ge
        };
        return Some(Match::Time(field, cmp, now - window));
    }
    let day = parse_iso_date(rest)?;
    // `split_cmp` defaults to `Ge`, which is right for sizes and wrong for a
    // bare date: "modified 2026-01-31" means that day, not that day onwards.
    let cmp = if v.starts_with(['>', '<', '=']) {
        cmp
    } else {
        Cmp::Eq
    };
    Some(Match::Time(field, cmp, day))
}

/// Seconds behind `24h`, `7d`, `today`, and their Turkish spellings.
fn relative_window(s: &str) -> Option<i64> {
    let named = match s {
        "today" | "bugun" | "bugün" => Some(DAY),
        "yesterday" | "dun" | "dün" => Some(2 * DAY),
        "week" | "thisweek" | "hafta" => Some(7 * DAY),
        "month" | "thismonth" | "ay" => Some(30 * DAY),
        "year" | "thisyear" | "yil" | "yıl" => Some(365 * DAY),
        _ => None,
    };
    if named.is_some() {
        return named;
    }
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
    let n: i64 = num.parse().ok()?;
    let mult = match unit {
        "h" | "s" | "saat" => HOUR,
        "d" | "g" | "gun" | "gün" => DAY,
        "w" | "hafta" => 7 * DAY,
        "m" | "ay" => 30 * DAY,
        "y" | "yil" | "yıl" => 365 * DAY,
        _ => return None,
    };
    n.checked_mul(mult)
}

/// `YYYY-MM-DD` to the epoch seconds of 00:00 UTC on that day.
fn parse_iso_date(s: &str) -> Option<i64> {
    let mut it = s.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let mo: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, mo, d) * DAY)
}

/// Howard Hinnant's civil-date algorithm: (year, month, day) to days since the
/// unix epoch. Correct for the whole proleptic Gregorian calendar, and short
/// enough that pulling in a date library to do it would be the larger cost.
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
    fn relative_windows_count_backwards_from_now() {
        let now = 1_800_000_000;
        assert_eq!(
            parse_time(TimeField::Modified, "7d", now),
            Some(Match::Time(TimeField::Modified, Cmp::Ge, now - 7 * DAY))
        );
        assert_eq!(
            parse_time(TimeField::Modified, "24h", now),
            Some(Match::Time(TimeField::Modified, Cmp::Ge, now - 24 * HOUR))
        );
        assert_eq!(
            parse_time(TimeField::Accessed, "hafta", now),
            Some(Match::Time(TimeField::Accessed, Cmp::Ge, now - 7 * DAY))
        );
    }

    #[test]
    fn a_bare_iso_date_means_that_day() {
        assert_eq!(
            parse_time(TimeField::Created, "2026-01-31", 0),
            Some(Match::Time(TimeField::Created, Cmp::Eq, 1_769_817_600))
        );
        assert_eq!(
            parse_time(TimeField::Accessed, ">2026-01-31", 0),
            Some(Match::Time(TimeField::Accessed, Cmp::Gt, 1_769_817_600))
        );
    }

    #[test]
    fn nonsense_is_rejected_so_it_can_fall_back_to_text() {
        assert_eq!(parse_time(TimeField::Modified, "", 0), None);
        assert_eq!(parse_time(TimeField::Modified, "yarin", 0), None);
        assert_eq!(parse_time(TimeField::Modified, "2026-13-01", 0), None);
        assert_eq!(parse_time(TimeField::Modified, "2026-01-32", 0), None);
        assert_eq!(parse_time(TimeField::Modified, "2026-01-01-01", 0), None);
    }

    #[test]
    fn civil_dates_match_known_epochs() {
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(days_from_civil(2000, 3, 1), 11_017);
        assert_eq!(days_from_civil(1969, 12, 31), -1);
        // A leap day, which is where an off-by-one in this algorithm shows up.
        assert_eq!(
            days_from_civil(2024, 2, 29) + 1,
            days_from_civil(2024, 3, 1)
        );
    }

    #[test]
    fn an_absurd_window_does_not_overflow() {
        assert_eq!(relative_window("9999999999999999999y"), None);
    }
}
