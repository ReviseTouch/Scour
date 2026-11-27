//! A word processor's document as a [`scour_flow::Flow`]: office_oxide's model
//! of it, kept to what flows. Here, in the extractor's process, because the
//! library parses what anybody can put on a disk.

use std::io::Cursor;

use office_oxide::ir::{
    DocumentIR, Element, Heading, ImageFormat, InlineContent, List, Paragraph, ParagraphAlignment,
    Table, TextSpan, VerticalAlign,
};
use office_oxide::{Document, DocumentFormat};
use scour_flow::{Align, Block, Cell, Flow, Para, Picture, Row, Run};

/// The document in `bytes`, by its extension.
pub fn flow(bytes: &[u8], extension: &str) -> Result<Flow, String> {
    let format = match extension.to_ascii_lowercase().as_str() {
        "docx" => DocumentFormat::Docx,
        "doc" => DocumentFormat::Doc,
        other => return Err(format!("{other} does not flow")),
    };
    let doc =
        Document::from_reader(Cursor::new(bytes.to_vec()), format).map_err(|e| e.to_string())?;
    Ok(from_ir(&doc.to_ir()).bounded())
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
    fn what_is_not_a_word_processor_s_or_not_a_document_is_refused() {
        assert!(flow(b"PK\x03\x04", "xlsx").is_err());
        assert!(flow(b"not a document", "docx").is_err());
    }
}
