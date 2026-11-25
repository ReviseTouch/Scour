//! A `dm:` value in words, for its capsule: "last 7 days", "12–25 Nov",
//! "before 2025". The words are msgids and the dates are days; how a day is
//! written (month names, order) is the face's, in the person's language. The
//! `!` of a negated term is the capsule's to draw; the words stay the same.

use super::TIME_PRESETS;
use super::time_pick::{Rel, Unit, rel, split_cmp};
use crate::calendar::{civil, days_from_civil};
use crate::format::DAY;

/// How much of a date is said.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DateForm {
    /// The day of the month alone: the first of "12–25 Nov".
    Day,
    DayMonth,
    DayMonthYear,
    MonthYear,
    Year,
}

/// A calendar day, as days since 1970-01-01, and how much of it to say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Date {
    pub day: i64,
    pub form: DateForm,
}

impl Date {
    /// Its year, month (1 to 12) and day of the month.
    pub fn ymd(self) -> (i64, u32, u32) {
        let (y, m, d) = civil(self.day);
        (y, m as u32, d as u32)
    }
}

/// What a capsule says for a `dm:` value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeSaid {
    /// A preset's msgid, said as it is.
    Label(&'static str),
    /// A msgid with `{n}`: "last {n} weeks".
    Count(&'static str, u64),
    /// One day, a whole month or a whole year, with no words.
    Date(Date),
    /// A msgid with `{date}`: "before {date}".
    Bound(&'static str, Date),
    /// From one day to another, both included, the older first: drawn with
    /// an en dash between, each part as its form says ("12–25 Nov").
    Range(Date, Date),
}

pub const SAID_BEFORE: &str = "before {date}";
pub const SAID_AFTER: &str = "after {date}";
pub const SAID_SINCE: &str = "since {date}";
pub const SAID_UNTIL: &str = "until {date}";

/// The msgid of "within the last `n`" in a unit.
fn last(n: u64, unit: Unit) -> &'static str {
    match unit {
        Unit::Hour if n == 1 => "last hour",
        Unit::Hour => "last {n} hours",
        Unit::Day => "last {n} days",
        Unit::Week => "last {n} weeks",
        Unit::Month => "last {n} months",
        Unit::Year => "last {n} years",
    }
}

/// A `dm:` value in words at `now` (unix seconds), `offset` seconds east of
/// UTC (`format::local_offset(now)`): a relative bound is said as the local
/// day it falls on now. `None` for a value the language does not read as a
/// time, or a range that holds nothing (`dm:7d..30d`).
pub fn time_said(value: &str, now: i64, offset: i64) -> Option<TimeSaid> {
    let v = value.trim().to_lowercase();
    let at = Clock { now, offset };
    let (cmp, rest) = split_cmp(&v);
    match (cmp, rest.split_once("..")) {
        ("", Some((lo, ""))) if !lo.is_empty() => at.bound(">=", lo),
        ("", Some(("", hi))) if !hi.is_empty() => at.bound("<=", hi),
        ("", Some((lo, hi))) if !lo.is_empty() => at.range(lo, hi),
        ("", None) => at.plain(rest),
        (c, None) => at.bound(c, rest),
        _ => None,
    }
}

/// One end of a range: an amount back from now, or a calendar day.
enum Point {
    Back(Rel),
    On(i64),
}

fn point(s: &str) -> Option<Point> {
    rel(s).map(Point::Back).or_else(|| iso(s).map(Point::On))
}

/// `YYYY-MM-DD` as days since the epoch, as `scour_query::time` reads one.
fn iso(s: &str) -> Option<i64> {
    let mut it = s.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    let fine = it.next().is_none() && (1..=12).contains(&m) && (1..=31).contains(&d);
    fine.then(|| days_from_civil(y, m, d))
}

struct Clock {
    now: i64,
    offset: i64,
}

impl Clock {
    /// The local day an amount back from now falls on.
    fn back(&self, r: Rel) -> i64 {
        (self.now - r.secs() + self.offset).div_euclid(DAY)
    }

    fn year(&self) -> i64 {
        civil((self.now + self.offset).div_euclid(DAY)).0
    }

    fn day_of(&self, p: &Point) -> i64 {
        match p {
            Point::Back(r) => self.back(*r),
            Point::On(d) => *d,
        }
    }

    /// A day said alone: its year only when it is not this one.
    fn single(&self, day: i64) -> Date {
        let form = if civil(day).0 == self.year() {
            DateForm::DayMonth
        } else {
            DateForm::DayMonthYear
        };
        Date { day, form }
    }

    fn window(&self, r: Rel) -> TimeSaid {
        let preset = TIME_PRESETS
            .iter()
            .find(|p| rel(p.value).is_some_and(|q| q.secs() == r.secs()));
        match preset {
            Some(p) => TimeSaid::Label(p.label),
            None => TimeSaid::Count(last(r.n, r.unit), r.n),
        }
    }

    fn plain(&self, s: &str) -> Option<TimeSaid> {
        Some(match point(s)? {
            Point::Back(r) => self.window(r),
            Point::On(d) => TimeSaid::Date(self.single(d)),
        })
    }

    fn bound(&self, cmp: &str, s: &str) -> Option<TimeSaid> {
        let day = match (cmp, point(s)?) {
            (">" | ">=", Point::Back(r)) => return Some(self.window(r)),
            ("<" | "<=", Point::Back(r)) => return Some(self.at(SAID_BEFORE, self.back(r))),
            ("=", Point::On(d)) => return Some(TimeSaid::Date(self.single(d))),
            (_, Point::On(d)) => d,
            _ => return None,
        };
        Some(match cmp {
            "<" => self.at(SAID_BEFORE, day),
            ">=" => self.at(SAID_SINCE, day),
            ">" => self.at(SAID_AFTER, day),
            _ => self.at(SAID_UNTIL, day),
        })
    }

    /// A bound said with as little of its date as is true: "before 2025"
    /// for 1 January, "after March 2026" for the 31st.
    fn at(&self, msgid: &'static str, day: i64) -> TimeSaid {
        let (_, m, d) = civil(day);
        let starts = msgid == SAID_BEFORE || msgid == SAID_SINCE;
        let edge = if starts {
            d == 1
        } else {
            day == month_end(day)
        };
        let form = match (edge, m == if starts { 1 } else { 12 }) {
            (true, true) => DateForm::Year,
            (true, false) => DateForm::MonthYear,
            _ => self.single(day).form,
        };
        TimeSaid::Bound(msgid, Date { day, form })
    }

    fn range(&self, lo: &str, hi: &str) -> Option<TimeSaid> {
        let (from, to) = (self.day_of(&point(lo)?), self.day_of(&point(hi)?));
        if from > to {
            return None;
        }
        let ((fy, fm, fd), (ty, tm, _)) = (civil(from), civil(to));
        let whole_month = fd == 1 && (fy, fm) == (ty, tm) && to == month_end(to);
        let whole_year = fd == 1 && fm == 1 && fy == ty && (tm, civil(to).2) == (12, 31);
        let alone = |form| Some(TimeSaid::Date(Date { day: from, form }));
        if from == to {
            return Some(TimeSaid::Date(self.single(from)));
        } else if whole_year {
            return alone(DateForm::Year);
        } else if whole_month {
            return alone(DateForm::MonthYear);
        }
        let to = self.single(to);
        let form = match (fy == ty, fm == tm) {
            (true, true) => DateForm::Day,
            (true, false) => DateForm::DayMonth,
            _ => self.single(from).form,
        };
        Some(TimeSaid::Range(Date { day: from, form }, to))
    }
}

/// The last day of the month `day` is in.
fn month_end(day: i64) -> i64 {
    let (y, m, _) = civil(day);
    let (ny, nm) = if m == 12 { (y + 1, 1) } else { (y, m + 1) };
    days_from_civil(ny, nm, 1) - 1
}

#[cfg(test)]
#[path = "time_said_tests.rs"]
mod tests;
