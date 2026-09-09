//! Where the desktop keeps its pictures, and what a name in there means: the
//! directory, the file name, the four sizes, the place a failure is recorded.
//! One copy for every face, because a thumbnail's name is the MD5 of the file's
//! URI: a reader and a writer that disagree by one escaped byte never meet, and
//! the symptom is a cache that fills while every picture stays blank.

use std::path::{Path, PathBuf};

/// The sizes the standard names, in the order a reader should prefer. Everything
/// drawn from these is downscaled — 18 pixels in a row, about 100 in a tile — so
/// the sharper source wins and none is large enough to be worth avoiding.
pub const SIZES: [&str; 4] = ["x-large", "large", "normal", "xx-large"];

/// The size Scour asks a thumbnailer for, in pixels: one, not the standard's
/// four, since everything drawn is downsampled anyway. `large` is the one
/// `gnome-desktop`'s factory asks for, so Files finds it without regenerating.
pub const ASKED: u32 = 256;

/// The name this program records its failures under. The standard gives every
/// application its own, so the weakest cannot veto the rest.
pub const FAILED_BY: &str = "scour";

/// Where thumbnails live. Read from the environment once: it cannot change while
/// the process runs, and this is asked once per row.
pub fn dir() -> &'static Path {
    static DIR: std::sync::LazyLock<PathBuf> = std::sync::LazyLock::new(|| {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cache"))
            .join("thumbnails")
    });
    &DIR
}

/// The thumbnail somebody has already made for this file, if there is one. Four
/// `stat` calls when the answer is no, so a caller rules out what it can first.
pub fn existing(path: &str) -> Option<PathBuf> {
    if path.is_empty() {
        return None;
    }
    let name = name_of(path);
    SIZES
        .iter()
        .map(|size| dir().join(size).join(&name))
        .find(|p| p.is_file())
}

/// Where a thumbnail of this file goes, at the size Scour asks for.
pub fn destination(path: &str) -> PathBuf {
    dir().join("large").join(name_of(path))
}

/// Where the note saying "this one cannot be done" goes.
pub fn failure(path: &str) -> PathBuf {
    dir().join("fail").join(FAILED_BY).join(name_of(path))
}

/// Has this file already been tried and failed? Without the record, a scroll past
/// ten thousand undrawable files is ten thousand doomed processes every time.
/// `mtime` is compared, and an unreadable stamp counts as no failure.
pub fn has_failed(path: &str, mtime: i64) -> bool {
    let p = failure(path);
    match crate::png::stamp_of(&p) {
        Some(when) => when == mtime,
        None => false,
    }
}

/// `<md5 of the uri>.png`, which is what the standard calls a thumbnail.
pub fn name_of(path: &str) -> String {
    format!("{}.png", md5_hex(uri_of(path).as_bytes()))
}

/// `file://` and the path, escaped the way GLib escapes it.
pub fn uri_of(path: &str) -> String {
    const SAFE: &[u8] = b"/-_.~!$&'()*+,;=:@";
    let mut out = String::from("file://");
    for &b in path.as_bytes() {
        if b.is_ascii_alphanumeric() || SAFE.contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// MD5, because the thumbnail specification names files with it. Written out
/// rather than depended on; it is a file name, never a security property.
pub fn md5_hex(input: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    /// The round constants, worked out once for the life of the process: built
    /// per call they cost 64 `sin()` and an allocation, once per row of an answer.
    static K: std::sync::LazyLock<[u32; 64]> = std::sync::LazyLock::new(|| {
        std::array::from_fn(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32)
    });
    let k = &*K;

    let mut msg = input.to_vec();
    let bits = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_le_bytes());

    let (mut a0, mut b0, mut c0, mut d0) = (
        0x6745_2301u32,
        0xefcd_ab89u32,
        0x98ba_dcfeu32,
        0x1032_5476u32,
    );
    for chunk in msg.chunks(64) {
        let m: Vec<u32> = chunk
            .chunks(4)
            .map(|w| u32::from_le_bytes([w[0], w[1], w[2], w[3]]))
            .collect();
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }
    [a0, b0, c0, d0]
        .iter()
        .flat_map(|w| w.to_le_bytes())
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_is_md5() {
        // The published vectors: a drift here makes every lookup miss.
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(b"The quick brown fox jumps over the lazy dog"),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
    }

    #[test]
    fn a_uri_escapes_what_glib_escapes() {
        assert_eq!(uri_of("/home/u/a b.png"), "file:///home/u/a%20b.png");
        // Every byte is escaped: the hash is over bytes, not characters.
        assert_eq!(
            uri_of("/home/u/Çalışma.png"),
            "file:///home/u/%C3%87al%C4%B1%C5%9Fma.png"
        );
        assert_eq!(uri_of("/a/b~c!d"), "file:///a/b~c!d");
    }

    /// The name the rest of the desktop looks for: a disagreement here is a
    /// disagreement with every other program.
    #[test]
    fn a_name_is_the_hash_of_the_uri() {
        assert_eq!(
            name_of("/home/u/a.png"),
            format!("{}.png", md5_hex(b"file:///home/u/a.png"))
        );
    }
}
