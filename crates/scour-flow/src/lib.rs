//! A word processor's document as a preview shows it: paragraphs, tables and
//! pictures that flow to the width of the panel, with no pages. What the
//! document says about how it looks — bold, colour, a heading's size, a
//! table's columns — is kept; where its lines break and its pages end is not.
//!
//! Sizes are in ems of the body's text, so a face picks the body's size and
//! everything else follows: a heading is `1.4`, an indent `2.5`.

mod b64;
mod html;
mod number;
mod sheet;

pub use b64::{decode, encode};
pub use html::html;
pub use number::{Locale, general, show};
pub use sheet::{
    GridCell, GridRow, MOST_CELL_TEXT, MOST_SHEET_COLUMNS, MOST_SHEET_ROWS, MOST_SHEETS, Sheet,
};

use serde::{Deserialize, Serialize};

/// The extensions read this way: a word processor's, whose text flows, and a
/// spreadsheet's, whose sheets are grids.
pub fn flows(extension: &str) -> bool {
    matches!(
        extension.to_ascii_lowercase().as_str(),
        "docx" | "doc" | "xlsx" | "xls"
    )
}

/// Blocks drawn at most; a preview past them says it stopped.
pub const MOST_BLOCKS: usize = 4_000;
/// Bytes of text drawn at most.
pub const MOST_TEXT: usize = 400_000;
/// One picture's bytes, base64 included, and all of them together.
pub const MOST_PICTURE: usize = 4 << 20;
pub const MOST_PICTURES: usize = 12 << 20;
/// A table's columns and rows.
pub const MOST_COLUMNS: usize = 40;
pub const MOST_ROWS: usize = 500;

/// The pictures a face can decode: what a browser and the `image` crate share.
pub const PICTURE_TYPES: &[&str] = &["image/png", "image/jpeg", "image/gif", "image/bmp"];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Flow {
    pub blocks: Vec<Block>,
    /// Stopped short of the end: the rest was more than a preview draws.
    #[serde(default)]
    pub cut: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "lowercase")]
pub enum Block {
    Para(Para),
    Table(Table),
    Picture(Picture),
    /// A page, a section or a drawn line ended here.
    Break,
    /// A spreadsheet's sheet: a book is these alone.
    Sheet(Sheet),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Para {
    pub runs: Vec<Run>,
    #[serde(default)]
    pub align: Align,
    /// From the left edge, and the first line from that, in ems.
    #[serde(default)]
    pub indent: f32,
    #[serde(default)]
    pub first: f32,
    /// Space above and below, in ems.
    #[serde(default)]
    pub before: f32,
    #[serde(default)]
    pub after: f32,
    /// Its text against the body's: `1.0` is the body.
    #[serde(default = "one")]
    pub size: f32,
    /// 1–6 for a heading, 0 for a paragraph.
    #[serde(default)]
    pub heading: u8,
    /// A list's mark, set before the first line in the indent: "•" or "3.".
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mark: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shade: Option<[u8; 3]>,
}

fn one() -> f32 {
    1.0
}

impl Default for Para {
    fn default() -> Self {
        Para {
            runs: Vec::new(),
            align: Align::Left,
            indent: 0.0,
            first: 0.0,
            before: 0.0,
            after: 0.0,
            size: 1.0,
            heading: 0,
            mark: String::new(),
            shade: None,
        }
    }
}

impl Para {
    /// Its words, as one string.
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
}

/// Justified text is set ragged: in a column a few words wide, spreading them
/// opens holes, and the document's own width is not the panel's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Align {
    #[default]
    Left,
    Center,
    Right,
}

/// A stretch of text that looks one way. A line break inside it is `\n`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Run {
    pub text: String,
    #[serde(default, skip_serializing_if = "no")]
    pub bold: bool,
    #[serde(default, skip_serializing_if = "no")]
    pub italic: bool,
    #[serde(default, skip_serializing_if = "no")]
    pub underline: bool,
    #[serde(default, skip_serializing_if = "no")]
    pub strike: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<[u8; 3]>,
    /// A highlighter's colour behind it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub marker: Option<[u8; 3]>,
    /// Raised (1) or lowered (-1) off the line.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub raise: i8,
}

fn no(b: &bool) -> bool {
    !*b
}

