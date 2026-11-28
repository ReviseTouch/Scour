//! A font file (TrueType, OpenType, a collection's first face) as a flow:
//! its family and style, what it holds, and its letters drawn in it at a few
//! sizes — Turkish ones among them — as a picture both faces show. Drawn
//! with ab_glyph and written as a PNG; the font is never installed.

use ab_glyph::{Font, FontRef, PxScale, ScaleFont};
use scour_flow::Flow;

use crate::build::Builder;

/// The picture's width, and its lines: the size each is drawn at and what it says.
const WIDTH: u32 = 960;
const LINES: &[(f32, &str)] = &[
    (44.0, "Aa Bb Çç Ğğ İı Öö Şş Üü"),
    (28.0, "Pijamalı hasta yağız şoföre çabucak güvendi."),
    (22.0, "The quick brown fox jumps over the lazy dog."),
    (18.0, "0123456789  ! ? & @ % ( ) [ ] { } / \\ * + − = € ₺"),
    (
        13.0,
        "ABCÇDEFGĞHIİJKLMNOÖPRSŞTUÜVYZ abcçdefgğhıijklmnoöprsştuüvyz",
    ),
];
const INK: u8 = 0x1f;

/// The font in `bytes`.
pub fn flow(bytes: &[u8]) -> Result<Flow, String> {
    let face = ttf_parser::Face::parse(bytes, 0).map_err(|e| e.to_string())?;
    let name = |id: u16| {
        face.names()
            .into_iter()
            .filter(|n| n.name_id == id)
            .find_map(|n| n.to_string())
    };
    let family = name(16).or_else(|| name(1)).unwrap_or_default();
    let style = name(17).or_else(|| name(2)).unwrap_or_default();
    let mut b = Builder::default();
    b.heading(2);
    b.words(
        if family.is_empty() {
            "(no name)"
        } else {
            family.as_str()
        },
        false,
    );
    b.end_heading();
    let mut said = vec![style];
    said.push(format!("{} glyphs", face.number_of_glyphs()));
    if face.is_variable() {
        said.push("variable".into());
    }
    if face.is_monospaced() {
        said.push("monospaced".into());
    }
    if let Some(v) = name(5) {
        said.push(v.trim().to_owned());
    }
    b.caption(
        &said
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · "),
    );
    let font = FontRef::try_from_slice_and_index(bytes, 0).map_err(|e| e.to_string())?;
    let png = sample(&font)?;
    b.picture(
        &png,
        Some((WIDTH as f32 / 13.0, sample_height() as f32 / 13.0)),
    );
    if let Some(c) = name(0) {
        b.caption(c.trim());
    }
    Ok(b.finish())
}

fn sample_height() -> u32 {
    let lines: f32 = LINES.iter().map(|(px, _)| px * 1.45).sum();
    lines as u32 + 24
}

/// The lines drawn in `font`, dark on white, as a PNG.
fn sample(font: &FontRef<'_>) -> Result<Vec<u8>, String> {
    let (w, h) = (WIDTH, sample_height());
    let mut grey = vec![0xffu8; (w * h) as usize];
    let mut baseline = 12.0f32;
    for &(px, text) in LINES {
        let scaled = font.as_scaled(PxScale::from(px));
        baseline += scaled.ascent();
        let mut x = 12.0f32;
        let mut last = None;
        for c in text.chars() {
            let id = font.glyph_id(c);
            if let Some(prev) = last {
                x += scaled.kern(prev, id);
            }
            let glyph = id.with_scale_and_position(px, ab_glyph::point(x, baseline));
            x += scaled.h_advance(id);
            last = Some(id);
            if x > w as f32 {
                break;
            }
            let Some(outline) = font.outline_glyph(glyph) else {
                continue;
            };
            let bounds = outline.px_bounds();
            outline.draw(|gx, gy, cover| {
                let px = bounds.min.x as i32 + gx as i32;
                let py = bounds.min.y as i32 + gy as i32;
                if px < 0 || py < 0 || px >= w as i32 || py >= h as i32 {
                    return;
                }
                let at = (py as u32 * w + px as u32) as usize;
                let ink = 255.0 - cover.clamp(0.0, 1.0) * (255.0 - f32::from(INK));
                grey[at] = grey[at].min(ink as u8);
            });
        }
        baseline += -scaled.descent() + scaled.line_gap() + px * 0.25;
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Grayscale);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|e| e.to_string())?;
        writer.write_image_data(&grey).map_err(|e| e.to_string())?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use scour_flow::Block;

    /// A font on this machine, if there is one where fonts usually are.
    fn some_font() -> Option<Vec<u8>> {
        for dir in ["/usr/share/fonts", "/usr/local/share/fonts"] {
            let mut stack = vec![std::path::PathBuf::from(dir)];
            while let Some(d) = stack.pop() {
                for e in std::fs::read_dir(&d).ok()?.flatten() {
                    let p = e.path();
                    if p.is_dir() {
                        stack.push(p);
                    } else if p.extension().is_some_and(|x| x == "ttf") {
                        return std::fs::read(p).ok();
                    }
                }
            }
        }
        None
    }

    #[test]
    fn a_font_is_its_name_and_its_letters() {
        let Some(bytes) = some_font() else { return };
        let flow = flow(&bytes).unwrap();
        assert!(
            matches!(&flow.blocks[0], Block::Para(p) if p.heading == 2 && !p.text().is_empty())
        );
        let picture = flow
            .blocks
            .iter()
            .find_map(|b| match b {
                Block::Picture(p) => Some(p),
                _ => None,
            })
            .expect("its letters drawn");
        assert_eq!(picture.mime, "image/png");
        // Ink on the page: some of the picture is not white.
        let png = scour_flow::decode(&picture.data).unwrap();
        let mut dec = png::Decoder::new(std::io::Cursor::new(png))
            .read_info()
            .unwrap();
        let mut buf = vec![0; dec.output_buffer_size().unwrap()];
        dec.next_frame(&mut buf).unwrap();
        assert!(buf.iter().filter(|&&g| g < 128).count() > 1000);
    }

    #[test]
    fn what_is_not_a_font_is_refused() {
        assert!(flow(b"not a font").is_err());
    }
}
