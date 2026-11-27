//! A word processor's document drawn as it flows (`scour-flow`): its blocks as
//! the rows of the preview's [`FlowRow`] list, each paragraph Slint's styled
//! text. Pictures are decoded off the UI thread and set as they come. A
//! spreadsheet's book is its sheets instead, one grid at a time. The words a
//! search found in the file are marked, and the view opens on the first.

use slint::private_unstable_api::re_exports::StyledText;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

use i_slint_common::styled_text::{
    FormattedSpan, MARKDOWN_INTERPOLATION_PLACEHOLDER, Style, StyledTextParagraph,
};
use scour_flow::{Align, Block, Flow, Locale, Marks, Para, Sheet};

use crate::ui::{FlowCell, FlowRow, GridCellView, GridRowView, MainWindow};

thread_local! {
    /// The book on show, and how its numbers are punctuated: a tab picks a
    /// sheet of it again.
    static BOOK: RefCell<Option<(Vec<Sheet>, Locale)>> = const { RefCell::new(None) };
    /// The words a search found in the file on show.
    static MARKS: RefCell<Marks> = RefCell::new(Marks::default());
}

/// A find's ink: no fill behind a word in styled text, so a colour that reads
/// on white paper and on a dark slide, bold and underlined.
const FOUND_INK: u32 = 0xffd9_7706;
/// A sheet's cell holding a find: the fill a highlighter leaves.
const FOUND_FILL: [u8; 3] = [0xff, 0xf0, 0xa6];

/// The finds' own words, from the lines the preview lists under the file.
fn found_words(w: &MainWindow) -> Marks {
    let found = w.get_peek_found();
    let words: Vec<String> = found
        .iter()
        .flat_map(|f| [f.hit, f.hit2, f.hit3])
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    Marks::new(&words)
}

/// Open on row `row` once the list has laid itself out, if the file is still
/// the one on show.
fn go_to(w: &MainWindow, path: &str, row: Option<usize>) {
    w.set_peek_goto(-1);
    let Some(row) = row else { return };
    let (weak, path) = (w.as_weak(), path.to_owned());
    slint::Timer::single_shot(std::time::Duration::from_millis(60), move || {
        if let Some(w) = weak.upgrade()
            && w.get_peek_path().as_str() == path
        {
            w.set_peek_goto(row as i32);
        }
    });
}

/// Whether a block holds one of the finds.
fn has_find(block: &Block, m: &Marks) -> bool {
    match block {
        Block::Para(p) => !m.find(&p.text()).is_empty(),
        Block::Table(t) => t.rows.iter().any(|r| {
            r.cells
                .iter()
                .any(|c| c.paras.iter().any(|p| !m.find(&p.text()).is_empty()))
        }),
        _ => false,
    }
}

