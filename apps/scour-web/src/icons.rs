//! The picture a file already has.
//!
//! **The thumbnail, and nothing else.** The file manager writes one into
//! `~/.cache/thumbnails` under the MD5 of the file's URI, and every desktop
//! program reads them from there. Nothing is generated here — this only looks.
//!
//! ## What used to be here
//!
//! Type icons, resolved out of the desktop's icon theme: a kind and an
//! extension became a list of names — `application-pdf`, `text-x-rust`,
//! `image-x-generic` — and the first one any installed theme could draw was
//! served. It worked, and what it drew was the problem. Adwaita's mimetype
//! icons are a blue rhombus for an executable and a near-blank page for a
//! document; at eighteen pixels on a dark list they read as coloured lint. And
//! it was a Linux answer to a question every platform asks: the search was
//! over XDG icon directories, so on Windows and macOS every row got a 404.
//!
//! The page draws its own now, as CSS masks — see the glyphs in `page.html`.
//! Fourteen shapes, one per kind, no request and no theme. This module kept
//! the half that cannot be drawn from a token, because it is the file itself.
//!
//! **Nothing outside the thumbnail cache is ever served.** An icon route that
//! took a path and read it would be the file-reading hole the token and the
//! origin check exist to prevent; the only paths that leave this module are
//! ones it built itself, from a hash.

use std::path::{Path, PathBuf};

/// What a browser is handed, and what it may keep.
pub struct Picture {
    pub bytes: Vec<u8>,
    pub kind: &'static str,
}

/// The thumbnail somebody has already made for this file.
pub fn thumbnail(path: &str) -> Option<Picture> {
    read(&thumbnail_path(path)?)
}

/// Is there one, without reading it?
///
/// Asked once per row of a page, so that the page requests only the pictures
/// that exist rather than two hundred that mostly do not.
///
/// **`kind` is asked first because the answer is four `stat` calls.** A
/// thumbnail is looked for in four size directories and a row that has none —
/// which is nearly every row — pays for all four. On a machine where half the
/// files are source and build output, most of those questions have a known
/// answer: nothing thumbnails a `.rs` file, a directory or an ELF binary. The
/// unknown kind is still asked, because a picture with an unhelpful name is
/// exactly the case where the desktop knows better than the extension does.
pub fn has_thumbnail(path: &str, kind: scour_core::Kind) -> bool {
    use scour_core::Kind::*;
    if matches!(kind, Dir | Code | Build | Exec | Archive) {
        return false;
    }
    thumbnail_path(path).is_some()
}

fn thumbnail_path(path: &str) -> Option<PathBuf> {
    if path.is_empty() {
        return None;
    }
    let name = format!("{}.png", md5_hex(file_uri(path).as_bytes()));
    let base = cache_dir().join("thumbnails");
    // Biggest first: this is drawn at 18 pixels and every one of them is
    // downscaled, so the sharper source wins and none of them is large.
    for size in ["x-large", "large", "normal", "xx-large"] {
        let p = base.join(size).join(&name);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

fn read(p: &Path) -> Option<Picture> {
    let kind = match p.extension().and_then(|e| e.to_str()) {
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("xpm") => "image/x-xpixmap",
        _ => return None,
    };
    // A ceiling, because this reads from a directory a package manager fills:
    // an icon is kilobytes and anything that is not is not an icon.
    let meta = std::fs::metadata(p).ok()?;
    if meta.len() > 4 * 1024 * 1024 {
        return None;
    }
    Some(Picture {
        bytes: std::fs::read(p).ok()?,
        kind,
    })
}

/// Where thumbnails live.
///
/// Read from the environment once. It was read per row, which is two
/// environment lookups and two `PathBuf`s to learn something that cannot
/// change while the process runs.
fn cache_dir() -> &'static Path {
    static DIR: std::sync::LazyLock<PathBuf> = std::sync::LazyLock::new(|| {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".cache"))
    });
    &DIR
}

/// `file://` and the path, escaped the way GLib escapes it.
///
/// Verified against this machine's real thumbnail cache rather than read off a
/// specification: 37 of the files under the picture directories hash to names
/// that are in it.
fn file_uri(path: &str) -> String {
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

/// MD5, because the thumbnail specification names files with it.
///
/// Written out rather than depended on: it is four dependencies away in the
/// registry and this is the only place in the program that needs one, in the
/// one role where nobody claims it is a security property — it is a file name
/// somebody else chose.
fn md5_hex(input: &[u8]) -> String {
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5,
        9, 14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10,
        15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    /// The round constants, worked out once for the life of the process.
    ///
    /// They were built here, which meant sixty-four `sin()` and an allocation
    /// **per call** — and this is called once per row of every answer. A page
    /// of two hundred rows spent 12,800 `sin()` deciding two hundred booleans.
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
        // The published vectors. If this drifts, every thumbnail lookup misses
        // and the list quietly loses its pictures.
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5_hex(b"abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(
            md5_hex(b"The quick brown fox jumps over the lazy dog"),
            "9e107d9d372bb6826bd81d3542a419d6"
        );
    }

    #[test]
    fn a_uri_escapes_what_glib_escapes() {
        assert_eq!(file_uri("/home/u/a b.png"), "file:///home/u/a%20b.png");
        // Turkish names are the ordinary case here, and every byte of them is
        // escaped — the hash is over bytes, not characters.
        assert_eq!(
            file_uri("/home/u/Çalışma.png"),
            "file:///home/u/%C3%87al%C4%B1%C5%9Fma.png"
        );
        assert_eq!(file_uri("/a/b~c!d"), "file:///a/b~c!d");
    }

    /// Every kind the engine can name has a drawing in the page.
    ///
    /// **This is the seam the icons moved across.** The glyphs are CSS now, so
    /// nothing in Rust reads them and the compiler cannot notice a kind added
    /// to [`scour_core::Kind`] with no shape to go with it — the row would get
    /// the plain page and nobody would find out from a test. The page is a
    /// string in this binary, so the test is to look in it.
    ///
    /// It checks the rule exists, not that the drawing is any good. That part
    /// is a screenshot and a pair of eyes.
    #[test]
    fn every_kind_the_engine_names_has_a_glyph_in_the_page() {
        for kind in scour_core::Kind::ALL {
            assert!(
                has_rule(crate::PAGE, kind.token()),
                "{} has no `.k-{}` rule in page.html",
                kind.token(),
                kind.token()
            );
        }
        // And the fallback the page reaches for when a token is one this
        // version has never heard of.
        assert!(has_rule(crate::PAGE, "file"));
    }

    /// Is there a `.k-<token>` selector, and not merely those characters?
    ///
    /// **A plain `contains` does not do this**, which the first version of the
    /// test above found out the hard way: renaming `.k-font` to `.k-fontXX`
    /// deletes the rule for fonts and leaves `.k-font` sitting inside the new
    /// name, so the test passed over a page that had lost a glyph. The name
    /// has to end where the selector ends.
    fn has_rule(page: &str, token: &str) -> bool {
        let needle = format!(".k-{token}");
        page.match_indices(&needle).any(|(at, _)| {
            page[at + needle.len()..]
                .chars()
                .next()
                .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '-' && c != '_')
        })
    }

    #[test]
    fn a_longer_name_is_not_the_rule_it_starts_with() {
        assert!(has_rule(".k-font { --k: red; }", "font"));
        assert!(has_rule(".k-font,.k-doc { }", "doc"));
        assert!(!has_rule(".k-fontXX { --k: red; }", "font"));
        assert!(!has_rule(".k-font-old { }", "font"));
        assert!(!has_rule("nothing here", "font"));
    }
}
