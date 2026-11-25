//! The time panel as this window keeps it: the scale and the mode chosen,
//! the one age facet it asks (the presets' bands and the bars' in one walk),
//! what a run of bars or an arrow key writes, and a `dm:` value said in the
//! window's words. What each choice means is `scour_ui::lens`'s.

use std::collections::HashMap;

use scour_core::{Catalog, FacetBy};
use scour_i18n::Catalogue;
use scour_proto::Response;
use scour_ui::calendar::MONTHS;
use scour_ui::format::{DAY, local_offset};
use scour_ui::lens::{
    self, Date, DateForm, Histogram, Mode, PRESET_EDGES, Scale, Selected, TIME_PRESETS, Term,
    TimeSaid,
};

use crate::qlens::{Env, Item, Open};
use crate::qpanel::{self, Want};

/// The facet's slot, and the whole query's count when no bars say it.
pub const AGES: u32 = 0;
pub const WHOLE: u32 = 1;

/// What the panel keeps across the redraws of one opening.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Look {
    pub scale: Scale,
    pub mode: Mode,
    /// A press on the bars made them the arrows' until the lens closes.
    pub armed: bool,
}

impl Default for Look {
    fn default() -> Self {
        Look {
            scale: Scale::Day,
            mode: Mode::Between,
            armed: false,
        }
    }
}

impl Look {
    /// As a term opens it: on the scale and mode its bars are drawn on.
    pub fn of(value: &str) -> Look {
        let s = lens::time_pick(value).and_then(|p| p.selected);
        Look {
            scale: s.map_or(Scale::Day, |s| s.scale),
            mode: s.map_or(Mode::Between, |s| s.mode),
            armed: false,
        }
    }
}

pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// Ask what the panel draws: one facet with two age groups, the presets'
/// and the scale's. A new scale is a new question; nothing else is.
pub fn fill(o: &mut Open, seen: &HashMap<String, Response>, q: &str, look: &Look) -> Vec<Want> {
    let mut out = Vec::new();
    let query = lens::time_ask(q);
    if lens::reads(&query) {
        return out;
    }
    let by = vec![
        FacetBy::Age {
            edges: PRESET_EDGES.to_vec(),
        },
        FacetBy::Age {
            edges: look.scale.edges(),
        },
    ];
    let key = format!("ages {} {query}", look.scale.label());
    o.got.ages = qpanel::ask_facets(seen, AGES, key, &query, by, &mut out);
    if o.got.ages.is_some() && kept(o, look).is_none() && !o.term.value.trim().is_empty() {
        o.got.whole = qpanel::count(seen, WHOLE, q, &mut out);
    }
    out
}

/// The bars of the scale shown, once counted.
pub fn bars(o: &Open, look: &Look) -> Option<Histogram> {
    let f = o.got.ages.as_ref()?;
    Some(lens::histogram(look.scale, qpanel::nth(f, 1), now()))
}

/// The run of bars the term is on the scale shown, if it is one.
pub fn shown(value: &str, look: &Look) -> Option<Selected> {
    lens::time_pick(value)?
        .selected
        .filter(|s| s.scale == look.scale)
}

/// What the term keeps, when the presets or the bars say it.
pub fn kept(o: &Open, look: &Look) -> Option<u64> {
    let f = o.got.ages.as_ref()?;
    let pick = lens::time_pick(&o.term.value)?;
    let n = match (pick.preset, shown(&o.term.value, look)) {
        (Some(i), _) => lens::preset_counts(qpanel::nth(f, 0))[i],
        (None, Some(s)) => {
            let h = bars(o, look)?;
            let run = |a: usize, b: usize| h.bars[a..=b].iter().map(|b| b.count).sum::<u64>();
            let last = h.bars.len() - 1;
            match s.mode {
                Mode::Between => run(s.first, s.last),
                Mode::Before => run(0, s.last) + h.older,
                Mode::After => run(s.first, last),
            }
        }
        _ => return None,
    };
    Some(if o.term.not {
        f.total.saturating_sub(n)
    } else {
        n
    })
}

