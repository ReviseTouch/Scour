//! The shapes the report is made of: an age strip, a row per weighed folder, a
//! bar by kind. Pure — no frame and no colour — so every width and every string
//! can be checked without a terminal.
//!
//! Every number here comes from `scour-chart`, which every face draws from: a
//! share shown in the terminal is the share shown in the window.

use scour_chart::{Rest, bar, fold, segment_cells, shares};
use scour_core::{DirUsage, UsageResponse};
use scour_ui::format;

/// The size column, the bar and the share: the same width on every row, so a
/// column of folders can be read down rather than across.
pub const SIZE: usize = 10;
pub const BAR: usize = 16;
pub const SHARE: usize = 6;
/// What a row spends before the name and after it: the mark, the size, the bar
/// and the share, with two spaces between each.
const FIXED: usize = 3 + SIZE + 2 + BAR + 2 + 2 + SHARE;
/// Under this many columns the report says less rather than wrapping: the
/// legends drop their percentages, then the rows drop the file counts.
pub const NARROW: usize = 60;
/// A name cut shorter than this says nothing at all.
const LEAST_NAME: usize = 8;
/// A strip shorter than this is not a distribution, so it is drawn wider than
/// the area and clipped instead.
const LEAST_STRIP: usize = 20;

/// What fits in a given width: how wide the name may be, and what was dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fit {
    pub name: usize,
    /// The file-count column, zero when there is no room for it.
    pub files: usize,
    /// Whether a legend says its percentages.
    pub percents: bool,
}

/// What a row can afford in `width` columns, given file counts `files` wide.
pub fn fit(width: usize, files: usize) -> Fit {
    let wide = width >= NARROW;
    let counted = wide && width >= FIXED + LEAST_NAME + 2 + files;
    let files = if counted { files } else { 0 };
    let name = width
        .saturating_sub(FIXED + if files > 0 { files + 2 } else { 0 })
        .max(LEAST_NAME);
    Fit {
        name,
        files,
        percents: wide,
    }
}

/// How wide the file counts of the shown children come to, for the column.
pub fn files_wide(children: &[DirUsage], shown: usize, group: char) -> usize {
    children
        .iter()
        .take(shown)
        .map(|c| format::grouped(c.files, group).chars().count())
        .max()
        .unwrap_or(1)
}

/// The cells of a strip: which band, and how many cells it takes, in band
/// order. A share too small for a cell gets none; the legend carries its number.
pub fn strip(values: &[u64], width: usize) -> Vec<(usize, usize)> {
    segment_cells(values, width.max(LEAST_STRIP))
        .into_iter()
        .enumerate()
        .filter(|(_, cells)| *cells > 0)
        .collect()
}

/// The legend under a strip: every band, its word, and its whole-number share.
/// Without `percents` the words stand alone — the colours still carry the order.
pub fn legend(values: &[u64], words: &[String], percents: bool) -> Vec<(usize, String)> {
    let share = shares(values, 0);
    words
        .iter()
        .enumerate()
        .map(|(i, word)| {
            let said = match percents {
                true => format!("{word} {:.0}", share.get(i).copied().unwrap_or(0.0)),
                false => word.clone(),
            };
            (i, said)
        })
        .collect()
}

/// How many of these legend entries fit in `width` columns. The words are the
/// catalogue's, so this is measured rather than assumed: a language with longer
/// words names fewer kinds, and the rest of them become one slice.
pub fn named(words: &[String], width: usize, percents: bool) -> usize {
    let mut left = width;
    let mut fit = 0;
    for word in words {
        // Two spaces between entries, and three for a share of up to a hundred.
        let cost = word.chars().count() + 2 + if percents { 3 } else { 0 };
        if cost > left {
            break;
        }
        left -= cost;
        fit += 1;
    }
    fit.max(1)
}

/// One weighed folder, laid out. The bar is cut into that folder's own age
/// bands, so a row says how big and how old at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// Whether the cursor is on this row.
    pub here: bool,
    pub size: String,
    /// The bar's filled cells: a band, and the glyphs that band owns.
    pub bar: Vec<(usize, String)>,
    /// The blank cells after them, which hold the name column in place.
    pub pad: usize,
    pub name: String,
    pub share: String,
    /// Empty when the column was dropped.
    pub files: String,
}

