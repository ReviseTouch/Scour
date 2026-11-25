//! What a choice in the time panel writes, and which choice a `dm:` value
//! is. A run of bars is written relative, older bound first, in the scale's
//! unit (`dm:30d..7d`, `dm:4w..1w`), not as dates: it finds exactly the rows
//! its bars counted and reads back to the same bars on any day, where UTC
//! dates never line up with bars counted back from now. It slides with the
//! clock; the capsule says the dates it covers when drawn.

use super::{Mode, SCALES, Scale, TIME_PRESETS, Term, neg};
use crate::format::DAY;

/// A run of bars on a scale, by the drawn indices of
/// [`Histogram::bars`](super::Histogram) (0 the oldest), and what it writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selected {
    pub scale: Scale,
    pub mode: Mode,
    pub first: usize,
    pub last: usize,
}

/// What the panel shows chosen for a `dm:` value: a preset, a run of bars,
/// or both (`dm:7d` is a preset and the last seven day bars).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimePick {
    /// An index into [`TIME_PRESETS`].
    pub preset: Option<usize>,
    pub selected: Option<Selected>,
}

/// The term preset `i` writes in place of `t`, its `!` kept.
pub fn time_preset_term(t: &Term, i: usize) -> String {
    format!("{}dm:{}", neg(t), TIME_PRESETS[i].value)
}

/// The term a run of bars writes in place of `t`, its `!` kept. `Between`
/// reaching now is a window (`dm:7d`); `Before` keeps the run's newer end
/// (`dm:<7d`), `After` its older end (`dm:>30d`). `Before` reaching now holds
/// everything and writes nothing: the term goes.
pub fn time_term(t: &Term, s: Selected) -> String {
    let n = s.scale.bars();
    let first = s.first.min(s.last).min(n - 1);
    let last = s.first.max(s.last).min(n - 1);
    let (newer, older, u) = (n - 1 - last, n - first, s.scale.unit());
    let v = match s.mode {
        Mode::Between if newer == 0 => format!("{older}{u}"),
        Mode::Between => format!("{older}{u}..{newer}{u}"),
        Mode::Before if newer == 0 => return String::new(),
        Mode::Before => format!("<{newer}{u}"),
        Mode::After => format!(">{older}{u}"),
    };
    format!("{}dm:{v}", neg(t))
}

/// The run a switch to `mode` leaves on the same scale, and its term in
/// place of `t`. Before and After turn about the run's older edge: Before
/// is what is older than the run, After the run and all that is newer;
/// Between keeps the bars shown. `None` when nothing would be left on the
/// axis. Touching now, Between writes a window (`dm:7d`), After the same
/// rows as `dm:>7d`, and a Before covering every bar writes nothing.
pub fn time_remode(t: &Term, s: Selected, mode: Mode) -> Option<(Selected, String)> {
    let n = s.scale.bars();
    let (first, last) = match (s.mode, mode) {
        (a, b) if a == b => (s.first, s.last),
        (_, Mode::Between) => (s.first, s.last),
        (Mode::Before, Mode::After) => (s.last + 1, n - 1),
        (_, Mode::After) => (s.first, n - 1),
        (_, Mode::Before) => (0, s.first.checked_sub(1)?),
    };
    if first > last || last >= n {
        return None;
    }
    let next = Selected {
        mode,
        first,
        last,
        ..s
    };
    Some((next, time_term(t, next)))
}

/// Which preset and bars a `dm:` value is; `None` for one the panel cannot
/// show: a date, `=`, a span that is no whole number of bars, or one past the
/// scale. `Before` reads back to the run from the oldest bar, `After` to the
/// run up to now: they write one end only. The bars are on the scale the
/// value names, else the finest that fits; see [`time_bars_on`].
pub fn time_pick(value: &str) -> Option<TimePick> {
    let (preset, selected) = pick(value, None)?;
    (preset.is_some() || selected.is_some()).then_some(TimePick { preset, selected })
}

/// The bars a `dm:` value covers on `scale`, whole bars only, so switching
/// scales keeps showing the choice where it lines up: `dm:7d` is one bar of
/// the week scale, `dm:3d` none.
pub fn time_bars_on(value: &str, scale: Scale) -> Option<Selected> {
    pick(value, Some(scale))?.1
}

type Found = (Option<usize>, Option<Selected>);

fn pick(value: &str, only: Option<Scale>) -> Option<Found> {
    let v = value.trim().to_lowercase();
    let (cmp, rest) = split_cmp(&v);
    Some(match (cmp, rest.split_once("..")) {
        ("", Some(("", hi))) => (None, before(rel(hi)?, only)),
        ("", Some((lo, ""))) => window(rel(lo)?, Mode::Between, only),
        ("", Some((lo, hi))) => (None, band(rel(lo)?, rel(hi)?, only)),
        ("", None) => window(rel(rest)?, Mode::Between, only),
        (">" | ">=", None) => window(rel(rest)?, Mode::After, only),
        ("<" | "<=", None) => (None, before(rel(rest)?, only)),
        _ => return None,
    })
}

