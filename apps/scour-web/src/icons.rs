//! The picture a file already has.
//!
//! **The thumbnail, and nothing else.** The file manager writes one into
//! `~/.cache/thumbnails` under the MD5 of the file's URI, and every desktop
//! program reads them from there. Nothing is generated here — this only looks,
//! and asks the service when there is nothing to look at. Where a picture
//! lives and what its name means is `scour-thumbs`, so that the code reading
//! this cache and the code writing it are the same lines: a reader and a
//! writer that disagree by one escaped byte never meet, and the symptom is a
//! cache filling up while every tile stays blank.
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

/// Is there one, without reading it?
///
/// Asked once per row of a page, so that the page requests only the pictures
/// that exist rather than two hundred that mostly do not.
pub fn has_thumbnail(path: &str, kind: scour_core::Kind) -> bool {
    scour_thumbs::has(path, kind.token())
}

/// Nothing has made one — but could something be asked to?
///
/// **Zero I/O**, which is what makes it safe to ask once per row of every
/// answer: it is an extension looked up in the machine's MIME table and a MIME
/// type looked up in the machine's thumbnailer table, both read once at start.
/// Cheaper than [`has_thumbnail`], which is four `stat` calls when it says no,
/// and so it is asked first — a row that nothing can draw never touches the
/// disk at all.
///
/// It says nothing about whether the attempt would *succeed*. That costs a
/// process, and the answer to it is the failure directory the service keeps.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The gate is the cost, so the gate is what to test.
    ///
    /// Both questions are asked once per row of every answer, and on this
    /// machine half the rows of a broad query are source and build output.
    /// Whether either of them can ever say yes depends on what is installed;
    /// that these five kinds are answered without asking anything does not,
    /// and it is the property the row build is written against.
    #[test]
    fn five_kinds_are_answered_without_looking_at_anything() {
        use scour_core::Kind::*;
        for kind in [Dir, Code, Build, Exec, Archive] {
            assert!(!has_thumbnail("/a/holiday.png", kind), "{kind:?}");
            assert!(!may_thumbnail("/a/holiday.png", kind), "{kind:?}");
        }
        // And a name no MIME database on any machine gives a type to. This is
        // the answer for the ordinary row: nothing declared, nothing asked.
        assert!(!may_thumbnail("/a/notes.not-a-real-extension", Image));
        assert!(!may_thumbnail("", File));
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
