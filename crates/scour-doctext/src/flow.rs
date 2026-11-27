//! A word processor's document as a [`scour_flow::Flow`], and a spreadsheet's
//! book as its sheets: office_oxide's model of them, kept to what a preview
//! draws. Here, in the extractor's process, because the library parses what
//! anybody can put on a disk.

use std::io::Cursor;

use office_oxide::ir::{
    DocumentIR, Element, Heading, ImageFormat, InlineContent, List, Paragraph, ParagraphAlignment,
    Table, TextSpan, VerticalAlign,
};
use office_oxide::{Document, DocumentFormat};
use scour_flow::{Align, Block, Cell, Flow, GridCell, GridRow, Para, Picture, Row, Run};

/// The document in `bytes`, by its extension.
pub fn flow(bytes: &[u8], extension: &str) -> Result<Flow, String> {
    let format = match extension.to_ascii_lowercase().as_str() {
        "docx" => DocumentFormat::Docx,
        "doc" => DocumentFormat::Doc,
        "xlsx" => DocumentFormat::Xlsx,
        "xls" => DocumentFormat::Xls,
        "pptx" => DocumentFormat::Pptx,
        "ppt" => DocumentFormat::Ppt,
        other => return Err(format!("{other} does not flow")),
    };
    let doc =
        Document::from_reader(Cursor::new(bytes.to_vec()), format).map_err(|e| e.to_string())?;
    let ir = doc.to_ir();
    Ok(match format {
        DocumentFormat::Xlsx | DocumentFormat::Xls => sheets(&ir),
        DocumentFormat::Pptx | DocumentFormat::Ppt => slides(&ir),
        _ => from_ir(&ir),
    }
    .bounded())
}

/// A presentation's slides, each a card: its title, then its boxes top to
/// bottom and left to right, as a reader goes through them.
fn slides(ir: &DocumentIR) -> Flow {
    let mut out = Out {
        blocks: Vec::new(),
        body_pt: body_pt(ir),
        text: 0,
        pictures: 0,
        blanks: 0,
        cut: false,
    };
    let mut cards = Vec::new();
    for section in &ir.sections {
        if section.hidden {
            continue;
        }
        let mut order: Vec<&Element> = section.elements.iter().collect();
        // A box's place; the title, which has none, first.
        let place = |e: &Element| match e {
            Element::Heading(_) => (i64::MIN, i64::MIN),
            Element::TextBox(t) => (t.y_emu.unwrap_or(0), t.x_emu.unwrap_or(0)),
            Element::Shape(s) => (s.y_emu, s.x_emu),
            _ => (0, 0),
        };
        order.sort_by_key(|e| place(e));
        out.blocks.clear();
        out.blanks = 0;
        for e in order {
            out.elements(std::slice::from_ref(e), 0);
        }
        // A slide's blank lines are its layout, which a card does not keep.
        let blocks: Vec<Block> = std::mem::take(&mut out.blocks)
            .into_iter()
            .filter(|b| match b {
                Block::Para(p) => {
                    p.runs.iter().any(|r| !r.text.trim().is_empty()) || !p.mark.is_empty()
                }
                _ => true,
            })
            .collect();
        cards.push(Block::Slide(scour_flow::Slide {
            background: section.background_rgb,
            blocks,
        }));
        if out.cut || cards.len() >= scour_flow::MOST_BLOCKS / 8 {
            break;
        }
    }
    Flow {
        blocks: cards,
        cut: out.cut,
    }
}

/// A grid's text is drawn at 12 px: an em of it is nine points.
const GRID_PT: f32 = 9.0;
/// Rows a column's width is read from, when the book gives none.
const MEASURED_ROWS: usize = 300;

/// A book's sheets, each a grid; hidden ones and charts left out.
fn sheets(ir: &DocumentIR) -> Flow {
    let mut blocks = Vec::new();
    for (n, section) in ir.sections.iter().enumerate() {
        if section.hidden
            || section
                .sheet_kind
                .as_ref()
                .is_some_and(|k| !matches!(k, office_oxide::ir::SheetKind::Worksheet))
        {
            continue;
        }
        let name = section
            .title
            .clone()
            .unwrap_or_else(|| format!("Sheet{}", n + 1));
        let table = section.elements.iter().find_map(|e| match e {
            Element::Table(t) => Some(t),
            _ => None,
        });
        blocks.push(Block::Sheet(match table {
            Some(t) => sheet(name, t),
            None => scour_flow::Sheet {
                name,
                ..scour_flow::Sheet::default()
            },
        }));
        if blocks.len() >= scour_flow::MOST_SHEETS {
            break;
        }
    }
    Flow { blocks, cut: false }
}

