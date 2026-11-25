//! The time panel's numbers: its presets, its four scales and the bars an
//! `age` facet fills. Every age is counted back from now as the service
//! counts it: a row `floor((now - mtime) / 86400)` days old falls in the
//! first edge `e` with `days <= e`, anything past the last in `older`.

use super::TIMES;
use crate::format::DAY;

/// One preset: the value it writes after `dm:`, its msgid, and the edge of
/// the `age` facet whose band is its newest-to-oldest end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimePreset {
    pub value: &'static str,
    pub label: &'static str,
    /// `dm:Nd` holds what is under N days old: the bands up to edge `N - 1`.
    pub edge: u32,
}

/// The age edges that count the presets: today (under 24 h), yesterday
/// (under 48 h, which `dm:yesterday` means), 7, 30 and 365 days.
pub const PRESET_EDGES: [u32; 5] = [0, 1, 6, 29, 364];

const fn preset(i: usize) -> TimePreset {
    TimePreset {
        value: TIMES[i].0,
        label: TIMES[i].1,
        edge: PRESET_EDGES[i],
    }
}

/// [`TIMES`] with their edges, newest first.
pub const TIME_PRESETS: [TimePreset; 5] = [preset(0), preset(1), preset(2), preset(3), preset(4)];

/// A facet's bands by edge, from its `(key, count)` pairs in any order: the
/// count for each of `edges`, and last what is `older`. A band it left out
/// holds nothing.
pub fn by_edge<'a>(edges: &[u32], facets: impl IntoIterator<Item = (&'a str, u64)>) -> Vec<u64> {
    let mut out = vec![0; edges.len() + 1];
    for (key, n) in facets {
        let at = match key {
            "older" => Some(edges.len()),
            k => k
                .parse::<u32>()
                .ok()
                .and_then(|e| edges.iter().position(|&x| x == e)),
        };
        if let Some(i) = at {
            out[i] += n;
        }
    }
    out
}

/// Each preset's count from a facet asked with [`PRESET_EDGES`]: a preset is
/// a window, so it holds its own band and every newer one.
pub fn preset_counts<'a>(facets: impl IntoIterator<Item = (&'a str, u64)>) -> [u64; 5] {
    let bands = by_edge(&PRESET_EDGES, facets);
    let mut out = [0; 5];
    let mut sum = 0;
    for (i, n) in bands.iter().take(5).enumerate() {
        sum += n;
        out[i] = sum;
    }
    out
}

/// The query the time panel counts over: the query less its `dm:` terms.
/// Ask `facets{query, by: [Age{edges: PRESET_EDGES}, Age{edges:
/// scale.edges()}]}`, one walk for the presets and the bars (the bridge:
/// `by=age,age&edges=0,1,6,29,364;<scale's>`).
pub fn time_ask(text: &str) -> String {
    super::without_field(text, "dm")
}

/// How far back one bar of the histogram reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scale {
    Day,
    Week,
    Month,
    Year,
}

/// The scales, in the order the switch lists them.
pub const SCALES: [Scale; 4] = [Scale::Day, Scale::Week, Scale::Month, Scale::Year];

impl Scale {
    /// The msgid on its switch.
    pub fn label(self) -> &'static str {
        match self {
            Scale::Day => "Day",
            Scale::Week => "Week",
            Scale::Month => "Month",
            Scale::Year => "Year",
        }
    }

    /// How many bars it draws.
    pub fn bars(self) -> usize {
        match self {
            Scale::Day => 30,
            Scale::Week => 26,
            Scale::Month => 24,
            Scale::Year => 10,
        }
    }

    /// Days in one bar: the language's own units, `w` 7, `m` 30, `y` 365.
    pub fn days(self) -> u32 {
        match self {
            Scale::Day => 1,
            Scale::Week => 7,
            Scale::Month => 30,
            Scale::Year => 365,
        }
    }

    /// The letter a term counts its bars in: `dm:4w..1w`.
    pub fn unit(self) -> &'static str {
        match self {
            Scale::Day => "d",
            Scale::Week => "w",
            Scale::Month => "m",
            Scale::Year => "y",
        }
    }

    /// The `age` facet's edges, in days, ascending: bar `k` from now holds
    /// ages `k` to `k + 1` units, so its edge is `(k + 1) * days - 1`.
    pub fn edges(self) -> Vec<u32> {
        (1..=self.bars() as u32)
            .map(|k| k * self.days() - 1)
            .collect()
    }

    /// How much of a date the axis ends are said with.
    pub fn ends(self) -> super::DateForm {
        match self {
            Scale::Day | Scale::Week => super::DateForm::DayMonth,
            Scale::Month => super::DateForm::MonthYear,
            Scale::Year => super::DateForm::Year,
        }
    }
}

/// What a selection of bars writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The bars chosen and nothing else.
    Between,
    /// The bars chosen and everything older.
    Before,
    /// The bars chosen and everything newer.
    After,
}

pub const MODES: [Mode; 3] = [Mode::Between, Mode::Before, Mode::After];

impl Mode {
    /// The msgid on its switch.
    pub fn label(self) -> &'static str {
        match self {
            Mode::Between => "Between",
            Mode::Before => "Before",
            Mode::After => "After",
        }
    }
}

/// One bar: how many rows, and the instants (unix seconds) it reaches from
/// and to, the older first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bar {
    pub count: u64,
    pub from: i64,
    pub to: i64,
}

/// A scale's bars as drawn, **oldest first**, left to right as time runs: the
/// last bar ends now. A selection names bars by these indices.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Histogram {
    pub scale: Scale,
    pub bars: Vec<Bar>,
    /// Rows older than the first bar: not drawn, but not nothing.
    pub older: u64,
    /// The tallest bar's count, which a bar's height is a share of.
    pub peak: u64,
    /// The axis: where the first bar starts and the last ends, in seconds;
    /// said with [`Scale::ends`].
    pub from: i64,
    pub to: i64,
}

/// The bars of a facet asked with `scale.edges()` at `now` (unix seconds).
pub fn histogram<'a>(
    scale: Scale,
    facets: impl IntoIterator<Item = (&'a str, u64)>,
    now: i64,
) -> Histogram {
    let bands = by_edge(&scale.edges(), facets);
    let n = scale.bars();
    let unit = i64::from(scale.days()) * DAY;
    let bars: Vec<Bar> = (0..n)
        .map(|i| {
            let k = (n - 1 - i) as i64;
            Bar {
                count: bands[n - 1 - i],
                from: now - (k + 1) * unit,
                to: now - k * unit,
            }
        })
        .collect();
    Histogram {
        scale,
        peak: bars.iter().map(|b| b.count).max().unwrap_or(0),
        older: bands[n],
        from: now - n as i64 * unit,
        to: now,
        bars,
    }
}

#[cfg(test)]
#[path = "time_tests.rs"]
mod tests;