/// Draw `flow` for `path`, which the preview is showing, in `language`.
pub fn show(w: &MainWindow, path: &str, flow: Flow, language: &str) {
    let marks = found_words(w);
    MARKS.with(|m| *m.borrow_mut() = marks.clone());
    let sheets: Vec<Sheet> = flow.sheets().cloned().collect();
    if !sheets.is_empty() {
        let names: Vec<slint::SharedString> =
            sheets.iter().map(|s| s.name.as_str().into()).collect();
        w.set_peek_sheets(ModelRc::new(VecModel::from(names)));
        BOOK.with(|b| *b.borrow_mut() = Some((sheets, Locale::of(language))));
        pick(w, 0);
        w.set_peek_book(true);
        w.set_peek_flowing(true);
        return;
    }
    w.set_peek_book(false);
    // Each picture this window decodes, and the row it lands in.
    let mut pictures = Vec::new();
    let mut rows: Vec<FlowRow> = Vec::with_capacity(flow.blocks.len());
    let mut slides = 0;
    let mut first: Option<usize> = None;
    let deck = flow.blocks.iter().any(|b| matches!(b, Block::Slide(_)));
    for block in &flow.blocks {
        if first.is_none() {
            let inner = match block {
                Block::Slide(s) => s.blocks.iter().any(|b| has_find(b, &marks)),
                b => has_find(b, &marks),
            };
            if inner {
                first = Some(rows.len());
            }
        }
        match block {
            Block::Slide(slide) => {
                slides += 1;
                let card = Card {
                    fill: color(slide.background.unwrap_or([0xff, 0xff, 0xff])),
                    ink: if slide.dark() {
                        slint::Color::from_rgb_u8(0xf6, 0xf8, 0xfa)
                    } else {
                        slint::Color::from_rgb_u8(0x1f, 0x23, 0x28)
                    },
                };
                rows.push(card.row(FlowRow {
                    kind: 4,
                    mark: slides.to_string().into(),
                    ..blank_row()
                }));
                for inner in &slide.blocks {
                    note_picture(inner, rows.len(), &mut pictures);
                    rows.extend(rows_of(inner).into_iter().map(|r| card.row(r)));
                }
                rows.push(card.row(FlowRow {
                    kind: 5,
                    ..blank_row()
                }));
            }
            _ => {
                note_picture(block, rows.len(), &mut pictures);
                rows.extend(rows_of(block));
            }
        }
    }
    let model = Rc::new(VecModel::from(rows));
    w.set_peek_flow(ModelRc::from(Rc::clone(&model)));
    w.set_peek_deck(deck);
    w.set_peek_flowing(true);
    go_to(w, path, first);
    if pictures.is_empty() {
        return;
    }
    // Decoded on a thread of their own, the bytes moved there: a page of
    // photographs is tens of milliseconds the panel would otherwise stand still.
    let weak = w.as_weak();
    let path = path.to_owned();
    let _ = std::thread::Builder::new()
        .name("scour-flow-pictures".into())
        .spawn(move || {
            for (row, data) in pictures {
                let Some(buf) = decode(&data) else { continue };
                let (weak, path) = (weak.clone(), path.clone());
                let _ = slint::invoke_from_event_loop(move || {
                    let Some(w) = weak.upgrade() else { return };
                    if w.get_peek_path().as_str() != path || !w.get_peek_flowing() {
                        return;
                    }
                    let model = w.get_peek_flow();
                    if let Some(mut r) = model.row_data(row) {
                        r.picture = slint::Image::from_rgba8(buf);
                        model.set_row_data(row, r);
                    }
                });
            }
        });
}

/// A picture this window decodes, to land in row `at`.
fn note_picture(block: &Block, at: usize, pictures: &mut Vec<(usize, String)>) {
    if let Block::Picture(p) = block
        && matches!(p.mime.as_str(), "image/png" | "image/jpeg")
    {
        pictures.push((at, p.data.clone()));
    }
}

/// A slide's colour and the ink for words with none of their own.
struct Card {
    fill: slint::Color,
    ink: slint::Color,
}

impl Card {
    fn row(&self, r: FlowRow) -> FlowRow {
        FlowRow {
            carded: true,
            card: self.fill,
            ink: self.ink,
            ..r
        }
    }
}

/// Back to nothing: another file, or none.
pub fn clear(w: &MainWindow) {
    if w.get_peek_flowing() {
        w.set_peek_flowing(false);
    }
    w.set_peek_goto(-1);
    if w.get_peek_flow().row_count() > 0 {
        w.set_peek_flow(ModelRc::default());
    }
    if w.get_peek_book() {
        w.set_peek_book(false);
        w.set_peek_sheets(ModelRc::default());
        w.set_peek_grid(ModelRc::default());
        w.set_peek_grid_head(ModelRc::default());
        BOOK.with(|b| *b.borrow_mut() = None);
    }
}

/// A grid's text is 12 px: an em of the model's widths.
const GRID_EM: f32 = 12.0;