fn sheet(name: String, t: &Table) -> scour_flow::Sheet {
    let mut rows: Vec<GridRow> = t
        .rows
        .iter()
        .take(scour_flow::MOST_SHEET_ROWS + 1)
        .map(|r| {
            let mut cells: Vec<GridCell> = r.cells.iter().map(grid_cell).collect();
            while cells.last().is_some_and(blank) {
                cells.pop();
            }
            GridRow { cells }
        })
        .collect();
    while rows.last().is_some_and(|r| r.cells.is_empty()) {
        rows.pop();
    }
    let cut = rows.len() > scour_flow::MOST_SHEET_ROWS;
    rows.truncate(scour_flow::MOST_SHEET_ROWS);
    let columns = rows
        .iter()
        .map(|r| r.cells.iter().map(|c| c.span as usize).sum::<usize>())
        .max()
        .unwrap_or(0);
    let widths = if t.column_widths_twips.len() >= columns && columns > 0 {
        t.column_widths_twips
            .iter()
            .take(columns)
            .map(|&w| w as f32 * PT_PER_TWIP / GRID_PT)
            .collect()
    } else {
        measured(&rows, columns)
    };
    // A first row of bold or shaded labels is the sheet's heading.
    let frozen = rows.len() > 2
        && rows[0].cells.iter().any(|c| !blank(c))
        && rows[0]
            .cells
            .iter()
            .filter(|c| !blank(c))
            .all(|c| c.bold || c.shade.is_some());
    scour_flow::Sheet {
        name,
        widths,
        rows,
        frozen,
        cut,
    }
}

fn blank(c: &GridCell) -> bool {
    c.blank()
}

/// Each column as wide as what it shows, near enough: a character of the
/// grid's text is a little over half an em.
fn measured(rows: &[GridRow], columns: usize) -> Vec<f32> {
    let locale = scour_flow::Locale::default();
    let mut chars = vec![0usize; columns];
    for r in rows.iter().take(MEASURED_ROWS) {
        let mut at = 0;
        for (i, c) in r.cells.iter().enumerate() {
            // Words with room beside them spill into it, as a title does: they
            // say nothing of their own column's width.
            let spills =
                c.value.is_none() && at + 1 < columns && r.cells.get(i + 1).is_none_or(blank);
            if c.span == 1 && at < columns && !spills {
                let longest = c
                    .shown(&locale)
                    .lines()
                    .map(|l| l.chars().count())
                    .max()
                    .unwrap_or(0);
                let n = if c.bold {
                    longest + longest / 8
                } else {
                    longest
                };
                chars[at] = chars[at].max(if c.wrap { n.min(24) } else { n });
            }
            at += c.span.max(1) as usize;
        }
    }
    chars
        .into_iter()
        .map(|n| (n as f32 * 0.58 + 1.0).clamp(2.5, 30.0))
        .collect()
}

fn grid_cell(c: &office_oxide::ir::TableCell) -> GridCell {
    use office_oxide::ir::CellDataType;
    let mut text = String::new();
    let mut first: Option<&TextSpan> = None;
    for e in &c.content {
        if let Element::Paragraph(p) = e {
            if !text.is_empty() {
                text.push('\n');
            }
            for i in &p.content {
                match i {
                    InlineContent::Text(t) => {
                        first.get_or_insert(t);
                        text.push_str(&t.text);
                    }
                    InlineContent::LineBreak => text.push('\n'),
                    _ => {}
                }
            }
        }
    }
    let value = match c.data_type {
        Some(CellDataType::Number | CellDataType::Date) | None => c.raw_number,
        _ => None,
    };
    let format = c
        .number_format
        .clone()
        .filter(|f| !f.eq_ignore_ascii_case("general"))
        .unwrap_or_default();
    // A colour that comes from a style is not in the model: on a dark fill,
    // which nobody fills behind black text, it was light.
    let dark = c.background_color.is_some_and(|[r, g, b]| {
        0.299 * f32::from(r) + 0.587 * f32::from(g) + 0.114 * f32::from(b) < 110.0
    });
    let color = first
        .and_then(|t| t.color)
        .or(dark.then_some([0xff, 0xff, 0xff]));
    GridCell {
        text,
        value,
        format,
        span: c.col_span.max(1),
        bold: first.is_some_and(|t| t.bold),
        italic: first.is_some_and(|t| t.italic),
        color,
        shade: c.background_color,
        align: c.text_align.as_ref().map(|a| align(&Some(a.clone()))),
        wrap: c.wrap_text,
    }
}

