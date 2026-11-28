//! A document's blocks as a reader walks it: words in a look, paragraphs
//! opened and closed, lists, quotes, code, tables and pictures. Shared by the
//! readers of markup — Markdown, HTML, an e-book's chapters, a message's body
//! — so a heading, a list or a table is drawn the same whatever wrote it.

use scour_flow::{Align, Block, Cell, Flow, Para, Picture, Row, Run, Table};

use crate::flow::LINK;

/// A fenced block's ground, and the grey of a quote and of what stands for a
/// picture.
pub(crate) const CODE_GROUND: [u8; 3] = [0xf6, 0xf8, 0xfa];
pub(crate) const QUIET: [u8; 3] = [0x57, 0x60, 0x6a];
/// Ems a list's level and a quote indent by.
const STEP_EM: f32 = 1.6;

/// How the words being read look: each a count, so what opens twice closes
/// twice.
#[derive(Default, Clone, Copy)]
pub(crate) struct Look {
    pub bold: u8,
    pub italic: u8,
    pub underline: u8,
    pub strike: u8,
    pub link: u8,
    pub code: u8,
    pub quiet: u8,
}

/// A list being read: its next number, or none for bullets.
struct List {
    next: Option<u64>,
}

#[derive(Default)]
pub(crate) struct Builder {
    blocks: Vec<Block>,
    /// The paragraph being read, if one is open.
    para: Option<Para>,
    pub look: Look,
    lists: Vec<List>,
    /// A list item's mark, for the first paragraph it opens.
    mark: Option<String>,
    quotes: usize,
    heading: u8,
    /// Text as it is written, white space and all: inside a code block.
    pub verbatim: bool,
    /// A table being read: its alignments, its rows, the row and the cell.
    table: Option<(Vec<Align>, Vec<Row>)>,
    row: Option<Row>,
    cell: Option<Vec<Para>>,
    /// Pictures' bytes so far, as base64.
    pictures: usize,
    text: usize,
    cut: bool,
}

impl Builder {
    /// Past a preview's bounds: the reader stops, and the drawing says so.
    pub fn full(&mut self) -> bool {
        if self.text > scour_flow::MOST_TEXT || self.blocks.len() > scour_flow::MOST_BLOCKS {
            self.cut = true;
        }
        self.cut
    }

    pub fn finish(mut self) -> Flow {
        self.flush();
        if let Some((_, rows)) = self.table.take() {
            self.blocks.push(Block::Table(table(rows)));
        }
        Flow {
            blocks: self.blocks,
            cut: self.cut,
        }
        .bounded()
    }

    fn indent(&self) -> f32 {
        let lists = self.lists.len() as f32;
        (lists + self.quotes as f32) * STEP_EM
    }

    /// The paragraph words go into, opened as the place they are read in
    /// says: a heading, a list item's first line, a quote.
    fn open(&mut self) -> &mut Para {
        if self.para.is_none() {
            let (size, before, after) = match self.heading {
                1 => (1.9, 0.6, 0.3),
                2 => (1.5, 0.6, 0.25),
                3 => (1.25, 0.5, 0.2),
                4 => (1.1, 0.4, 0.15),
                5 => (1.0, 0.4, 0.1),
                6 => (0.9, 0.4, 0.1),
                _ => (1.0, 0.0, 0.45),
            };
            self.para = Some(Para {
                indent: self.indent(),
                before,
                after: if self.lists.is_empty() || self.heading > 0 {
                    after
                } else {
                    0.15
                },
                size,
                heading: self.heading,
                mark: self.mark.take().unwrap_or_default(),
                ..Para::default()
            });
        }
        self.para.as_mut().expect("opened above")
    }

    /// Words in the look they are read in. `code` sets them fixed-width
    /// whatever the look says.
    pub fn words(&mut self, text: &str, code: bool) {
        if text.is_empty() {
            return;
        }
        self.text += text.len();
        let look = self.look;
        let heading = self.heading > 0;
        let run = Run {
            text: text.to_owned(),
            bold: look.bold > 0 || heading,
            italic: look.italic > 0,
            underline: look.link > 0 || look.underline > 0,
            strike: look.strike > 0,
            color: if look.link > 0 {
                Some(LINK)
            } else if look.quiet > 0 || self.quotes > 0 {
                Some(QUIET)
            } else {
                None
            },
            code: code || look.code > 0,
            ..Run::default()
        };
        let para = self.open();
        match para.runs.last_mut() {
            Some(last) if same(last, &run) => last.text.push_str(text),
            _ => para.runs.push(run),
        }
    }