fn window(r: Rel, mode: Mode, only: Option<Scale>) -> Found {
    let preset = (mode == Mode::Between)
        .then(|| {
            TIME_PRESETS
                .iter()
                .position(|p| rel(p.value).is_some_and(|q| q.secs() == r.secs()))
        })
        .flatten();
    let selected = fit(&[r], only, |n, b| (1..=b).contains(&n[0])).map(|(scale, n)| {
        let b = scale.bars();
        Selected {
            scale,
            mode,
            first: b - n[0] as usize,
            last: b - 1,
        }
    });
    (preset, selected)
}

fn band(older: Rel, newer: Rel, only: Option<Scale>) -> Option<Selected> {
    let (scale, n) = fit(&[older, newer], only, |n, b| n[0] > n[1] && n[0] <= b)?;
    let b = scale.bars();
    Some(Selected {
        scale,
        mode: Mode::Between,
        first: b - n[0] as usize,
        last: b - 1 - n[1] as usize,
    })
}

fn before(r: Rel, only: Option<Scale>) -> Option<Selected> {
    let (scale, n) = fit(&[r], only, |n, b| n[0] >= 1 && n[0] < b)?;
    Some(Selected {
        scale,
        mode: Mode::Before,
        first: 0,
        last: scale.bars() - 1 - n[0] as usize,
    })
}

/// The scale the amounts are whole bars of and fit: `only`, when given;
/// else the unit written first when it is a week, month or year, then the
/// finest that does.
fn fit(
    rels: &[Rel],
    only: Option<Scale>,
    fits: impl Fn(&[u64], u64) -> bool,
) -> Option<(Scale, Vec<u64>)> {
    let written = match rels[0].unit {
        Unit::Week => Some(Scale::Week),
        Unit::Month => Some(Scale::Month),
        Unit::Year => Some(Scale::Year),
        _ => None,
    }
    .filter(|_| rels.iter().all(|r| r.unit == rels[0].unit));
    let all: Vec<Scale> = match only {
        Some(s) => vec![s],
        None => written.into_iter().chain(SCALES).collect(),
    };
    all.into_iter().find_map(|scale| {
        let bar = i64::from(scale.days()) * DAY;
        let n: Option<Vec<u64>> = rels
            .iter()
            .map(|r| (r.secs() % bar == 0).then(|| (r.secs() / bar) as u64))
            .collect();
        n.filter(|n| fits(n, scale.bars() as u64))
            .map(|n| (scale, n))
    })
}

/// A leading comparison and the rest.
pub(super) fn split_cmp(v: &str) -> (&str, &str) {
    for p in [">=", "<=", ">", "<", "="] {
        if let Some(rest) = v.strip_prefix(p) {
            return (p, rest);
        }
    }
    ("", v)
}

/// A unit of a relative amount, as the language spells them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Unit {
    Hour,
    Day,
    Week,
    Month,
    Year,
}

/// A relative amount: `7d`, `3hafta`, `today`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Rel {
    pub n: u64,
    pub unit: Unit,
}

impl Rel {
    pub fn secs(self) -> i64 {
        let unit = match self.unit {
            Unit::Hour => 3_600,
            Unit::Day => DAY,
            Unit::Week => 7 * DAY,
            Unit::Month => 30 * DAY,
            Unit::Year => 365 * DAY,
        };
        self.n as i64 * unit
    }
}

/// A relative amount as `scour_query::time` reads one, lower case.
pub(super) fn rel(s: &str) -> Option<Rel> {
    let named = |n, unit| Some(Rel { n, unit });
    match s {
        "today" | "bugun" | "bugün" => return named(1, Unit::Day),
        "yesterday" | "dun" | "dün" => return named(2, Unit::Day),
        "week" | "thisweek" | "hafta" => return named(1, Unit::Week),
        "month" | "thismonth" | "ay" => return named(1, Unit::Month),
        "year" | "thisyear" | "yil" | "yıl" => return named(1, Unit::Year),
        _ => {}
    }
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
    let n = num.parse().ok()?;
    let unit = match unit {
        "h" | "s" | "saat" => Unit::Hour,
        "d" | "g" | "gun" | "gün" => Unit::Day,
        "w" | "hafta" => Unit::Week,
        "m" | "ay" => Unit::Month,
        "y" | "yil" | "yıl" => Unit::Year,
        _ => return None,
    };
    Some(Rel { n, unit })
}

#[cfg(test)]
#[path = "time_pick_tests.rs"]
mod tests;