/// Show the book's `index`th sheet.
pub fn pick(w: &MainWindow, index: usize) {
    BOOK.with(|b| {
        let b = b.borrow();
        let Some((sheets, locale)) = b.as_ref() else {
            return;
        };
        let Some(sheet) = sheets.get(index) else {
            return;
        };
        let px: Vec<f32> = sheet.widths.iter().map(|w| w * GRID_EM).collect();
        let marks = MARKS.with(|m| m.borrow().clone());
        let mut first = None;
        let mut rows: Vec<GridRowView> = Vec::with_capacity(sheet.rows.len());
        for (n, r) in sheet.rows.iter().enumerate() {
            let (cells, found) = cells(r, &px, locale, &marks);
            if found && first.is_none() {
                first = Some(n);
            }
            rows.push(GridRowView {
                cells: ModelRc::new(VecModel::from(cells)),
            });
        }
        let head = if sheet.frozen && !rows.is_empty() {
            first = first.and_then(|n| n.checked_sub(1));
            rows.remove(0).cells
        } else {
            ModelRc::default()
        };
        w.set_peek_grid_width(px.iter().sum());
        w.set_peek_grid_head(head);
        w.set_peek_grid(ModelRc::new(VecModel::from(rows)));
        w.set_peek_sheet(index as i32);
        go_to(w, &w.get_peek_path(), first);
    });
}

/// A row's cells across every column, the missing ones blank, so the grid's
/// lines run the whole width.
fn cells(
    r: &scour_flow::GridRow,
    px: &[f32],
    locale: &Locale,
    marks: &Marks,
) -> (Vec<GridCellView>, bool) {
    let mut out = Vec::with_capacity(px.len());
    let mut at = 0usize;
    let mut found = false;
    for (i, c) in r.cells.iter().enumerate() {
        if at >= px.len() {
            break;
        }
        let span = (c.span.max(1) as usize).min(px.len() - at);
        let text = c.shown(locale);
        let width: f32 = px[at..at + span].iter().sum();
        // Words run on over the blank cells after them, to the grid's edge.
        let mut reach = width;
        if c.spills() {
            let mut col = at + span;
            for n in &r.cells[i + 1..] {
                if !n.blank() || col >= px.len() {
                    break;
                }
                let s = n.span.max(1) as usize;
                reach += px[col..(col + s).min(px.len())].iter().sum::<f32>();
                col += s;
            }
            if i + 1 >= r.cells.len() {
                reach += px[col.min(px.len())..].iter().sum::<f32>();
            }
        }
        // A cell holding a find is filled as a highlighter would.
        let hit = !marks.find(&text).is_empty();
        found |= hit;
        out.push(GridCellView {
            // One line, as a cell shows it unless it wraps.
            text: text.lines().next().unwrap_or("").into(),
            width,
            reach,
            align: match c.alignment() {
                Align::Left => 0,
                Align::Center => 1,
                Align::Right => 2,
            },
            bold: c.bold || hit,
            italic: c.italic,
            color: color(if hit {
                [0x1f, 0x23, 0x28]
            } else {
                c.color.unwrap_or_default()
            }),
            colored: c.color.is_some() || hit,
            shade: color(if hit {
                FOUND_FILL
            } else {
                c.shade.unwrap_or_default()
            }),
            shaded: c.shade.is_some() || hit,
        });
        at += span;
    }
    for &w in &px[at.min(px.len())..] {
        out.push(GridCellView {
            text: Default::default(),
            width: w,
            reach: w,
            align: 0,
            bold: false,
            italic: false,
            color: slint::Color::default(),
            colored: false,
            shade: slint::Color::default(),
            shaded: false,
        });
    }
    (out, found)
}