impl Row {
    /// The row as the terminal receives it, with the colour taken out — what
    /// the layout is asserted against, and what the drawing is checked to paint.
    #[cfg(test)]
    pub fn text(&self) -> String {
        let mut out = format!(" {} {}  ", if self.here { '▸' } else { ' ' }, self.size);
        for (_, glyphs) in &self.bar {
            out.push_str(glyphs);
        }
        out.extend(std::iter::repeat_n(' ', self.pad));
        out.push_str("  ");
        out.push_str(&self.name);
        out.push_str("  ");
        out.push_str(&self.share);
        if !self.files.is_empty() {
            out.push_str("  ");
            out.push_str(&self.files);
        }
        out
    }
}

/// The shares of the parent: one per shown child, then one for everything else
/// — the folders under the fold and the files in the folder itself. They add up
/// to a hundred, so the rest line is the remainder and never a second rounding.
pub fn parts(usage: &UsageResponse, shown: usize) -> (Vec<f64>, Option<Rest>) {
    let shown = shown.min(usage.children.len());
    let mut values: Vec<u64> = usage.children[..shown].iter().map(|c| c.bytes).collect();
    let taken: u64 = values.iter().sum();
    let left = usage.root.bytes.saturating_sub(taken);
    let count = (usage.child_count as usize).saturating_sub(shown);
    let rest = match left > 0 || count > 0 {
        true => Some(Rest { count, value: left }),
        false => None,
    };
    if let Some(rest) = &rest {
        values.push(rest.value);
    }
    (shares(&values, 1), rest)
}

/// One row: the size, a bar of the folder's weight against the heaviest of
/// them, cut into its age bands, the name, its share and how many files.
pub fn row(
    child: &DirUsage,
    here: bool,
    most: u64,
    share: f64,
    fit: &Fit,
    mark: (char, char),
) -> Row {
    let drawn = bar(child.bytes as f64 / most.max(1) as f64, BAR);
    let filled: Vec<char> = drawn.chars().take_while(|c| *c != ' ').collect();
    // A folder whose bytes carry no time at all still gets a bar: the oldest
    // band, which is the quiet one, rather than a gap in the column.
    let cells = match child.age.iter().sum::<u64>() {
        0 => {
            let mut only = vec![0; child.age.len()];
            if let Some(last) = only.last_mut() {
                *last = filled.len();
            }
            only
        }
        _ => segment_cells(&child.age, filled.len()),
    };
    let mut bands = Vec::new();
    let mut from = 0;
    for (band, cells) in cells.iter().enumerate() {
        if *cells == 0 {
            continue;
        }
        bands.push((band, filled[from..from + cells].iter().collect::<String>()));
        from += cells;
    }
    let name = crate::draw::cut(scour_ui::path::leaf(&child.path), fit.name);
    Row {
        here,
        size: format!("{:>SIZE$}", format::compact_bytes(child.bytes, mark.1)),
        bar: bands,
        pad: BAR - filled.len(),
        name: format!("{name:<0$}", fit.name),
        share: format!("{:>SHARE$}", percent(share, mark.1)),
        files: match fit.files {
            0 => String::new(),
            wide => format!("{:>wide$}", format::grouped(child.files, mark.0)),
        },
    }
}

/// The line under the rows: everything not shown, as one. `template` is the
/// catalogue's, with `{n}` for how many folders were left out.
pub fn rest_line(rest: &Rest, share: f64, template: &str, mark: (char, char)) -> String {
    format!(
        "   …  {}  {}  {}",
        template.replace("{n}", &rest.count.to_string()),
        format::compact_bytes(rest.value, mark.1),
        percent(share, mark.1),
    )
}

/// The kinds worth a slice of their own, largest first, and the rest as one.
pub fn kinds(counts: &[(String, u64)], keep: usize) -> (Vec<(String, u64)>, Option<Rest>) {
    let some: Vec<(String, u64)> = counts
        .iter()
        .filter(|(_, n)| *n > 0)
        .cloned()
        .collect::<Vec<_>>();
    let folded = fold(some, |k| k.1, keep);
    (folded.kept, folded.rest)
}

/// A share as this language writes it: one decimal, its own mark, a per cent.
fn percent(share: f64, decimal: char) -> String {
    let said = format!("{share:.1}%");
    match decimal {
        '.' => said,
        mark => said.replace('.', &mark.to_string()),
    }
}

#[cfg(test)]
mod tests {
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
}