    /// Words of markup, white space folded as a browser folds it: a run of it
    /// is one space, and none opens a paragraph.
    pub fn folded(&mut self, text: &str) {
        if self.verbatim {
            self.words(text, true);
            return;
        }
        let mut out = String::with_capacity(text.len());
        let mut space = self
            .para
            .as_ref()
            .and_then(|p| p.runs.last())
            .is_none_or(|r| r.text.ends_with([' ', '\n']));
        for c in text.chars() {
            if c.is_whitespace() && c != '\u{a0}' {
                if !space {
                    out.push(' ');
                    space = true;
                }
            } else {
                out.push(c);
                space = false;
            }
        }
        if !out.is_empty() {
            self.words(&out, false);
        }
    }

    /// The open paragraph ended: into the cell being read, or the document.
    pub fn flush(&mut self) {
        let Some(mut para) = self.para.take() else {
            return;
        };
        if let Some(last) = para.runs.last_mut()
            && !last.code
        {
            let kept = last.text.trim_end_matches(' ').len();
            last.text.truncate(kept);
        }
        para.runs.retain(|r| !r.text.is_empty());
        if para.runs.is_empty() && para.mark.is_empty() {
            return;
        }
        match self.cell.as_mut() {
            Some(cell) => cell.push(para),
            None => self.blocks.push(Block::Para(para)),
        }
    }

    pub fn heading(&mut self, level: u8) {
        self.flush();
        self.heading = level.min(6);
    }

    pub fn end_heading(&mut self) {
        self.flush();
        self.heading = 0;
    }

    pub fn quote(&mut self) {
        self.flush();
        self.quotes += 1;
    }

    pub fn end_quote(&mut self) {
        self.flush();
        self.quotes = self.quotes.saturating_sub(1);
    }

    /// A list opens: numbered from `first`, or bulleted.
    pub fn list(&mut self, first: Option<u64>) {
        self.flush();
        self.lists.push(List { next: first });
    }

    pub fn end_list(&mut self) {
        self.flush();
        self.lists.pop();
        // The gap a list leaves under it.
        if self.lists.is_empty()
            && let Some(Block::Para(p)) = self.blocks.last_mut()
        {
            p.after = p.after.max(0.45);
        }
    }

    /// An item of the list being read; outside one, a bullet of its own.
    pub fn item(&mut self) {
        self.flush();
        let level = self.lists.len().max(1);
        let mark = match self.lists.last_mut() {
            Some(List { next: Some(n) }) => {
                let mark = format!("{n}.");
                *n += 1;
                mark
            }
            _ => ["•", "◦", "▪"][(level - 1) % 3].to_owned(),
        };
        self.mark = Some(mark);
    }

    /// A task's box, for the item it is read in.
    pub fn task(&mut self, done: bool) {
        let mark = if done { "☑" } else { "☐" };
        match self.para.as_mut().filter(|p| p.runs.is_empty()) {
            Some(p) => p.mark = mark.into(),
            None => self.mark = Some(mark.into()),
        }
    }

    pub fn rule(&mut self) {
        self.flush();
        self.blocks.push(Block::Break);
    }

    /// A block of code: its lines as they are, on a ground of their own.
    pub fn code_block(&mut self, code: &str) {
        self.flush();
        let code = code.trim_end_matches(['\n', '\r']);
        if code.is_empty() {
            return;
        }
        self.text += code.len();
        let para = Para {
            runs: vec![Run {
                text: code.to_owned(),
                code: true,
                // What a program printed, set apart from the program.
                color: (self.look.quiet > 0).then_some(QUIET),
                ..Run::default()
            }],
            indent: self.indent(),
            before: 0.15,
            after: 0.6,
            size: 0.92,
            shade: Some(CODE_GROUND),
            ..Para::default()
        };
        match self.cell.as_mut() {
            Some(cell) => cell.push(para),
            None => self.blocks.push(Block::Para(para)),
        }
    }

    /// A small line above what follows, quietly: a code block's language, a
    /// message's sender.
    pub fn caption(&mut self, text: &str) {
        self.flush();
        self.look.quiet += 1;
        self.words(text, false);
        self.look.quiet -= 1;
        if let Some(p) = self.para.as_mut() {
            p.size = 0.85;
            p.after = 0.0;
        }
        self.flush();
    }

