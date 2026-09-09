//! The two text chunks that make a thumbnail a thumbnail. No pixel is read here.
//! `Thumb::URI` and `Thumb::MTime` are required and a thumbnail without them is
//! invalid — every desktop regenerates. Thumbnailers do not write them: by the
//! standard's division of labour that is the managing application's job.

/// Put text chunks into a PNG, replacing any with the same keyword. `None` for
/// anything that is not a PNG — which is also how a thumbnailer that exited zero
/// having written nothing is caught.
pub fn with_text(png: &[u8], pairs: &[(&str, String)]) -> Option<Vec<u8>> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    if png.len() < 8 || png[..8] != SIGNATURE {
        return None;
    }
    let mut out = Vec::with_capacity(png.len() + 128);
    out.extend_from_slice(&SIGNATURE);

    let replacing: Vec<&str> = pairs.iter().map(|(k, _)| *k).collect();
    let mut wrote_ours = false;
    let mut saw_header = false;

    for (kind, data) in chunks(&png[8..])? {
        // The header stays first and ours go straight after, where a reader
        // that gives up early still finds them.
        if kind == *b"IHDR" {
            saw_header = true;
            out.extend_from_slice(&chunk(&kind, data));
            for (key, value) in pairs {
                out.extend_from_slice(&text_chunk(key, value));
            }
            wrote_ours = true;
            continue;
        }
        // Somebody else's answer to the same question, dropped.
        if kind == *b"tEXt"
            && let Some((key, _)) = split_text(data)
            && replacing.contains(&key)
        {
            continue;
        }
        out.extend_from_slice(&chunk(&kind, data));
    }
    (saw_header && wrote_ours).then_some(out)
}

/// The `Thumb::MTime` a PNG records, if it records one — how a failure note says
/// which version of a file failed.
pub fn stamp_of(path: &std::path::Path) -> Option<i64> {
    // A ceiling: this reads a directory anything may write into, and a
    // thumbnail is kilobytes.
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > 4 * 1024 * 1024 {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() < 8 {
        return None;
    }
    for (kind, data) in chunks(&bytes[8..])? {
        if kind == *b"tEXt"
            && let Some(("Thumb::MTime", value)) = split_text(data)
        {
            return value.trim().parse().ok();
        }
    }
    None
}

/// The smallest valid PNG, which is what a failure note is made of: the standard
/// wants a real image, and one transparent pixel costs 69 bytes.
pub fn one_transparent_pixel() -> Vec<u8> {
    // 1×1, 8 bits, colour type 6 (RGBA), no compression/filter/interlace.
    let mut header = Vec::new();
    header.extend_from_slice(&1u32.to_be_bytes());
    header.extend_from_slice(&1u32.to_be_bytes());
    header.extend_from_slice(&[8, 6, 0, 0, 0]);

    // A zlib stream of one stored deflate block: the scanline's filter byte and
    // four zero channels. Hand-written rather than a deflate dependency.
    let raw = [0u8; 5];
    let mut data = vec![0x78, 0x01]; // zlib header: deflate, 32 KiB window
    data.push(0x01); // final block, stored
    data.extend_from_slice(&(raw.len() as u16).to_le_bytes());
    data.extend_from_slice(&(!(raw.len() as u16)).to_le_bytes());
    data.extend_from_slice(&raw);
    data.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    out.extend_from_slice(&chunk(b"IHDR", &header));
    out.extend_from_slice(&chunk(b"IDAT", &data));
    out.extend_from_slice(&chunk(b"IEND", &[]));
    out
}

/// Every chunk in order, or `None` if the run does not add up. Strict: a
/// truncated file stamped with our metadata is a broken picture readers trust.
fn chunks(mut rest: &[u8]) -> Option<Vec<([u8; 4], &[u8])>> {
    let mut out = Vec::new();
    while !rest.is_empty() {
        if rest.len() < 12 {
            return None;
        }
        let len = u32::from_be_bytes(rest[0..4].try_into().ok()?) as usize;
        let end = 8usize.checked_add(len)?.checked_add(4)?;
        if end > rest.len() {
            return None;
        }
        let kind: [u8; 4] = rest[4..8].try_into().ok()?;
        out.push((kind, &rest[8..8 + len]));
        rest = &rest[end..];
    }
    if out.is_empty() { None } else { Some(out) }
}

fn chunk(kind: &[u8; 4], data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 12);
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = Vec::with_capacity(data.len() + 4);
    crc.extend_from_slice(kind);
    crc.extend_from_slice(data);
    out.extend_from_slice(&crc32(&crc).to_be_bytes());
    out
}

