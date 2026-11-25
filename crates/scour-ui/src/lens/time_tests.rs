use super::*;
use crate::lens::{TIMES, time_pick::rel};

/// The service's rule for a row `age` seconds old (`facets.rs`): the first
/// edge `e` with `floor(age / 86400) <= e`, else past the last.
fn band(edges: &[u32], age: i64) -> usize {
    let days = age.max(0) / DAY;
    edges
        .iter()
        .position(|&e| days <= i64::from(e))
        .unwrap_or(edges.len())
}

#[test]
fn the_presets_are_the_lens_s_bands_with_their_edges() {
    let values: Vec<(&str, &str)> = TIME_PRESETS.iter().map(|p| (p.value, p.label)).collect();
    assert_eq!(values, TIMES);
    let edges: Vec<u32> = TIME_PRESETS.iter().map(|p| p.edge).collect();
    assert_eq!(edges, PRESET_EDGES);
}

/// A preset counts exactly the rows its term finds: every age up to two
/// years, in hours, not on a day's boundary (where `dm:Nd` takes one second
/// more than the facet).
#[test]
fn a_preset_counts_what_its_term_finds() {
    for p in TIME_PRESETS {
        let window = rel(p.value).unwrap().secs();
        for hours in 0..(730 * 24) {
            let age = hours * 3_600 + 1_800;
            let found = age <= window;
            let counted =
                band(&PRESET_EDGES, age) <= PRESET_EDGES.iter().position(|&e| e == p.edge).unwrap();
            assert_eq!(found, counted, "{} at {hours} h", p.value);
        }
    }
}

/// The "last 24 hours" chip once showed what is under 48 hours old.
#[test]
fn today_is_the_first_band_alone_and_each_preset_holds_the_newer_ones() {
    let facets = [("1", 3), ("0", 5), ("older", 100), ("29", 7), ("364", 11)];
    assert_eq!(preset_counts(facets), [5, 8, 8, 15, 26]);
    assert_eq!(preset_counts([]), [0; 5]);
    assert_eq!(
        by_edge(&PRESET_EDGES, [("6", 2), ("9", 50), ("older", 4)]),
        [0, 0, 2, 0, 0, 4],
        "a key that is no edge is not counted anywhere"
    );
}

#[test]
fn each_scale_asks_its_bars_in_the_language_s_units() {
    let day = Scale::Day.edges();
    assert_eq!(day, (0..30).collect::<Vec<u32>>());
    let week = Scale::Week.edges();
    assert_eq!((week.len(), week[0], week[1], week[25]), (26, 6, 13, 181));
    let month = Scale::Month.edges();
    assert_eq!((month.len(), month[0], month[23]), (24, 29, 719));
    let year = Scale::Year.edges();
    assert_eq!((year.len(), year[0], year[9]), (10, 364, 3649));
    for s in SCALES {
        let e = s.edges();
        assert_eq!(e.len(), s.bars(), "{s:?}");
        assert!(e.windows(2).all(|w| w[0] < w[1]), "{s:?} not ascending");
    }
    let labels: Vec<&str> = SCALES.iter().map(|s| s.label()).collect();
    assert_eq!(labels, ["Day", "Week", "Month", "Year"]);
    let modes: Vec<&str> = MODES.iter().map(|m| m.label()).collect();
    assert_eq!(modes, ["Between", "Before", "After"]);
}

#[test]
fn the_bars_run_oldest_first_and_the_last_ends_now() {
    let now = 1_800_000_000;
    let h = histogram(
        Scale::Week,
        [("6", 9), ("13", 4), ("181", 2), ("older", 70)],
        now,
    );
    assert_eq!(h.bars.len(), 26);
    assert_eq!(h.bars[25].count, 9, "the newest week is drawn last");
    assert_eq!(h.bars[24].count, 4);
    assert_eq!(h.bars[0].count, 2, "the oldest week is drawn first");
    assert_eq!((h.older, h.peak), (70, 9));
    assert_eq!((h.bars[25].from, h.bars[25].to), (now - 7 * DAY, now));
    assert_eq!(h.bars[0].from, now - 26 * 7 * DAY);
    assert_eq!((h.from, h.to), (now - 26 * 7 * DAY, now));
    assert!(h.bars.windows(2).all(|w| w[0].to == w[1].from));
    assert_eq!(Scale::Month.ends(), crate::lens::DateForm::MonthYear);
}

#[test]
fn a_bar_spans_the_ages_its_facet_band_counts() {
    let now = 1_800_000_000;
    for scale in SCALES {
        let h = histogram(scale, [], now);
        for (i, bar) in h.bars.iter().enumerate() {
            let age = now - (bar.from + bar.to) / 2;
            let k = band(&scale.edges(), age);
            assert_eq!(k, scale.bars() - 1 - i, "{scale:?} bar {i}");
        }
    }
}

#[test]
fn the_time_panel_counts_over_the_query_less_its_own_term() {
    assert_eq!(time_ask("rapor dm:7d ext:pdf"), "rapor ext:pdf");
    assert_eq!(
        time_ask("rapor !modified:7d|dm:1y kind:doc"),
        "rapor kind:doc"
    );
}
