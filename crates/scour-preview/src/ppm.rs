//! Binary PPM (`P6`) parser turning `pdftoppm` stdout bytes into straight RGBA.

/// One rendered document page as straight 8-bit RGBA pixels.
#[derive(Debug, Clone)]
pub struct PageImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// Parse a binary `P6` PPM image into straight RGBA pixels.
pub fn parse_p6(bytes: &[u8]) -> Option<PageImage> {
    if !bytes.starts_with(b"P6") {
        return None;
    }
    let mut at = 2usize;
    let width = next_token_u32(bytes, &mut at)?;
    let height = next_token_u32(bytes, &mut at)?;
    let maxval = next_token_u32(bytes, &mut at)?;
    if width == 0 || height == 0 || width > 8192 || height > 8192 || maxval != 255 {
        return None;
    }
    // Single whitespace delimiter after maxval before raw RGB triplets.
    if !bytes.get(at)?.is_ascii_whitespace() {
        return None;
    }
    at += 1;
    let pixels = (width as usize).checked_mul(height as usize)?;
    let rgb_len = pixels.checked_mul(3)?;
    let rgb = bytes.get(at..at + rgb_len)?;
    let mut rgba = Vec::with_capacity(pixels * 4);
    for px in rgb.as_chunks::<3>().0 {
        rgba.extend_from_slice(&[px[0], px[1], px[2], 255]);
    }
    Some(PageImage {
        width,
        height,
        rgba,
    })
}

fn next_token_u32(bytes: &[u8], at: &mut usize) -> Option<u32> {
    while let Some(&b) = bytes.get(*at) {
        if b == b'#' {
            while bytes.get(*at).is_some_and(|&c| c != b'\n') {
                *at += 1;
            }
        } else if b.is_ascii_whitespace() {
            *at += 1;
        } else {
            break;
        }
    }
    let start = *at;
    while bytes.get(*at).is_some_and(u8::is_ascii_digit) {
        *at += 1;
    }
    if *at == start {
        return None;
    }
    std::str::from_utf8(&bytes[start..*at]).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_binary_ppm_with_comments_into_rgba() {
        let mut raw = b"P6\n# pdftoppm comment\n2 1\n255\n".to_vec();
        raw.extend_from_slice(&[10, 20, 30, 200, 210, 220]);
        let img = parse_p6(&raw).expect("valid P6");
        assert_eq!((img.width, img.height), (2, 1));
        assert_eq!(img.rgba, [10, 20, 30, 255, 200, 210, 220, 255]);
    }

    #[test]
    fn rejects_truncated_or_non_p6_streams() {
        assert!(parse_p6(b"P3\n1 1\n255\n0 0 0").is_none());
        assert!(parse_p6(b"P6\n2 2\n255\n\x00\x00\x00").is_none());
    }
}