/// A `tEXt` chunk: a Latin-1 keyword, a NUL, and Latin-1 text. Every value given
/// is already ASCII, and a byte that is not is dropped rather than mangled.
fn text_chunk(key: &str, value: &str) -> Vec<u8> {
    let mut data = Vec::with_capacity(key.len() + value.len() + 1);
    data.extend(key.bytes().filter(|b| *b != 0));
    data.push(0);
    data.extend(value.bytes().filter(|b| *b != 0));
    chunk(b"tEXt", &data)
}

fn split_text(data: &[u8]) -> Option<(&str, &str)> {
    let at = data.iter().position(|b| *b == 0)?;
    let key = std::str::from_utf8(&data[..at]).ok()?;
    let value = std::str::from_utf8(&data[at + 1..]).ok()?;
    Some((key, value))
}

fn crc32(bytes: &[u8]) -> u32 {
    static TABLE: std::sync::LazyLock<[u32; 256]> = std::sync::LazyLock::new(|| {
        std::array::from_fn(|n| {
            (0..8).fold(n as u32, |c, _| {
                if c & 1 != 0 {
                    0xedb8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                }
            })
        })
    });
    let table = &*TABLE;
    !bytes.iter().fold(!0u32, |c, b| {
        table[((c ^ *b as u32) & 0xff) as usize] ^ (c >> 8)
    })
}

fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for byte in bytes {
        a = (a + *byte as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The checksums are the part that cannot be eyeballed: a chunk with a wrong
    /// CRC is refused by every decoder.
    #[test]
    fn the_smallest_png_is_a_png() {
        let png = one_transparent_pixel();
        assert_eq!(&png[..8], &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]);
        let parsed = chunks(&png[8..]).expect("it parses as chunks");
        let kinds: Vec<String> = parsed
            .iter()
            .map(|(k, _)| String::from_utf8_lossy(k).into_owned())
            .collect();
        assert_eq!(kinds, ["IHDR", "IDAT", "IEND"]);
    }

    /// The published check values, so a broken table is caught here.
    #[test]
    fn crc32_is_crc32() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(adler32(b"123456789"), 0x091e_01de);
    }

    #[test]
    fn text_goes_in_and_comes_back_out() {
        let png = one_transparent_pixel();
        let with = with_text(
            &png,
            &[
                ("Thumb::URI", "file:///a/b.png".into()),
                ("Thumb::MTime", "1699999999".into()),
            ],
        )
        .expect("a PNG takes text");
        let parsed = chunks(&with[8..]).unwrap();
        // Straight after the header, where a reader looks first.
        assert_eq!(&parsed[0].0, b"IHDR");
        assert_eq!(
            split_text(parsed[1].1),
            Some(("Thumb::URI", "file:///a/b.png"))
        );
        assert_eq!(
            split_text(parsed[2].1),
            Some(("Thumb::MTime", "1699999999"))
        );
    }

    /// A reader takes the first it meets, so a stale `Thumb::MTime` in front of a
    /// fresh one is a thumbnail that is invalid forever.
    #[test]
    fn writing_again_replaces_rather_than_repeats() {
        let png = one_transparent_pixel();
        let once = with_text(&png, &[("Thumb::MTime", "1".into())]).unwrap();
        let twice = with_text(&once, &[("Thumb::MTime", "2".into())]).unwrap();
        let stamps: Vec<&str> = chunks(&twice[8..])
            .unwrap()
            .iter()
            .filter(|(k, _)| k == b"tEXt")
            .filter_map(|(_, d)| split_text(d))
            .filter(|(k, _)| *k == "Thumb::MTime")
            .map(|(_, v)| v)
            .collect();
        assert_eq!(stamps, ["2"]);
    }

    #[test]
    fn what_is_not_a_png_is_refused() {
        assert!(with_text(b"", &[]).is_none());
        assert!(with_text(b"not a png at all", &[]).is_none());
        // A signature and nothing behind it, as a thumbnailer that wrote nothing
        // leaves.
        assert!(with_text(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a], &[]).is_none());
        // Truncated in the middle of a chunk.
        let png = one_transparent_pixel();
        assert!(with_text(&png[..png.len() - 6], &[]).is_none());
    }

    #[test]
    fn a_stamp_is_read_back_from_a_file() {
        let dir = tempfile::tempdir().unwrap();
        let at = dir.path().join("t.png");
        let png = with_text(
            &one_transparent_pixel(),
            &[("Thumb::MTime", "1700000042".into())],
        )
        .unwrap();
        std::fs::write(&at, &png).unwrap();
        assert_eq!(stamp_of(&at), Some(1_700_000_042));
        // Nothing is claimed about a file that has none.
        std::fs::write(&at, one_transparent_pixel()).unwrap();
        assert_eq!(stamp_of(&at), None);
        assert_eq!(stamp_of(&dir.path().join("absent.png")), None);
    }
}