/// The presets, with their counts once the facet is in: the lines the keys
/// move over.
pub fn items(o: &mut Open, env: &Env) -> Vec<Item> {
    let tr = env.tr;
    o.title = tr("When it last changed");
    o.foot = tr("Or type one: 24h, 2w, 2026-03-01, >2026-06-01");
    let counts = o
        .got
        .ages
        .as_ref()
        .map(|f| lens::preset_counts(qpanel::nth(f, 0)));
    TIME_PRESETS
        .iter()
        .enumerate()
        .map(|(i, p)| Item {
            label: tr(p.label),
            term: lens::time_preset_term(&o.term, i),
            n: counts.map(|c| c[i] as i64),
            ..Default::default()
        })
        .collect()
}

/// What bars `a..=b` write in place of `t`. A Before reaching now holds
/// everything: the filter is left empty, still open.
pub fn bars_term(t: &Term, look: &Look, a: usize, b: usize) -> String {
    let s = Selected {
        scale: look.scale,
        mode: look.mode,
        first: a.min(b),
        last: a.max(b),
    };
    match lens::time_term(t, s) {
        e if e.is_empty() => "dm:".into(),
        e => e,
    }
}

/// An arrow on the bars: ← and → move the end on their side outward,
/// Shift the other end inward. Before and After have one end to move.
pub fn step(value: &str, look: &Look, dir: i32, shift: bool) -> Option<(usize, usize)> {
    let s = shown(value, look)?;
    let top = look.scale.bars() as i32 - 1;
    let at = |v: usize, by: i32| (v as i32 + by).clamp(0, top) as usize;
    let (mut f, mut l) = (s.first, s.last);
    match (s.mode, dir < 0, shift) {
        (Mode::Before, ..) => l = at(l, dir),
        (Mode::After, ..) => f = at(f, dir),
        (_, true, false) => f = at(f, -1),
        (_, false, false) => l = at(l, 1),
        (_, true, true) => l = at(l, -1).max(f),
        (_, false, true) => f = at(f, 1).min(l),
    }
    Some((f, l))
}

/// A day as the window says one: the day and the month's name, in the
/// order of the calendar's titles.
pub fn date(d: Date, cat: &Catalogue) -> String {
    let (y, m, day) = d.ymd();
    let month = cat.get(MONTHS[m as usize - 1]);
    match d.form {
        DateForm::Day => day.to_string(),
        DateForm::DayMonth => format!("{day} {month}"),
        DateForm::DayMonthYear => format!("{day} {month} {y}"),
        DateForm::MonthYear => format!("{month} {y}"),
        DateForm::Year => y.to_string(),
    }
}

/// A `dm:` value in the window's words: "last 7 days", "12–25 November".
pub fn words(value: &str, cat: &Catalogue) -> Option<String> {
    let now = now();
    Some(match lens::time_said(value, now, local_offset(now))? {
        TimeSaid::Label(m) => cat.get(m).into_owned(),
        TimeSaid::Count(m, n) => cat.get(m).replace("{n}", &n.to_string()),
        TimeSaid::Date(d) => date(d, cat),
        TimeSaid::Bound(m, d) => cat.get(m).replace("{date}", &date(d, cat)),
        TimeSaid::Range(a, b) => format!("{}–{}", date(a, cat), date(b, cat)),
    })
}

/// Where the axis starts, said as the scale says its ends.
pub fn axis_from(h: &Histogram, cat: &Catalogue) -> String {
    let day = (h.from + local_offset(h.from)).div_euclid(DAY);
    date(
        Date {
            day,
            form: h.scale.ends(),
        },
        cat,
    )
}

/// The days a date term covers, for the calendar to light: one day, a run
/// of days, a whole month or year. None for anything said relative to now.
pub fn days(value: &str) -> Option<(i64, i64)> {
    if lens::time_pick(value).is_some() {
        return None;
    }
    let now = now();
    let end = |d: Date| -> i64 {
        let (y, m, _) = d.ymd();
        let last = |y: i64, m: u32| {
            scour_ui::calendar::month(y, m)
                .iter()
                .rfind(|x| x.here)
                .map_or(d.day, |x| x.n)
        };
        match d.form {
            DateForm::MonthYear => last(y, m),
            DateForm::Year => last(y, 12),
            _ => d.day,
        }
    };
    match lens::time_said(value, now, local_offset(now))? {
        TimeSaid::Date(d) => Some((d.day, end(d))),
        TimeSaid::Range(a, b) => Some((a.day, b.day)),
        _ => None,
    }
}

#[cfg(test)]
#[path = "qtime_tests.rs"]
mod tests;
