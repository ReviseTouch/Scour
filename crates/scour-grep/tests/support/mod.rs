//! What the tests share: files in a temporary directory, a scanner over
//! them, and a ZIP made by hand.

#![allow(dead_code)]

use std::path::Path;

use scour_grep::{File, Ground, Medium, Needle, Options, Pattern, Scanner};

pub fn put(dir: &Path, name: &str, bytes: &[u8]) -> File {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    File {
        path,
        size: Some(bytes.len() as u64),
        class: None,
    }
}

pub fn scanner(needle: Needle, options: Options, root: &Path) -> Scanner {
    let ground = Ground::of([(root.to_path_buf(), Some("disk".into()), Medium::Solid)]);
    Scanner::new(Pattern::new(&needle, false).unwrap(), options, ground)
}

pub fn text(needle: &str) -> Needle {
    Needle::Text(needle.into())
}

pub fn utf16le(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

pub fn make_zip(entries: &[(&str, u16, &[u8])]) -> Vec<u8> {
    use flate2::{Compress, Compression, FlushCompress};
    let mut out = Vec::new();
    let mut cd = Vec::new();
    for &(name, method, raw) in entries {
        let data = if method == 8 {
            let mut c = Compress::new(Compression::fast(), false);
            let mut dst = Vec::with_capacity(raw.len() + 64);
            c.compress_vec(raw, &mut dst, FlushCompress::Finish)
                .unwrap();
            dst
        } else {
            raw.to_vec()
        };
        let local_off = out.len() as u32;
        let nb = name.as_bytes();
        // Local file header
        out.extend_from_slice(&[0x50, 0x4B, 0x03, 0x04, 20, 0, 0, 0]);
        out.extend_from_slice(&method.to_le_bytes());
        out.extend_from_slice(&[0; 8]); // mtime/mdate/crc
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        out.extend_from_slice(&(nb.len() as u16).to_le_bytes());
        out.extend_from_slice(&0u16.to_le_bytes());
        out.extend_from_slice(nb);
        out.extend_from_slice(&data);

        // Central directory header
        cd.extend_from_slice(&[0x50, 0x4B, 0x01, 0x02, 20, 0, 20, 0, 0, 0]);
        cd.extend_from_slice(&method.to_le_bytes());
        cd.extend_from_slice(&[0; 8]);
        cd.extend_from_slice(&(data.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(raw.len() as u32).to_le_bytes());
        cd.extend_from_slice(&(nb.len() as u16).to_le_bytes());
        cd.extend_from_slice(&[0; 12]);
        cd.extend_from_slice(&local_off.to_le_bytes());
        cd.extend_from_slice(nb);
    }
    let cd_off = out.len() as u32;
    let cd_len = cd.len() as u32;
    out.extend_from_slice(&cd);
    out.extend_from_slice(&[0x50, 0x4B, 0x05, 0x06, 0, 0, 0, 0]);
    let cnt = entries.len() as u16;
    out.extend_from_slice(&cnt.to_le_bytes());
    out.extend_from_slice(&cnt.to_le_bytes());
    out.extend_from_slice(&cd_len.to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}