fn is_zero(n: &i8) -> bool {
    *n == 0
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Table {
    /// Each column's share of the width, summing to one.
    pub widths: Vec<f32>,
    pub rows: Vec<Row>,
    /// Lines between the cells.
    #[serde(default)]
    pub ruled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// A heading row.
    #[serde(default)]
    pub head: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Cell {
    pub paras: Vec<Para>,
    /// Columns it covers.
    #[serde(default = "one_column")]
    pub span: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shade: Option<[u8; 3]>,
}

fn one_column() -> u32 {
    1
}

/// A picture, its bytes as base64 so the protocol's JSON stays text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Picture {
    pub mime: String,
    pub data: String,
    /// The size the document gives it, in ems; a face shrinks it to fit.
    pub width: f32,
    pub height: f32,
}

/// Equal to itself: every number in it is finite once [`Flow::bounded`], and a
/// flow crosses the protocol only bounded.
impl Eq for Flow {}

impl Flow {
    /// Held to the bounds whatever wrote it: the extractor that fills a flow
    /// reads untrusted files and is not trusted to have kept to them either.
    pub fn bounded(mut self) -> Flow {
        let mut budget = Budget {
            blocks: MOST_BLOCKS,
            text: MOST_TEXT,
            pictures: MOST_PICTURES,
        };
        let mut kept = Vec::with_capacity(self.blocks.len().min(MOST_BLOCKS));
        let mut sheets = 0;
        for block in self.blocks {
            if budget.blocks == 0 || budget.text == 0 {
                self.cut = true;
                break;
            }
            if matches!(block, Block::Sheet(_)) {
                sheets += 1;
                if sheets > MOST_SHEETS {
                    self.cut = true;
                    break;
                }
            }
            budget.blocks -= 1;
            if let Some(b) = bound_block(block, &mut budget) {
                kept.push(b);
            }
        }
        self.blocks = kept;
        self
    }

    /// Every word of it, a paragraph a line: what a search finds in it.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for b in &self.blocks {
            match b {
                Block::Para(p) => {
                    out.push_str(&p.text());
                    out.push('\n');
                }
                Block::Table(t) => {
                    for r in &t.rows {
                        for c in &r.cells {
                            for p in &c.paras {
                                out.push_str(&p.text());
                                out.push('\n');
                            }
                        }
                    }
                }
                Block::Sheet(s) => {
                    for r in &s.rows {
                        let line: Vec<&str> = r.cells.iter().map(|c| c.text.as_str()).collect();
                        out.push_str(&line.join("\t"));
                        out.push('\n');
                    }
                }
                Block::Picture(_) | Block::Break => {}
            }
        }
        out
    }

    /// Its sheets, when it is a spreadsheet's book.
    pub fn sheets(&self) -> impl Iterator<Item = &Sheet> {
        self.blocks.iter().filter_map(|b| match b {
            Block::Sheet(s) => Some(s),
            _ => None,
        })
    }
}

pub(crate) struct Budget {
    pub(crate) blocks: usize,
    pub(crate) text: usize,
    pub(crate) pictures: usize,
}

fn bound_block(block: Block, budget: &mut Budget) -> Option<Block> {
    match block {
        Block::Para(p) => Some(Block::Para(bound_para(p, budget))),
        Block::Table(t) => {
            let rows: Vec<Row> = t
                .rows
                .into_iter()
                .take(MOST_ROWS)
                .map(|r| Row {
                    head: r.head,
                    cells: r
                        .cells
                        .into_iter()
                        .take(MOST_COLUMNS)
                        .map(|c| Cell {
                            span: c.span.clamp(1, MOST_COLUMNS as u32),
                            shade: c.shade,
                            paras: c.paras.into_iter().map(|p| bound_para(p, budget)).collect(),
                        })
                        .collect(),
                })
                .filter(|r| !r.cells.is_empty())
                .collect();
            if rows.is_empty() {
                return None;
            }
            let columns = rows
                .iter()
                .map(|r| r.cells.iter().map(|c| c.span as usize).sum::<usize>())
                .max()
                .unwrap_or(1)
                .clamp(1, MOST_COLUMNS);
            Some(Block::Table(Table {
                widths: shares(&t.widths, columns),
                rows,
                ruled: t.ruled,
            }))
        }
        Block::Picture(p) => {
            let fits = PICTURE_TYPES.contains(&p.mime.as_str())
                && p.data.len() <= MOST_PICTURE
                && p.data.len() <= budget.pictures
                && b64::is_base64(&p.data)
                && p.width.is_finite()
                && p.height.is_finite();
            if !fits {
                return None;
            }
            budget.pictures -= p.data.len();
            Some(Block::Picture(Picture {
                width: p.width.clamp(0.5, 200.0),
                height: p.height.clamp(0.5, 400.0),
                ..p
            }))
        }
        Block::Break => Some(Block::Break),
        Block::Sheet(s) => Some(Block::Sheet(sheet::bound(s, budget))),
    }
}

fn bound_para(mut p: Para, budget: &mut Budget) -> Para {
    for run in &mut p.runs {
        if run.text.len() > budget.text {
            let mut end = budget.text;
            while !run.text.is_char_boundary(end) {
                end -= 1;
            }
            run.text.truncate(end);
        }
        budget.text -= run.text.len();
        run.raise = run.raise.clamp(-1, 1);
    }
    p.runs.retain(|r| !r.text.is_empty());
    p.indent = finite(p.indent).clamp(0.0, 12.0);
    p.first = finite(p.first).clamp(-4.0, 4.0);
    p.before = finite(p.before).clamp(0.0, 3.0);
    p.after = finite(p.after).clamp(0.0, 3.0);
    p.size = if p.size.is_finite() {
        p.size.clamp(0.6, 2.4)
    } else {
        1.0
    };
    p.heading = p.heading.min(6);
    p.mark = p.mark.chars().take(8).collect();
    p
}

fn finite(n: f32) -> f32 {
    if n.is_finite() { n } else { 0.0 }
}

/// `widths` as shares of `columns` that sum to one; even when they are missing,
/// too few, or nonsense.
fn shares(widths: &[f32], columns: usize) -> Vec<f32> {
    let mut w: Vec<f32> = widths
        .iter()
        .take(columns)
        .map(|&x| if x.is_finite() && x > 0.0 { x } else { 0.0 })
        .collect();
    let valid = w.iter().filter(|&&x| x > 0.0).count();
    let mean = if valid == 0 {
        1.0
    } else {
        w.iter().sum::<f32>() / valid as f32
    };
    for x in &mut w {
        if *x <= 0.0 {
            *x = mean;
        }
    }
    w.resize(columns, mean);
    let total: f32 = w.iter().sum();
    w.iter().map(|x| x / total).collect()
}

#[cfg(test)]
mod tests;
