//! Direct RGBA pixel buffers built off the drawing thread: Slint wraps a
//! [`SharedPixelBuffer`] on the UI thread without copying or decoding.

use std::io::Read;
use std::path::Path;

use image::{DynamicImage, ImageFormat, imageops::FilterType};

pub type RgbaBuf = slint::SharedPixelBuffer<slint::Rgba8Pixel>;

const CACHED_CAP: u64 = 8 * 1024 * 1024;
const DIRECT_CAP: u64 = 48 * 1024 * 1024;

/// Wrap an OS-rendered RGBA [`scour_thumbs::native::Picture`] in a Slint buffer.
pub fn from_picture(p: scour_thumbs::native::Picture) -> Option<RgbaBuf> {
    let expected = (p.width as usize)
        .checked_mul(p.height as usize)?
        .checked_mul(4)?;
    if p.width == 0 || p.height == 0 || p.rgba.len() != expected {
        return None;
    }
    Some(RgbaBuf::clone_from_slice(&p.rgba, p.width, p.height))
}

/// Decode a cached thumbnail PNG off the drawing thread.
pub fn load_cached_png(path: &Path) -> Option<RgbaBuf> {
    let bytes = read_bounded(path, CACHED_CAP)?;
    let img = image::load_from_memory_with_format(&bytes, ImageFormat::Png).ok()?;
    to_rgba_buf(img)
}

/// Decode a local PNG or JPEG directly at preview resolution, honouring JPEG
/// EXIF orientation so phone photographs match the desktop's file manager.
pub fn load_direct_image(path: &Path, max_px: u32) -> Option<RgbaBuf> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let fmt = match ext.as_str() {
        "png" => ImageFormat::Png,
        "jpg" | "jpeg" | "jfif" => ImageFormat::Jpeg,
        _ => return None,
    };
    let bytes = read_bounded(path, DIRECT_CAP)?;
    let orient = if fmt == ImageFormat::Jpeg {
        jpeg_exif_orientation(&bytes)
    } else {
        1
    };
    let img = apply_orientation(
        image::load_from_memory_with_format(&bytes, fmt).ok()?,
        orient,
    );
    let side = max_px.clamp(64, 2048);
    let scaled = if img.width() > side || img.height() > side {
        img.resize(side, side, FilterType::Triangle)
    } else {
        img
    };
    to_rgba_buf(scaled)
}

fn to_rgba_buf(img: DynamicImage) -> Option<RgbaBuf> {
    let rgba = img.into_rgba8();
    let (w, h) = rgba.dimensions();
    (w > 0 && h > 0).then(|| RgbaBuf::clone_from_slice(rgba.as_raw(), w, h))
}

fn read_bounded(path: &Path, cap: u64) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > cap {
        return None;
    }
    let mut out = Vec::with_capacity(meta.len() as usize);
    std::fs::File::open(path)
        .ok()?
        .take(cap)
        .read_to_end(&mut out)
        .ok()?;
    Some(out)
}

fn apply_orientation(img: DynamicImage, orientation: u16) -> DynamicImage {
    match orientation {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

/// Read the TIFF Orientation tag (0x0112) from a JPEG's APP1 Exif segment.
fn jpeg_exif_orientation(bytes: &[u8]) -> u16 {
    if bytes.len() < 4 || bytes[0..2] != [0xff, 0xd8] {
        return 1;
    }
    let mut i = 2usize;
    while i + 4 <= bytes.len() && bytes[i] == 0xff {
        let marker = bytes[i + 1];
        if marker == 0xff {
            i += 1;
            continue;
        }
        if marker == 0xda || marker == 0xd9 {
            break;
        }
        let seg_len = u16::from_be_bytes([bytes[i + 2], bytes[i + 3]]) as usize;
        if seg_len < 2 || i + 2 + seg_len > bytes.len() {
            break;
        }
        let body = &bytes[i + 4..i + 2 + seg_len];
        if marker == 0xe1 && body.starts_with(b"Exif\0\0") {
            return tiff_orientation(&body[6..]).unwrap_or(1);
        }
        i += 2 + seg_len;
    }
    1
}

fn tiff_orientation(tiff: &[u8]) -> Option<u16> {
    if tiff.len() < 8 {
        return None;
    }
    let le = match &tiff[0..2] {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |at: usize| -> Option<u16> {
        let b: [u8; 2] = tiff.get(at..at + 2)?.try_into().ok()?;
        Some(if le {
            u16::from_le_bytes(b)
        } else {
            u16::from_be_bytes(b)
        })
    };
    let u32_at = |at: usize| -> Option<u32> {
        let b: [u8; 4] = tiff.get(at..at + 4)?.try_into().ok()?;
        Some(if le {
            u32::from_le_bytes(b)
        } else {
            u32::from_be_bytes(b)
        })
    };
    if u16_at(2)? != 42 {
        return None;
    }
    let ifd = u32_at(4)? as usize;
    let entries = u16_at(ifd)? as usize;
    for n in 0..entries.min(64) {
        let base = ifd.checked_add(2)?.checked_add(n * 12)?;
        if u16_at(base)? == 0x0112 && u16_at(base + 2)? == 3 {
            let val = u16_at(base + 8)?;
            return (1..=8).contains(&val).then_some(val);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picture_validation_rejects_mismatched_buffers() {
        assert!(
            from_picture(scour_thumbs::native::Picture {
                width: 0,
                height: 2,
                rgba: vec![],
            })
            .is_none()
        );
        assert!(
            from_picture(scour_thumbs::native::Picture {
                width: 2,
                height: 2,
                rgba: vec![0; 12],
            })
            .is_none()
        );
        let ok = from_picture(scour_thumbs::native::Picture {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 255, 0, 255],
        })
        .expect("valid 2x1 RGBA");
        assert_eq!((ok.width(), ok.height()), (2, 1));
    }

    #[test]
    fn exif_orientation_reads_both_endiannesses() {
        // Minimal JPEG with APP1 Exif (big-endian MM, 1 IFD entry: tag 0x0112 = 6).
        let mut jpg = vec![
            0xff, 0xd8, 0xff, 0xe1, 0x00, 0x22, b'E', b'x', b'i', b'f', 0, 0, b'M', b'M', 0x00,
            0x2a, 0x00, 0x00, 0x00, 0x08, 0x00, 0x01, 0x01, 0x12, 0x00, 0x03, 0x00, 0x00, 0x00,
            0x01, 0x00, 0x06, 0x00, 0x00, 0x00, 0x00, 0xff, 0xd9,
        ];
        assert_eq!(jpeg_exif_orientation(&jpg), 6);

        // Little-endian II, tag 0x0112 = 3.
        jpg[12..36].copy_from_slice(&[
            b'I', b'I', 0x2a, 0x00, 0x08, 0x00, 0x00, 0x00, 0x01, 0x00, 0x12, 0x01, 0x03, 0x00,
            0x01, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00,
        ]);
        assert_eq!(jpeg_exif_orientation(&jpg), 3);
    }
}
