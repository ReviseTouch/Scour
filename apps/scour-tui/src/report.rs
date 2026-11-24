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
mod tests;