    /// A table opens, its columns aligned as given (as many as are known).
    /// One inside another is read into the outer one's cell.
    pub fn table(&mut self, aligns: Vec<Align>) -> bool {
        if self.table.is_some() {
            return false;
        }
        self.flush();
        self.table = Some((aligns, Vec::new()));
        true
    }

    pub fn row(&mut self, head: bool) {
        if self.table.is_none() {
            return;
        }
        self.end_row();
        self.row = Some(Row {
            cells: Vec::new(),
            head,
        });
    }

    pub fn cell(&mut self) {
        if self.table.is_none() {
            return;
        }
        self.end_cell();
        if self.row.is_none() {
            self.row(false);
        }
        self.cell = Some(Vec::new());
    }

    pub fn end_cell(&mut self) {
        if self.cell.is_none() {
            return;
        }
        self.flush();
        let paras = self.cell.take().unwrap_or_default();
        let column = self.row.as_ref().map_or(0, |r| r.cells.len());
        let align = self
            .table
            .as_ref()
            .and_then(|(a, _)| a.get(column).copied())
            .unwrap_or_default();
        let paras = paras
            .into_iter()
            .map(|p| Para {
                align,
                after: 0.0,
                ..p
            })
            .collect();
        if let Some(row) = self.row.as_mut() {
            row.cells.push(Cell {
                paras,
                span: 1,
                shade: None,
            });
        }
    }

    pub fn end_row(&mut self) {
        self.end_cell();
        if let (Some(row), Some((_, rows))) = (self.row.take(), self.table.as_mut())
            && !row.cells.is_empty()
        {
            rows.push(row);
        }
    }

    pub fn end_table(&mut self) {
        self.end_row();
        if let Some((_, rows)) = self.table.take()
            && !rows.is_empty()
        {
            self.blocks.push(Block::Table(table(rows)));
        }
    }

    /// A picture's bytes, if they are one a face draws and there is room for
    /// them; its size in ems, if the document gives one.
    pub fn picture(&mut self, data: &[u8], size: Option<(f32, f32)>) -> bool {
        // A cell holds words: a picture in one is named instead.
        if self.cell.is_some() {
            return false;
        }
        let Some(mime) = mime_of(data) else {
            return false;
        };
        let encoded = data.len().div_ceil(3) * 4;
        if encoded > scour_flow::MOST_PICTURE || self.pictures + encoded > scour_flow::MOST_PICTURES
        {
            return false;
        }
        self.pictures += encoded;
        self.flush();
        let (width, height) = size.unwrap_or((24.0, 16.0));
        let block = Block::Picture(Picture {
            mime: mime.to_owned(),
            data: scour_flow::encode(data),
            width,
            height,
        });
        self.blocks.push(block);
        true
    }
}

/// The kinds of picture both faces draw, by their first bytes.
pub(crate) fn mime_of(data: &[u8]) -> Option<&'static str> {
    if data.starts_with(b"\x89PNG") {
        Some("image/png")
    } else if data.starts_with(b"\xFF\xD8") {
        Some("image/jpeg")
    } else if data.starts_with(b"GIF8") {
        Some("image/gif")
    } else if data.starts_with(b"BM") {
        Some("image/bmp")
    } else {
        None
    }
}

fn same(a: &Run, b: &Run) -> bool {
    (
        a.bold,
        a.italic,
        a.underline,
        a.strike,
        a.color,
        a.code,
        a.raise,
    ) == (
        b.bold,
        b.italic,
        b.underline,
        b.strike,
        b.color,
        b.code,
        b.raise,
    )
}

/// A table's rows, its columns as wide as their words ask, within reason.
fn table(mut rows: Vec<Row>) -> Table {
    let columns = rows.iter().map(|r| r.cells.len()).max().unwrap_or(0);
    // A short row is filled, so the grid's lines run across.
    for row in &mut rows {
        while row.cells.len() < columns {
            row.cells.push(Cell {
                paras: Vec::new(),
                span: 1,
                shade: None,
            });
        }
    }
    let mut want = vec![3.0f32; columns];
    for row in &rows {
        for (i, cell) in row.cells.iter().enumerate() {
            let chars: usize = cell.paras.iter().map(|p| p.text().chars().count()).sum();
            want[i] = want[i].max((chars as f32).min(40.0));
        }
    }
    let total: f32 = want.iter().sum::<f32>().max(1.0);
    Table {
        widths: want.iter().map(|w| w / total).collect(),
        rows,
        ruled: true,
    }
}
