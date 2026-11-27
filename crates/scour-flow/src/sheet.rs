//! A spreadsheet's sheet as a preview draws it: a grid, each column at its
//! width, each cell's number through its own format in the reader's punctuation.

use serde::{Deserialize, Serialize};

use crate::{Align, Budget, Locale, general, show};

/// Sheets drawn of one book, and of each sheet its rows and columns.
pub const MOST_SHEETS: usize = 24;
pub const MOST_SHEET_ROWS: usize = 2_000;
pub const MOST_SHEET_COLUMNS: usize = 80;
/// One cell's text.
pub const MOST_CELL_TEXT: usize = 2_000;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Sheet {
    pub name: String,
    /// Each column's width, in ems of the grid's text.
    pub widths: Vec<f32>,
    pub rows: Vec<GridRow>,
    /// The first row stays in view as the rest scroll: a heading row.
    #[serde(default)]
    pub frozen: bool,
    /// Rows or columns past the bounds were left out.
    #[serde(default)]
    pub cut: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GridRow {
    pub cells: Vec<GridCell>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GridCell {
    /// What it says when it is not a number, or a number's text as stored.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// A number or a date, shown through `format`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<f64>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub format: String,
    /// Columns it covers.
    #[serde(default = "one")]
    pub span: u32,
    #[serde(default, skip_serializing_if = "no")]
    pub bold: bool,
    #[serde(default, skip_serializing_if = "no")]
    pub italic: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[u8; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shade: Option<[u8; 3]>,
    /// Its own alignment; none is a number's right and a word's left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub align: Option<Align>,
    /// Its text wraps inside it rather than running on.
    #[serde(default, skip_serializing_if = "no")]
    pub wrap: bool,
}

fn one() -> u32 {
    1
}

fn no(b: &bool) -> bool {
    !*b
}

impl Default for GridCell {
    fn default() -> Self {
        GridCell {
            text: String::new(),
            value: None,
            format: String::new(),
            span: 1,
            bold: false,
            italic: false,
            color: None,
            shade: None,
            align: None,
            wrap: false,
        }
    }
}

impl GridCell {
    /// What the cell shows, punctuated for `locale`.
    pub fn shown(&self, locale: &Locale) -> String {
        match self.value {
            Some(v) if self.format.is_empty() => general(v, locale),
            Some(v) => show(v, &self.format, locale),
            None => self.text.clone(),
        }
    }

    /// Nothing in it, and nothing behind it: room for a neighbour's words.
    pub fn blank(&self) -> bool {
        self.value.is_none() && self.text.trim().is_empty() && self.shade.is_none()
    }

    /// Words that run on past its right edge when the cells there are blank,
    /// as a spreadsheet lets them: left-set text, not a number.
    pub fn spills(&self) -> bool {
        self.value.is_none()
            && !self.wrap
            && self.alignment() == Align::Left
            && !self.text.is_empty()
    }

    /// Where its text sits: its own, else a number's right and a word's left.
    pub fn alignment(&self) -> Align {
        self.align.unwrap_or(if self.value.is_some() {
            Align::Right
        } else {
            Align::Left
        })
    }
}

pub(crate) fn bound(mut sheet: Sheet, budget: &mut Budget) -> Sheet {
    if sheet.rows.len() > MOST_SHEET_ROWS {
        sheet.rows.truncate(MOST_SHEET_ROWS);
        sheet.cut = true;
    }
    sheet.name = sheet.name.chars().take(64).collect();
    let mut kept = Vec::with_capacity(sheet.rows.len());
    for mut row in sheet.rows {
        if budget.text == 0 {
            sheet.cut = true;
            break;
        }
        if row.cells.len() > MOST_SHEET_COLUMNS {
            row.cells.truncate(MOST_SHEET_COLUMNS);
            sheet.cut = true;
        }
        for cell in &mut row.cells {
            if cell.text.len() > MOST_CELL_TEXT.min(budget.text) {
                let mut end = MOST_CELL_TEXT.min(budget.text);
                while !cell.text.is_char_boundary(end) {
                    end -= 1;
                }
                cell.text.truncate(end);
            }
            budget.text -= cell.text.len().min(budget.text);
            cell.format = cell.format.chars().take(128).collect();
            cell.span = cell.span.clamp(1, MOST_SHEET_COLUMNS as u32);
            if cell.value.is_some_and(|v| !v.is_finite()) {
                cell.value = None;
            }
        }
        kept.push(row);
    }
    sheet.rows = kept;
    let columns = sheet
        .rows
        .iter()
        .map(|r| r.cells.iter().map(|c| c.span as usize).sum::<usize>())
        .max()
        .unwrap_or(0)
        .min(MOST_SHEET_COLUMNS);
    sheet.widths.truncate(columns);
    sheet.widths.resize(columns, 5.0);
    for w in &mut sheet.widths {
        *w = if w.is_finite() {
            w.clamp(1.5, 40.0)
        } else {
            5.0
        };
    }
    sheet
}