/// Points per twip, per EMU.
const PT_PER_TWIP: f32 = 1.0 / 20.0;
const PT_PER_EMU: f32 = 1.0 / 12_700.0;
/// The size a run with none of its own is taken to have.
const PLAIN_PT: f32 = 11.0;
/// Empty paragraphs in a row kept: they are a document's spacing, and a title
/// page's twenty of them are a screen of nothing.
const BLANKS: usize = 2;

struct Out {
    blocks: Vec<Block>,
    body_pt: f32,
    text: usize,
    pictures: usize,
    blanks: usize,
    cut: bool,
}

fn from_ir(ir: &DocumentIR) -> Flow {
    let mut out = Out {
        blocks: Vec::new(),
        body_pt: body_pt(ir),
        text: 0,
        pictures: 0,
        blanks: 0,
        cut: false,
    };
    // The first page's heading, once: a letterhead, a "confidential". The
    // rest repeat it, and a footer is mostly a page number.
    if let Some(first) = ir.sections.first()
        && let Some(head) = first.first_page_header.as_ref().or(first.header.as_ref())
    {
        out.elements(&head.content, 0);
    }
    for (i, section) in ir.sections.iter().enumerate() {
        if i > 0 {
            out.push(Block::Break);
        }
        out.elements(&section.elements, 0);
        if out.cut {
            break;
        }
    }
    Flow {
        blocks: out.blocks,
        cut: out.cut,
    }
}

/// The size most of the body's text is set in.
fn body_pt(ir: &DocumentIR) -> f32 {
    let mut by_size = std::collections::HashMap::<u32, usize>::new();
    fn count(els: &[Element], by_size: &mut std::collections::HashMap<u32, usize>) {
        for e in els {
            match e {
                Element::Paragraph(p) => {
                    for i in &p.content {
                        if let InlineContent::Text(t) = i
                            && let Some(h) = t.font_size_half_pt
                        {
                            *by_size.entry(h).or_default() += t.text.len();
                        }
                    }
                }
                Element::List(l) => {
                    for item in &l.items {
                        count(&item.content, by_size);
                    }
                }
                Element::Table(t) => {
                    for r in &t.rows {
                        for c in &r.cells {
                            count(&c.content, by_size);
                        }
                    }
                }
                Element::TextBox(t) => count(&t.content, by_size),
                _ => {}
            }
        }
    }
    for s in &ir.sections {
        count(&s.elements, &mut by_size);
    }
    by_size
        .into_iter()
        .max_by_key(|&(h, n)| (n, h))
        .map_or(PLAIN_PT, |(h, _)| h as f32 / 2.0)
        .clamp(6.0, 36.0)
}

impl Out {
    fn push(&mut self, block: Block) {
        if self.blocks.len() >= scour_flow::MOST_BLOCKS || self.text >= scour_flow::MOST_TEXT {
            self.cut = true;
            return;
        }
        if let Block::Para(p) = &block {
            if p.runs.iter().all(|r| r.text.trim().is_empty()) && p.mark.is_empty() {
                self.blanks += 1;
                if self.blanks > BLANKS {
                    return;
                }
            } else {
                self.blanks = 0;
            }
            self.text += p.runs.iter().map(|r| r.text.len()).sum::<usize>();
        } else {
            self.blanks = 0;
        }
        self.blocks.push(block);
    }

