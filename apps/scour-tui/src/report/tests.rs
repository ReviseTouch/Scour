use super::*;

fn folder(path: &str, bytes: u64, files: u64, age: [u64; 6]) -> DirUsage {
    DirUsage {
        path: path.into(),
        bytes,
        disk: bytes,
        files,
        age,
    }
}

/// The live home, cut down: the shape a row takes at a hundred columns.
fn home() -> UsageResponse {
    UsageResponse {
        root: folder("/home/hasan", 798_000_000_000, 2_124_324, [0; 6]),
        children: vec![
            folder(
                "/home/hasan/Projeler",
                651_571_270_524,
                896_648,
                [
                    89_000_000_000,
                    12_000_000_000,
                    499_571_270_524,
                    47_000_000_000,
                    4_000_000_000,
                    0,
                ],
            ),
            folder(
                "/home/hasan/.config",
                31_643_455_926,
                98_380,
                [
                    12_600_000_000,
                    200_000_000,
                    4_143_455_926,
                    14_700_000_000,
                    0,
                    0,
                ],
            ),
        ],
        child_count: 63,
        took_us: 821_000,
    }
}

#[test]
fn a_row_lands_column_by_column() {
    let usage = home();
    let (share, rest) = parts(&usage, 2);
    let fit = fit(100, 7);
    assert_eq!(fit.name, 50, "the name takes what the columns leave");
    let most = usage.children[0].bytes;
    let first = row(&usage.children[0], true, most, share[0], &fit, ('.', ','));
    assert_eq!(
        first.text(),
        " ▸   606,8 GB  ████████████████  Projeler                                             81,6%  896.648"
    );
    let second = row(&usage.children[1], false, most, share[1], &fit, ('.', ','));
    assert_eq!(
        second.text(),
        "      29,5 GB  ▊                 .config                                               4,0%   98.380"
    );
    // Every glyph of the bar belongs to a band, and the bar stays BAR wide.
    let cells: usize = first.bar.iter().map(|(_, g)| g.chars().count()).sum();
    assert_eq!(cells + first.pad, BAR);
    assert_eq!(first.text().chars().count(), 100, "and the row fills it");
    let rest = rest.expect("61 folders under the fold, and the home's own files");
    assert_eq!(rest.count, 61);
    assert_eq!(
        rest_line(&rest, share[2], "the other {n} folders", ('.', ',')),
        "   …  the other 61 folders  106,9 GB  14,4%"
    );
    let whole: f64 = share.iter().sum();
    assert!((whole - 100.0).abs() < 1e-9, "{share:?}");

    // A folder with no folders under it: one share, and nothing folded
    // away for a line to name.
    let alone = UsageResponse {
        root: folder("/home/hasan/vm", 7_400_000_000, 7, [0; 6]),
        children: Vec::new(),
        child_count: 0,
        took_us: 10,
    };
    let (share, rest) = parts(&alone, 6);
    assert_eq!(share, vec![100.0]);
    assert_eq!(rest.map(|r| r.count), Some(0));
}

#[test]
fn the_bar_carries_the_folders_own_age_bands() {
    let usage = home();
    let fit = fit(100, 7);
    let first = row(
        &usage.children[0],
        false,
        usage.children[0].bytes,
        81.7,
        &fit,
        ('.', ','),
    );
    let bands: Vec<usize> = first.bar.iter().map(|(band, _)| *band).collect();
    assert_eq!(bands, vec![0, 1, 2, 3], "today, week, month, six months");
    // Nothing older than six months in it, so no band past the fourth.
    assert!(first.bar.iter().all(|(band, _)| *band < 4));
    // A folder with no ages at all is still drawn, in the oldest band.
    let blank = folder("/x/none", 1_000, 0, [0; 6]);
    let blank = row(&blank, false, 1_000, 100.0, &fit, ('.', ','));
    assert_eq!(blank.bar.len(), 1);
    assert_eq!(blank.bar[0].0, 5);
}

