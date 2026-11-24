//! The time strip: how many bars, how far back, and which bar a day falls in.

/// How many bars the time ribbon has.
pub const BAR_COUNT: usize = 24;

/// How far back the ribbon reaches, in days.
pub const BAR_SPAN_DAYS: f64 = 730.0;

/// The upper edge of each bar, in days, newest first. Logarithmic, so the last
/// day, week and year all have room: a linear scale spends 23 of 24 bars on
/// "older than a month". The service is asked for exactly these edges.
pub fn bar_edges() -> Vec<u32> {
    let mut v: Vec<u32> = (0..BAR_COUNT)
        .map(|i| {
            let t = 1.0 - (i as f64) / (BAR_COUNT as f64);
            (((10f64.powf(t) - 1.0) / 9.0) * BAR_SPAN_DAYS)
                .round()
                .max(1.0) as u32
        })
        .collect();
    v.reverse();
    v
}

/// Which of the six time bands an age in days falls in, oldest last — the same
/// six the rows use, so a bar and the rows it stands for match.
pub fn band_of(days: f64) -> usize {
    match days {
        d if d < 1.0 => 0,
        d if d < 7.0 => 1,
        d if d < 30.0 => 2,
        d if d < 365.0 => 3,
        d if d < 730.0 => 4,
        _ => 5,
    }
}

#[cfg(test)]
mod ribbon_tests {
    use super::*;

    /// The edges, written out rather than recomputed: every face must draw the
    /// same twenty-four numbers.
    #[test]
    fn the_edges_match_the_page() {
        let want: [u32; BAR_COUNT] = [
            8, 17, 27, 38, 50, 63, 78, 94, 111, 131, 152, 175, 201, 230, 261, 295, 333, 375, 421,
            471, 527, 588, 656, 730,
        ];
        assert_eq!(bar_edges(), want, "the ribbon's bars moved");
    }

    /// The ribbon reads left to right as time does, and `bar_edges()` is newest
    /// first: the edges are upper bounds, so every face reverses them before
    /// drawing or the bars run backwards under the axis.
    #[test]
    fn the_edges_are_newest_first_and_the_ribbon_is_not() {
        let e = bar_edges();
        assert!(
            e[0] < e[e.len() - 1],
            "the edges stopped being newest first"
        );
        let drawn: Vec<u32> = e.iter().rev().copied().collect();
        assert_eq!(drawn[0], 730, "the leftmost bar is not the oldest");
        assert!(
            drawn[drawn.len() - 1] < 10,
            "the rightmost bar is not the newest"
        );
    }

    /// A bar's colour is the colour the rows in it get.
    #[test]
    fn a_band_is_one_of_six() {
        assert_eq!(band_of(0.5), 0);
        assert_eq!(band_of(3.0), 1);
        assert_eq!(band_of(10.0), 2);
        assert_eq!(band_of(100.0), 3);
        assert_eq!(band_of(400.0), 4);
        assert_eq!(band_of(1000.0), 5);
    }
}