    fn elements(&mut self, els: &[Element], level: u8) {
        for e in els {
            if self.cut {
                return;
            }
            match e {
                Element::Heading(h) => {
                    let p = self.heading(h);
                    self.push(Block::Para(p));
                }
                Element::Paragraph(p) => {
                    let p = self.paragraph(p);
                    self.push(Block::Para(p));
                }
                Element::List(l) => {
                    for p in self.list(l, level) {
                        self.push(Block::Para(p));
                    }
                }
                Element::Table(t) => {
                    if let Some(b) = self.table(t) {
                        self.push(b);
                    }
                }
                Element::Image(img) => {
                    if let Some(b) = self.picture(img) {
                        self.push(b);
                    }
                }
                Element::TextBox(tb) => self.elements(&tb.content, level),
                Element::CodeBlock(c) => {
                    for line in c.content.lines() {
                        self.push(Block::Para(Para {
                            runs: vec![plain(line)],
                            ..Para::default()
                        }));
                    }
                }
                Element::PageBreak | Element::ThematicBreak => self.push(Block::Break),
                // A column break, a note, a drawn shape: nothing that flows.
                _ => {}
            }
        }
    }

    fn ems(&self, twips: Option<i32>) -> f32 {
        twips.map_or(0.0, |t| t as f32 * PT_PER_TWIP / self.body_pt)
    }

    fn paragraph(&self, p: &Paragraph) -> Para {
        let runs = runs(&p.content);
        Para {
            size: self.size_of(&p.content, 1.0),
            align: align(&p.alignment),
            indent: self.ems(p.indent_left_twips),
            first: self.ems(p.first_line_indent_twips),
            before: self.ems(p.space_before_twips.map(|t| t as i32)),
            after: self.ems(p.space_after_twips.map(|t| t as i32)),
            shade: p.background_color,
            runs,
            ..Para::default()
        }
    }

    fn heading(&self, h: &Heading) -> Para {
        let level = h.level.clamp(1, 6);
        let default = [1.6, 1.35, 1.2, 1.1, 1.0, 1.0][level as usize - 1];
        Para {
            size: self.size_of(&h.content, default),
            align: align(&h.alignment),
            indent: self.ems(h.indent_left_twips),
            first: self.ems(h.first_line_indent_twips),
            before: self.ems(h.space_before_twips.map(|t| t as i32)).max(0.4),
            after: self.ems(h.space_after_twips.map(|t| t as i32)).max(0.2),
            heading: level,
            runs: runs(&h.content),
            ..Para::default()
        }
    }

    /// The size most of a paragraph's text is set in, against the body's;
    /// `default` when it gives none.
    fn size_of(&self, content: &[InlineContent], default: f32) -> f32 {
        let mut best: Option<(usize, u32)> = None;
        for i in content {
            if let InlineContent::Text(t) = i
                && let Some(h) = t.font_size_half_pt
            {
                let n = t.text.len();
                if best.is_none_or(|(m, _)| n > m) {
                    best = Some((n, h));
                }
            }
        }
        best.map_or(default, |(_, h)| h as f32 / 2.0 / self.body_pt)
    }

    fn list(&self, l: &List, level: u8) -> Vec<Para> {
        let mut out = Vec::new();
        let start = l.start_number.unwrap_or(1);
        let depth = level.max(l.level);
        for (n, item) in l.items.iter().enumerate() {
            let mark = if l.ordered {
                format!("{}.", start as usize + n)
            } else if depth.is_multiple_of(2) {
                "•".to_owned()
            } else {
                "◦".to_owned()
            };
            let mut first = true;
            for e in &item.content {
                let mut p = match e {
                    Element::Paragraph(p) => self.paragraph(p),
                    Element::Heading(h) => self.heading(h),
                    _ => continue,
                };
                p.indent = p.indent.max(f32::from(depth) * 1.6);
                p.first = 0.0;
                if first {
                    p.mark.clone_from(&mark);
                    first = false;
                }
                out.push(p);
            }
            if let Some(nested) = &item.nested {
                out.extend(self.list(nested, depth + 1));
            }
        }
        out
    }

    fn table(&self, t: &Table) -> Option<Block> {
        let rows: Vec<Row> = t
            .rows
            .iter()
            .map(|r| Row {
                head: r.is_header || r.repeat_as_header,
                cells: r
                    .cells
                    .iter()
                    .map(|c| Cell {
                        paras: self.cell(&c.content),
                        span: c.col_span.max(1),
                        shade: c.background_color,
                    })
                    .collect(),
            })
            .filter(|r: &Row| !r.cells.is_empty())
            .collect();
        if rows.is_empty() {
            return None;
        }
        let ruled = t.border.is_some()
            || t.rows
                .iter()
                .any(|r| r.cells.iter().any(|c| c.border.is_some()));
        Some(Block::Table(scour_flow::Table {
            widths: t.column_widths_twips.iter().map(|&w| w as f32).collect(),
            rows,
            ruled,
        }))
    }

