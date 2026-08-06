//! A picture for a row.
//!
//! Two different things, and only the second is what makes a list stop looking
//! like a spreadsheet:
//!
//! * **The type icon** comes from the desktop's icon theme, which maps a kind
//!   of file to a drawing. Adwaita ships twenty-seven of them and they are all
//!   generic — there is no `application-pdf`, no `text-x-rust` — so what a
//!   theme can actually draw is close to the taxonomy this index already has.
//!   The specific name is tried first anyway, because Papirus and others do
//!   ship hundreds and a user who installed one should see them.
//! * **The thumbnail** is the real picture, and it already exists: the file
//!   manager writes one into `~/.cache/thumbnails` under the MD5 of the file's
//!   URI, and every desktop program reads them from there. Nothing is
//!   generated here — this only looks.
//!
//! **Nothing outside those two places is ever served.** An icon route that took
//! a path and read it would be the file-reading hole the token and the origin
//! check exist to prevent; the only paths that leave this module are ones it
//! built itself, from a theme directory or from a hash.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

/// What a browser is handed, and what it may keep.
pub struct Picture {
    pub bytes: Vec<u8>,
    pub kind: &'static str,
}

/// The icon for a kind of file, from the desktop's own theme.
///
/// Cached by what was asked for rather than by what was found: the answer for
/// `rs` is the same for every Rust file on the screen, and a list of two
/// hundred rows asks for a dozen distinct things.
pub fn for_kind(kind: &str, ext: &str) -> Option<Picture> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<PathBuf>>>> = OnceLock::new();
    let key = format!("{kind}\u{1}{ext}");
    let found = {
        let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
        let mut held = cache.lock().ok()?;
        held.entry(key)
            .or_insert_with(|| names_for(kind, ext).iter().find_map(|n| find(n)))
            .clone()
    };
    read(&found?)
}

/// The thumbnail somebody has already made for this file.
pub fn thumbnail(path: &str) -> Option<Picture> {
    read(&thumbnail_path(path)?)
}

/// Is there one, without reading it?
///
/// Asked once per row of a page, so that the page requests only the pictures
/// that exist rather than two hundred that mostly do not.
pub fn has_thumbnail(path: &str) -> bool {
    thumbnail_path(path).is_some()
}

fn thumbnail_path(path: &str) -> Option<PathBuf> {
    if path.is_empty() {
        return None;
    }
    let name = format!("{}.png", md5_hex(file_uri(path).as_bytes()));
    let base = dirs().0.join("thumbnails");
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

/// `(cache, data)` — where thumbnails live, and where themes do.
fn dirs() -> (PathBuf, Vec<PathBuf>) {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".cache"));
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    (
        cache,
        vec![
            data.join("icons"),
            home.join(".icons"),
            PathBuf::from("/usr/share/icons"),
            PathBuf::from("/usr/local/share/icons"),
        ],
    )
}

/// The themes to look in, most specific first.
///
/// The user's choice comes from the desktop's own setting, asked once. Every
/// theme is required to inherit from `hicolor`, which is where anything that
/// installs an icon without a theme puts it.
fn themes() -> &'static [String] {
    static THEMES: OnceLock<Vec<String>> = OnceLock::new();
    THEMES.get_or_init(|| {
        let chosen = std::process::Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "icon-theme"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().trim_matches('\'').to_owned())
            .filter(|s| !s.is_empty() && !s.contains(' '));
        let mut all = Vec::new();
        if let Some(one) = chosen {
            all.push(one);
        }
        for fallback in ["Adwaita", "Papirus", "breeze", "gnome", "hicolor"] {
            if !all.iter().any(|t| t == fallback) {
                all.push(fallback.to_owned());
            }
        }
        all
    })
}

/// Where a theme keeps an icon of a given name, in the order worth trying.
fn find(name: &str) -> Option<PathBuf> {
    let (_, roots) = dirs();
    for root in &roots {
        for theme in themes() {
            let base = root.join(theme);
            if !base.is_dir() {
                continue;
            }
            // Scalable first: it is drawn at whatever size the row is.
            for (dir, file) in [
                (base.join("scalable/mimetypes"), format!("{name}.svg")),
                (base.join("scalable/places"), format!("{name}.svg")),
                (base.join("scalable/apps"), format!("{name}.svg")),
            ] {
                let p = dir.join(&file);
                if p.is_file() {
                    return Some(p);
                }
            }
            for size in ["64x64", "48x48", "32x32", "24x24", "128x128", "16x16"] {
                for where_ in ["mimetypes", "places", "apps"] {
                    let p = base.join(size).join(where_).join(format!("{name}.png"));
                    if p.is_file() {
                        return Some(p);
                    }
                }
            }
        }
    }
    None
}

/// The icon names to try for a kind and an extension, most specific first.
///
/// The specific ones are there for themes that ship them; the generic one is
/// what Adwaita actually has, and is never wrong.
fn names_for(kind: &str, ext: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let specific: &[&str] = match ext {
        "pdf" => &["application-pdf", "x-office-document"],
        "doc" | "docx" | "odt" | "rtf" => &["x-office-document"],
        "xls" | "xlsx" | "ods" | "csv" => &["x-office-spreadsheet"],
        "ppt" | "pptx" | "odp" => &["x-office-presentation"],
        "html" | "htm" | "xhtml" => &["text-html"],
        "zip" | "tar" | "gz" | "xz" | "zst" | "7z" | "rar" | "bz2" => &["package-x-generic"],
        "so" | "dll" | "dylib" => &["application-x-sharedlib"],
        "iso" | "img" => &["application-x-cd-image", "media-optical"],
        "torrent" => &["application-x-bittorrent"],
        "epub" | "mobi" => &["x-office-document"],
        _ => &[],
    };
    names.extend(specific.iter().map(|s| (*s).to_owned()));
    // Then the theme's own name for the extension, which the specific themes
    // do ship — `text-rust`, `text-x-python` and so on.
    if !ext.is_empty() && ext.len() <= 12 && ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        names.push(format!("text-x-{ext}"));
        names.push(format!("application-x-{ext}"));
    }
    names.extend(
        match kind {
            "folder" => ["inode-directory", "folder"].as_slice(),
            "image" => &["image-x-generic"],
            "video" => &["video-x-generic"],
            "audio" => &["audio-x-generic"],
            "media" => &["video-x-generic"],
            "archive" => &["package-x-generic"],
            "exec" => &["application-x-executable"],
            "code" => &["text-x-script"],
            "build" => &["application-x-addon", "text-x-generic"],
            "config" => &["text-x-generic"],
            "font" => &["font-x-generic"],
            "doc" => &["x-office-document"],
            "data" => &["application-x-generic"],
            _ => &["text-x-generic"],
        }
        .iter()
        .map(|s| (*s).to_owned()),
    );
    names.push("text-x-generic".to_owned());
    names
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
    let k: Vec<u32> = (0..64)
        .map(|i| ((i as f64 + 1.0).sin().abs() * 4_294_967_296.0) as u32)
        .collect();

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

    #[test]
    fn a_kind_always_has_something_to_draw() {
        // Whatever the theme has or has not, the list of names to try ends
        // with one every theme is required to ship.
        for kind in [
            "file", "folder", "code", "image", "archive", "doc", "exec", "media", "audio", "video",
            "build", "data", "config", "font",
        ] {
            let names = names_for(kind, "");
            assert_eq!(names.last().map(String::as_str), Some("text-x-generic"));
        }
        // And a known extension is asked for by its own name first.
        assert_eq!(names_for("doc", "pdf").first().unwrap(), "application-pdf");
    }
}
