//! The public API: the numbers every face draws from.

use scour_chart::{Rest, arcs, bar, dashes, fold, segment_cells, segments, shares};

#[test]
fn shares_add_up_to_a_hundred_whatever_the_rounding() {
    // Three equal thirds would be 33.3 each and sum to 99.9; one of them takes
    // the missing tenth, the first, being the largest by the tie rule.
    assert_eq!(shares(&[1, 1, 1], 1), vec![33.4, 33.3, 33.3]);
    assert_eq!(shares(&[1, 1, 1], 0), vec![34.0, 33.0, 33.0]);
    // The live home: Projeler and eleven others.
    let home = [
        651_571_270_524u64,
        31_643_455_926,
        15_738_588_363,
        14_110_521_835,
        10_863_393_896,
        10_852_991_366,
        9_242_344_819,
        8_271_160_692,
        7_898_028_191,
        7_372_347_728,
        5_889_237_540,
        4_856_318_717,
        20_649_608_929,
    ];
    let s = shares(&home, 1);
    let sum: f64 = s.iter().sum();
    assert!((sum - 100.0).abs() < 1e-9, "{s:?} sums to {sum}");
    assert!(
        (s[0] - 81.55).abs() < 0.06,
        "Projeler is 81.55%, shown as {}",
        s[0]
    );
    assert_eq!(shares(&[0, 0], 1), vec![0.0, 0.0]);
    assert_eq!(shares(&[7], 2), vec![100.0]);
    assert_eq!(shares(&[], 1), Vec::<f64>::new());
}

#[test]
fn segments_meet_end_to_end() {
    let s = segments(&[1, 3]);
    assert_eq!(s[0].start, 0.0);
    assert_eq!(s[0].width, 0.25);
    assert_eq!(s[1].start, 0.25);
    assert_eq!(s[1].width, 0.75);
    assert!(segments(&[0, 0]).iter().all(|s| s.width == 0.0));
}

#[test]
fn fold_keeps_the_largest_and_sums_the_rest() {
    let f = fold(vec![("b", 5u64), ("a", 9), ("c", 1), ("d", 5)], |x| x.1, 2);
    assert_eq!(f.kept, vec![("a", 9), ("b", 5)], "stable: b before d");
    assert_eq!(f.rest, Some(Rest { count: 2, value: 6 }));
    let all = fold(vec![("a", 1u64)], |x| x.1, 3);
    assert_eq!(all.rest, None);
    assert_eq!(all.kept.len(), 1);
}

#[test]
fn a_ring_is_the_same_shares_as_degrees_and_as_dashes() {
    let a = arcs(&[1, 1, 2]);
    assert_eq!(a[0].start, 0.0);
    assert_eq!(a[0].sweep, 90.0);
    assert_eq!(a[2].start, 180.0);
    assert_eq!(a[2].sweep, 180.0);
    let total: f64 = a.iter().map(|x| x.sweep).sum();
    assert!((total - 360.0).abs() < 1e-9);

    let c = 339.29;
    let d = dashes(&[1, 1, 2], c);
    assert!((d[0].length - c / 4.0).abs() < 1e-9);
    assert_eq!(d[0].offset, 0.0);
    assert!(
        (d[1].offset + c / 4.0).abs() < 1e-9,
        "negative and cumulative"
    );
    assert!((d[2].offset + c / 2.0).abs() < 1e-9);
    let drawn: f64 = d.iter().map(|x| x.length).sum();
    assert!((drawn - c).abs() < 1e-9);
}

#[test]
fn block_bars_show_eighths_and_line_up() {
    assert_eq!(bar(1.0, 4), "████");
    assert_eq!(bar(0.5, 4), "██  ");
    assert_eq!(bar(0.125, 4), "▌   ", "half a cell of four");
    assert_eq!(bar(0.0, 4), "    ");
    assert_eq!(bar(0.001, 4), "▏   ", "anything above zero shows");
    assert_eq!(bar(2.0, 3), "███", "clamped");
    assert_eq!(bar(0.99, 2).chars().count(), 2);
    for w in 1..12 {
        for n in 0..=20 {
            let s = bar(n as f64 / 20.0, w);
            assert_eq!(s.chars().count(), w, "{n}/20 of {w}: {s:?}");
        }
    }
}

#[test]
fn segment_cells_fill_the_width_exactly() {
    assert_eq!(segment_cells(&[1, 1, 1], 10), vec![4, 3, 3]);
    assert_eq!(
        segment_cells(&[816, 40, 20, 124], 40).iter().sum::<usize>(),
        40
    );
    assert_eq!(
        segment_cells(&[1000, 1], 10),
        vec![10, 0],
        "too small for a cell"
    );
    assert_eq!(segment_cells(&[0, 0], 10), vec![0, 0]);
    assert_eq!(segment_cells(&[3, 1], 0), vec![0, 0]);
}