    /// A cell's paragraphs; a table inside one, a line a row.
    fn cell(&self, els: &[Element]) -> Vec<Para> {
        let mut out = Vec::new();
        for e in els {
            match e {
                Element::Paragraph(p) => out.push(self.paragraph(p)),
                Element::Heading(h) => out.push(self.heading(h)),
                Element::List(l) => out.extend(self.list(l, 0)),
                Element::Table(t) => {
                    for r in &t.rows {
                        let line: Vec<String> = r
                            .cells
                            .iter()
                            .map(|c| {
                                self.cell(&c.content)
                                    .iter()
                                    .map(Para::text)
                                    .collect::<Vec<_>>()
                                    .join(" ")
                            })
                            .collect();
                        out.push(Para {
                            runs: vec![plain(&line.join(" · "))],
                            ..Para::default()
                        });
                    }
                }
                Element::TextBox(tb) => out.extend(self.cell(&tb.content)),
                _ => {}
            }
        }
        out
    }

    fn picture(&mut self, img: &office_oxide::ir::Image) -> Option<Block> {
        let mime = match img.format.as_ref()? {
            ImageFormat::Png => "image/png",
            ImageFormat::Jpeg => "image/jpeg",
            ImageFormat::Gif => "image/gif",
            ImageFormat::Bmp => "image/bmp",
            ImageFormat::Tiff | ImageFormat::Emf | ImageFormat::Wmf => return None,
        };
        let data = img.data.as_ref()?;
        let encoded_len = data.len().div_ceil(3) * 4;
        if encoded_len > scour_flow::MOST_PICTURE
            || self.pictures + encoded_len > scour_flow::MOST_PICTURES
        {
            return None;
        }
        let (w_pt, h_pt) = match (img.display_width_emu, img.display_height_emu) {
            (Some(w), Some(h)) if w > 0 && h > 0 => (w as f32 * PT_PER_EMU, h as f32 * PT_PER_EMU),
            _ => match (img.pixel_width, img.pixel_height) {
                // At 96 dots an inch.
                (Some(w), Some(h)) if w > 0 && h > 0 => (w as f32 * 0.75, h as f32 * 0.75),
                _ => (240.0, 160.0),
            },
        };
        self.pictures += encoded_len;
        Some(Block::Picture(Picture {
            mime: mime.to_owned(),
            data: scour_flow::encode(data),
            width: w_pt / self.body_pt,
            height: h_pt / self.body_pt,
        }))
    }
}

fn align(a: &Option<ParagraphAlignment>) -> Align {
    match a {
        Some(ParagraphAlignment::Center) => Align::Center,
        Some(ParagraphAlignment::Right) => Align::Right,
        _ => Align::Left,
    }
}

fn plain(text: &str) -> Run {
    Run {
        text: text.to_owned(),
        ..Run::default()
    }
}

/// A paragraph's text, run by run; a line break joins the run before it.
fn runs(content: &[InlineContent]) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    for i in content {
        match i {
            InlineContent::Text(t) => out.push(run(t)),
            InlineContent::LineBreak => match out.last_mut() {
                Some(last) => last.text.push('\n'),
                None => out.push(plain("\n")),
            },
            InlineContent::FootnoteRef(f) | InlineContent::EndnoteRef(f) => out.push(Run {
                text: f.marker.clone().unwrap_or_else(|| f.note_id.to_string()),
                raise: 1,
                ..Run::default()
            }),
            _ => {}
        }
    }
    out
}

/// A link with no colour of its own looks like one.
const LINK: [u8; 3] = [0x09, 0x69, 0xda];

