//! The thumbnail a file already has, and nothing else.
//!
//! The file manager writes one into `~/.cache/thumbnails` under the MD5 of the
//! file's URI; naming lives in `scour-thumbs`, so reader and writer agree. The
//! only paths leaving this module are ones it built itself, from a hash.

use std::path::Path;

/// What a browser is handed, and what it may keep.
pub struct Picture {
    pub bytes: Vec<u8>,
    pub kind: &'static str,
}

/// The thumbnail somebody has already made for this file.
pub fn thumbnail(path: &str) -> Option<Picture> {
    read(&scour_thumbs::cache::existing(path)?)
}

/// Is there one, without reading it? Asked once per row, so the page requests
/// only the pictures that exist.
pub fn has_thumbnail(path: &str, kind: scour_core::Kind) -> bool {
    scour_thumbs::has(path, kind.token())
}

/// Nothing has made one — but could something be asked to? Zero I/O, so it is
/// asked before [`has_thumbnail`]'s four `stat` calls.
pub fn may_thumbnail(path: &str, kind: scour_core::Kind) -> bool {
    scour_thumbs::may(path, kind.token())
}

fn read(p: &Path) -> Option<Picture> {
    let kind = match p.extension().and_then(|e| e.to_str()) {
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("xpm") => "image/x-xpixmap",
        _ => return None,
    };
    // A ceiling on a directory a package manager fills: an icon is kilobytes.
    let meta = std::fs::metadata(p).ok()?;
    if meta.len() > 4 * 1024 * 1024 {
        return None;
    }
    Some(Picture {
        bytes: std::fs::read(p).ok()?,
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate is the cost: these five kinds answer without touching a disk.
    #[test]
    fn five_kinds_are_answered_without_looking_at_anything() {
        use scour_core::Kind::*;
        for kind in [Dir, Code, Build, Exec, Archive] {
            assert!(!has_thumbnail("/a/holiday.png", kind), "{kind:?}");
            assert!(!may_thumbnail("/a/holiday.png", kind), "{kind:?}");
        }
        // A name no MIME database gives a type to: the ordinary row.
        assert!(!may_thumbnail("/a/notes.not-a-real-extension", Image));
        assert!(!may_thumbnail("", File));
    }

    /// Every kind the engine can name has a drawing in the page. The glyphs are
    /// CSS, so the compiler cannot notice a kind with no shape.
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
        // The fallback for a token this version has never heard of.
        assert!(has_rule(crate::PAGE, "file"));
    }

    /// Is there a `.k-<token>` selector, and not merely those characters? A
    /// plain `contains` finds `.k-font` inside `.k-fontXX`.
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
