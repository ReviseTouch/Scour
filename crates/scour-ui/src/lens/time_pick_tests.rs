use super::*;
use crate::lens::{Histogram, histogram, term_at};

fn t(q: &str) -> Term {
    term_at(q, q.len()).unwrap()
}

fn sel(scale: Scale, mode: Mode, first: usize, last: usize) -> Selected {
    Selected {
        scale,
        mode,
        first,
        last,
    }
}

#[test]
fn a_run_reaching_now_is_a_window_and_one_before_it_a_band_older_first() {
    let dm = t("dm:");
    assert_eq!(
        time_term(&dm, sel(Scale::Day, Mode::Between, 29, 29)),
        "dm:1d"
    );
    assert_eq!(
        time_term(&dm, sel(Scale::Day, Mode::Between, 23, 29)),
        "dm:7d"
    );
    assert_eq!(
        time_term(&dm, sel(Scale::Day, Mode::Between, 0, 22)),
        "dm:30d..7d"
    );
    assert_eq!(
        time_term(&dm, sel(Scale::Week, Mode::Between, 24, 22)),
        "dm:4w..1w"
    );
    assert_eq!(
        time_term(&dm, sel(Scale::Year, Mode::Between, 8, 8)),
        "dm:2y..1y"
    );
    assert_eq!(
        time_term(&dm, sel(Scale::Month, Mode::Before, 0, 20)),
        "dm:<3m"
    );
    assert_eq!(
        time_term(&dm, sel(Scale::Week, Mode::After, 23, 25)),
        "dm:>3w"
    );
    assert_eq!(time_term(&dm, sel(Scale::Day, Mode::Before, 10, 29)), "");
    assert_eq!(
        time_term(&dm, sel(Scale::Day, Mode::Between, 0, 99)),
        "dm:30d"
    );
}

#[test]
fn a_choice_keeps_the_negation_and_writes_the_canonical_field() {
    assert_eq!(
        time_term(&t("!modified:7d"), sel(Scale::Week, Mode::Between, 24, 25)),
        "!dm:2w"
    );
    assert_eq!(time_preset_term(&t("!dm:30d..7d"), 2), "!dm:7d");
    assert_eq!(time_preset_term(&t("dm:"), 0), "dm:today");
}

/// What the panel writes, it reads back: every run on every scale.
#[test]
fn every_run_reads_back_to_itself() {
    for scale in SCALES {
        let n = scale.bars();
        for first in 0..n {
            for last in first..n {
                let modes = [
                    (Mode::Between, first, last),
                    (Mode::Before, 0, last),
                    (Mode::After, first, n - 1),
                ];
                for (mode, a, b) in modes {
                    let s = sel(scale, mode, a, b);
                    let term = time_term(&t("dm:"), s);
                    if term.is_empty() {
                        continue;
                    }
                    let back = time_pick(&t(&term).value).unwrap_or_else(|| panic!("{term}"));
                    assert_eq!(back.selected, Some(s), "{term}");
                }
            }
        }
    }
}

#[test]
fn a_preset_reads_back_to_itself_and_to_its_bars() {
    for (i, p) in TIME_PRESETS.iter().enumerate() {
        let back = time_pick(p.value).unwrap();
        assert_eq!(back.preset, Some(i), "{}", p.value);
        assert!(back.selected.is_some(), "{}", p.value);
    }
    let seven = time_pick("7d").unwrap().selected.unwrap();
    assert_eq!(seven, sel(Scale::Day, Mode::Between, 23, 29));
    let year = time_pick("1y").unwrap().selected.unwrap();
    assert_eq!(year, sel(Scale::Year, Mode::Between, 9, 9));
}

#[test]
fn other_spellings_of_the_same_choice_read_the_same() {
    let preset = |v: &str| time_pick(v).and_then(|p| p.preset);
    assert_eq!(preset("24h"), Some(0));
    assert_eq!(preset("bugün"), Some(0));
    assert_eq!(preset("48saat"), Some(1));
    assert_eq!(preset("dün"), Some(1));
    assert_eq!(preset("1w"), Some(2));
    assert_eq!(preset("hafta"), Some(2));
    assert_eq!(preset("1m"), Some(3));
    assert_eq!(preset("365d"), Some(4));
    assert_eq!(preset("1yıl"), Some(4));
    assert_eq!(preset(">7d"), None, "After is not a preset");
    let bars = |v: &str| time_pick(v).and_then(|p| p.selected);
    assert_eq!(
        bars("2hafta"),
        Some(sel(Scale::Week, Mode::Between, 24, 25))
    );
    assert_eq!(bars("14d"), Some(sel(Scale::Day, Mode::Between, 16, 29)));
    assert_eq!(bars("60d"), Some(sel(Scale::Month, Mode::Between, 22, 23)));
    assert_eq!(bars("30d.."), Some(sel(Scale::Day, Mode::Between, 0, 29)));
    assert_eq!(bars("..7d"), Some(sel(Scale::Day, Mode::Before, 0, 22)));
    assert_eq!(bars("<=7D"), Some(sel(Scale::Day, Mode::Before, 0, 22)));
    assert_eq!(bars(">=3w"), Some(sel(Scale::Week, Mode::After, 23, 25)));
}

#[test]
fn what_the_panel_cannot_show_is_none() {
    for v in [
        "",
        "2026-11-03",
        "2026-11-12..2026-11-25",
        "<2025-01-01",
        "7d..30d",
        "=7d",
        "12h",
        "31d..7d",
        "11y",
        "<10y",
        "<0d",
        "..",
        "rapor",
    ] {
        assert_eq!(time_pick(v), None, "{v}");
    }
}

#[test]
fn a_negated_band_round_trips_through_the_panel() {
    for q in ["!dm:4w..1w", "!dm:<3m", "!dm:>2y", "!dm:7d"] {
        let term = t(q);
        let back = time_pick(&term.value).unwrap();
        let written = match back.preset {
            Some(i) => time_preset_term(&term, i),
            None => time_term(&term, back.selected.unwrap()),
        };
        assert_eq!(written, q);
    }
}

/// The bars a run names hold the counts the facet gave them.
#[test]
fn a_selection_names_the_bars_it_was_drawn_from() {
    let h: Histogram = histogram(Scale::Day, [("0", 1), ("6", 2), ("7", 4)], 0);
    let s = time_pick("7d").unwrap().selected.unwrap();
    let sum: u64 = h.bars[s.first..=s.last].iter().map(|b| b.count).sum();
    assert_eq!(sum, 3, "today and the seventh day, not the eighth");
}