/// A picture's bytes as pixels, if they are a kind this window decodes.
fn decode(data: &str) -> Option<slint::SharedPixelBuffer<slint::Rgba8Pixel>> {
    let bytes = scour_flow::decode(data)?;
    let img = image::load_from_memory(&bytes).ok()?;
    // The panel is narrow: no picture needs more than this across.
    let img = if img.width() > 1200 {
        img.resize(1200, 1200 * 4, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let rgba = img.to_rgba8();
    Some(slint::SharedPixelBuffer::clone_from_slice(
        rgba.as_raw(),
        rgba.width(),
        rgba.height(),
    ))
}

fn rows_of(block: &Block) -> Vec<FlowRow> {
    match block {
        Block::Para(p) => vec![para_row(p)],
        Block::Table(t) => t
            .rows
            .iter()
            .enumerate()
            .map(|(n, r)| {
                let mut at = 0usize;
                let cells: Vec<FlowCell> = r
                    .cells
                    .iter()
                    .map(|c| {
                        let span = c.span.max(1) as usize;
                        let share: f32 = t.widths.iter().skip(at).take(span).sum();
                        at += span;
                        let paras: Vec<&Para> = c.paras.iter().collect();
                        FlowCell {
                            text: styled(&paras, r.head),
                            share: if share > 0.0 {
                                share
                            } else {
                                1.0 / t.widths.len().max(1) as f32
                            },
                            shade: color(c.shade.unwrap_or_default()),
                            shaded: c.shade.is_some(),
                        }
                    })
                    .collect();
                FlowRow {
                    kind: 1,
                    cells: ModelRc::new(VecModel::from(cells)),
                    ruled: t.ruled,
                    last: n + 1 == t.rows.len(),
                    ..blank_row()
                }
            })
            .collect(),
        Block::Picture(p) => vec![FlowRow {
            kind: 2,
            pic_w: p.width,
            pic_h: p.height,
            ..blank_row()
        }],
        Block::Break => vec![FlowRow {
            kind: 3,
            ..blank_row()
        }],
        // A book is drawn as its grids, and a slide by the caller, on its card.
        Block::Sheet(_) | Block::Slide(_) => Vec::new(),
    }
}

fn para_row(p: &Para) -> FlowRow {
    FlowRow {
        kind: 0,
        text: styled(&[p], false),
        size: p.size,
        align: match p.align {
            Align::Left => 0,
            Align::Center => 1,
            Align::Right => 2,
        },
        indent: p.indent,
        before: p.before,
        after: p.after,
        mark: p.mark.as_str().into(),
        shade: color(p.shade.unwrap_or_default()),
        shaded: p.shade.is_some(),
        ..blank_row()
    }
}

fn blank_row() -> FlowRow {
    FlowRow {
        kind: 0,
        text: StyledText::default(),
        size: 1.0,
        align: 0,
        indent: 0.0,
        before: 0.0,
        after: 0.0,
        mark: Default::default(),
        shade: slint::Color::default(),
        shaded: false,
        cells: ModelRc::default(),
        ruled: false,
        last: false,
        picture: slint::Image::default(),
        pic_w: 0.0,
        pic_h: 0.0,
        carded: false,
        card: slint::Color::default(),
        ink: slint::Color::default(),
    }
}

fn color(c: [u8; 3]) -> slint::Color {
    slint::Color::from_rgb_u8(c[0], c[1], c[2])
}

/// Paragraphs as one styled text, a paragraph each. A heading, and a heading
/// row's cells, are bold throughout.
fn styled(paras: &[&Para], bold: bool) -> StyledText {
    let args: Vec<Vec<StyledTextParagraph>> =
        paras.iter().map(|p| vec![paragraph(p, bold)]).collect();
    let placeholder = MARKDOWN_INTERPOLATION_PLACEHOLDER.to_string();
    let format = vec![placeholder; args.len()].join("\n\n");
    StyledText::parse_interpolated(&format, &args).unwrap_or_default()
}

fn paragraph(p: &Para, bold: bool) -> StyledTextParagraph {
    let mut text = String::new();
    let mut formatting = Vec::new();
    for r in &p.runs {
        let start = text.len();
        text.push_str(&r.text);
        let range = start..text.len();
        let mut style = |on: bool, style: Style| {
            if on && !range.is_empty() {
                formatting.push(FormattedSpan {
                    range: range.clone(),
                    style,
                });
            }
        };
        style(bold || r.bold || p.heading > 0, Style::Strong);
        style(r.italic, Style::Emphasis);
        style(r.underline, Style::Underline);
        style(r.strike, Style::Strikethrough);
        if let Some([red, green, blue]) = r.color {
            style(
                true,
                Style::Color(
                    0xff00_0000 | u32::from(red) << 16 | u32::from(green) << 8 | u32::from(blue),
                ),
            );
        }
        // The finds in this run, over whatever it says of itself.
        MARKS.with(|m| {
            for (a, b) in m.borrow().find(&r.text) {
                let range = start + a..start + b;
                for style in [Style::Color(FOUND_INK), Style::Strong, Style::Underline] {
                    formatting.push(FormattedSpan {
                        range: range.clone(),
                        style,
                    });
                }
            }
        });
    }
    StyledTextParagraph {
        text,
        formatting,
        links: Vec::new(),
    }
}
