//! A word processor's document drawn as it flows (`scour-flow`): its blocks as
//! the rows of the preview's [`FlowRow`] list, each paragraph Slint's styled
//! text. Pictures are decoded off the UI thread and set as they come.

use slint::private_unstable_api::re_exports::StyledText;
use slint::{ComponentHandle, Model, ModelRc, VecModel};
use std::rc::Rc;

use i_slint_common::styled_text::{
    FormattedSpan, MARKDOWN_INTERPOLATION_PLACEHOLDER, Style, StyledTextParagraph,
};
use scour_flow::{Align, Block, Flow, Para};

use crate::ui::{FlowCell, FlowRow, MainWindow};

/// Draw `flow` for `path`, which the preview is showing.
pub fn show(w: &MainWindow, path: &str, flow: Flow) {
    // Each picture this window decodes, and the row it lands in.
    let mut pictures = Vec::new();
    let mut rows: Vec<FlowRow> = Vec::with_capacity(flow.blocks.len());
    for block in &flow.blocks {
        if let Block::Picture(p) = block
            && matches!(p.mime.as_str(), "image/png" | "image/jpeg")
        {
            pictures.push((rows.len(), p.data.clone()));
        }
        rows.extend(rows_of(block));
    }
    let model = Rc::new(VecModel::from(rows));
    w.set_peek_flow(ModelRc::from(Rc::clone(&model)));
    w.set_peek_flowing(true);
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

/// Back to nothing: another file, or none.
pub fn clear(w: &MainWindow) {
    if w.get_peek_flowing() {
        w.set_peek_flowing(false);
    }
    if w.get_peek_flow().row_count() > 0 {
        w.set_peek_flow(ModelRc::default());
    }
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
    }
    StyledTextParagraph {
        text,
        formatting,
        links: Vec::new(),
    }
}