#[test]
fn a_narrow_report_drops_the_percentages_then_the_counts() {
    let wide = fit(100, 7);
    assert!(wide.percents && wide.files == 7);
    let tight = fit(59, 7);
    assert!(
        !tight.percents,
        "under sixty the legends lose their numbers"
    );
    assert_eq!(tight.files, 0, "and a row loses its file count");
    assert_eq!(tight.name, 18, "the name takes back what they left");
    assert_eq!(fit(20, 7).name, LEAST_NAME, "never cut past this");
    // Sixty columns is where it all still fits: a name of ten, and a count.
    assert_eq!(fit(60, 7).files, 7);
    assert_eq!(fit(60, 7).name, 10);
    assert!(fit(60, 7).percents);
    let usage = home();
    let (share, _) = parts(&usage, 2);
    let tight = row(
        &usage.children[1],
        false,
        usage.children[0].bytes,
        share[1],
        &tight,
        ('.', ','),
    );
    assert!(tight.files.is_empty());
    assert!(!tight.text().contains("98.380"));
    assert_eq!(
        tight.text(),
        "      29,5 GB  ▊                 .config               4,0%"
    );
    assert_eq!(
        tight.text().chars().count(),
        59,
        "never wider than the area"
    );
}

#[test]
fn a_legend_says_every_band_and_its_share() {
    let words: Vec<String> = ["today", "week", "month", "six months", "year", "older"]
        .iter()
        .map(|w| w.to_string())
        .collect();
    let age = [143, 34, 648, 154, 5, 16];
    let said: Vec<String> = legend(&age, &words, true)
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    assert_eq!(
        said,
        vec![
            "today 14",
            "week 3",
            "month 65",
            "six months 15",
            "year 1",
            "older 2"
        ]
    );
    let quiet: Vec<String> = legend(&age, &words, false)
        .into_iter()
        .map(|(_, text)| text)
        .collect();
    assert_eq!(quiet, words, "the words stay, the numbers go");
}

#[test]
fn a_strip_fills_its_width_exactly() {
    let age = [143u64, 34, 648, 154, 5, 16];
    let cells: usize = strip(&age, 64).iter().map(|(_, n)| n).sum();
    assert_eq!(cells, 64);
    // The bands come back in order, and an empty one is left out.
    let bands: Vec<usize> = strip(&[1, 0, 1], 64).iter().map(|(b, _)| *b).collect();
    assert_eq!(bands, vec![0, 2]);
    // Never shorter than the least: a five-cell distribution is no picture.
    let cells: usize = strip(&age, 5).iter().map(|(_, n)| n).sum();
    assert_eq!(cells, LEAST_STRIP);
}

#[test]
fn a_legend_names_as_many_as_the_width_holds() {
    let words: Vec<String> = ["build output", "file", "folder", "data", "config", "doc"]
        .iter()
        .map(|w| w.to_string())
        .collect();
    // 17 + 9 + 11 = 37 with the shares, and `data` would make 46.
    assert_eq!(named(&words, 40, true), 3);
    assert_eq!(named(&words, 40, false), 4, "without them one more fits");
    assert_eq!(named(&words, 4, true), 1, "never nothing at all");
}

#[test]
fn the_kinds_keep_six_and_fold_the_rest() {
    let counts: Vec<(String, u64)> = [
        ("build", 75_090u64),
        ("file", 49_295),
        ("folder", 41_307),
        ("data", 16_128),
        ("config", 5_248),
        ("doc", 4_786),
        ("image", 4_000),
        ("audio", 3_146),
        ("video", 1_000),
        ("font", 0),
    ]
    .iter()
    .map(|(k, n)| (k.to_string(), *n))
    .collect();
    let (kept, rest) = kinds(&counts, 6);
    assert_eq!(kept.len(), 6);
    assert_eq!(kept[0].0, "build");
    assert_eq!(
        rest,
        Some(Rest {
            count: 3,
            value: 8_146
        }),
        "the empty kind is not one of the others"
    );
}