fn run(t: &TextSpan) -> Run {
    let link = t.hyperlink.is_some();
    Run {
        text: t.text.clone(),
        bold: t.bold,
        italic: t.italic,
        underline: t.underline.is_some() || (link && t.color.is_none()),
        strike: t.strikethrough,
        color: t.color.or(link.then_some(LINK)),
        marker: t.highlight,
        raise: match t.vertical_align {
            Some(VerticalAlign::Superscript) => 1,
            Some(VerticalAlign::Subscript) => -1,
            _ => 0,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn docx(markdown: &str) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        office_oxide::create::create_from_markdown_to_writer(
            markdown,
            DocumentFormat::Docx,
            &mut bytes,
        )
        .unwrap();
        bytes.into_inner()
    }

    #[test]
    fn a_document_s_headings_emphasis_lists_and_tables_flow() {
        let bytes = docx(
            "# Kira sözleşmesi\n\nDavalı **kiracı** bedeli *ödememiştir*.\n\n\
             - birinci\n- ikinci\n\n\
             | Sıfat | Ad |\n|---|---|\n| Kiracı | Selin |\n",
        );
        let flow = flow(&bytes, "docx").unwrap();
        let text = flow.text();
        assert!(
            text.contains("Kira sözleşmesi") && text.contains("Selin"),
            "{text}"
        );
        let heading = flow.blocks.iter().find_map(|b| match b {
            Block::Para(p) if p.heading > 0 => Some(p),
            _ => None,
        });
        assert!(heading.is_some_and(|h| h.size > 1.0), "{flow:?}");
        let bold = flow.blocks.iter().any(|b| match b {
            Block::Para(p) => p.runs.iter().any(|r| r.bold && r.text.contains("kiracı")),
            _ => false,
        });
        assert!(bold, "{flow:?}");
        let marks: Vec<&str> = flow
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Para(p) if !p.mark.is_empty() => Some(p.mark.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(marks, ["•", "•"], "{flow:?}");
        assert!(
            flow.blocks
                .iter()
                .any(|b| matches!(b, Block::Table(t) if t.rows.len() == 2))
        );
    }

    #[test]
    fn a_book_is_its_sheets_numbers_kept_as_numbers() {
        let mut bytes = Cursor::new(Vec::new());
        office_oxide::create::create_from_markdown_to_writer(
            "| Ad | Tutar |\n|---|---|\n| Ali | 1500 |\n| Veli | 2,5 |\n",
            DocumentFormat::Xlsx,
            &mut bytes,
        )
        .unwrap();
        let flow = flow(&bytes.into_inner(), "xlsx").unwrap();
        let sheet = flow.sheets().next().expect("a sheet");
        assert_eq!(sheet.widths.len(), 2, "{sheet:?}");
        let shown: Vec<Vec<String>> = sheet
            .rows
            .iter()
            .map(|r| {
                r.cells
                    .iter()
                    .map(|c| c.shown(&scour_flow::Locale::of("tr")))
                    .collect()
            })
            .collect();
        assert_eq!(shown[0], ["Ad", "Tutar"], "{sheet:?}");
        assert_eq!(shown[1][0], "Ali");
        assert!(sheet.widths.iter().all(|w| *w >= 2.5));
    }

    #[test]
    fn a_presentation_is_its_slides_each_a_card_in_reading_order() {
        let mut bytes = Cursor::new(Vec::new());
        office_oxide::create::create_from_markdown_to_writer(
            "# Klonlama\n\nBirinci slaytın sözü.\n\n---\n\n# İkinci\n\n- madde bir\n- madde iki\n",
            DocumentFormat::Pptx,
            &mut bytes,
        )
        .unwrap();
        let flow = flow(&bytes.into_inner(), "pptx").unwrap();
        let slides: Vec<&scour_flow::Slide> = flow
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Slide(s) => Some(s),
                _ => None,
            })
            .collect();
        assert!(!slides.is_empty(), "{flow:?}");
        let text = flow.text();
        assert!(
            text.contains("Klonlama") && text.contains("madde iki"),
            "{text}"
        );
        // The title first on its card.
        let first = match &slides[0].blocks[0] {
            Block::Para(p) => p.text(),
            other => format!("{other:?}"),
        };
        assert!(first.contains("Klonlama"), "{flow:?}");
    }

    #[test]
    fn what_is_not_a_word_processor_s_or_not_a_document_is_refused() {
        assert!(flow(b"PK\x03\x04", "odp").is_err());
        assert!(flow(b"not a document", "docx").is_err());
    }
}
