//! The desktop's thumbnail cache: what is in it, who fills it, and how to ask.
//!
//! **Scour decodes nothing and invents nothing.** Every picture here was made
//! by a program the machine declared for the purpose, in the place the
//! freedesktop thumbnail managing standard says to put it, with the metadata
//! that standard requires. The result is shared: a picture Scour asked for is
//! one Files and Loupe find already made, and the reverse.
//!
//! Four pieces, in the order a request moves through them:
//!
//! * [`cache`] — where a picture lives and what its name means.
//! * [`known`] — what this machine declares it can draw, and with what.
//! * [`make`] — running that command, bounded, and putting the result in place.
//! * [`png`] — the two text chunks without which a thumbnail is invalid.
//!
//! ## Not a Windows story yet
//!
//! `.thumbnailer` files and `$XDG_CACHE_HOME/thumbnails` are a Unix desktop's
//! contract. This compiles everywhere and on a machine with neither it finds
//! nothing, so [`make::can_make`] answers false and nothing is ever asked for
//! — which is exactly the behaviour there was before any of this. Windows has
//! its own thumbnail cache behind `IThumbnailProvider`, and reaching it is a
//! COM story rather than a process one, and it is not written.

pub mod cache;
pub mod known;
pub mod make;
mod png;

pub use make::{Made, Maker, Wanted, can_make};

/// The kinds no thumbnailer will ever be asked about.
///
/// **The kind is asked first because the alternative is four `stat` calls.** A
/// thumbnail is looked for in four size directories and a row that has none —
/// which is nearly every row — pays for all four. On a machine where half the
/// files are source and build output, most of those questions have a known
/// answer: nothing thumbnails a `.rs` file, a directory or an ELF binary. The
/// unknown kind is still asked, because a picture with an unhelpful name is
/// exactly the case where the desktop knows better than the extension does.
///
/// **A token rather than a `Kind`**, so that this crate stays free of every
/// dependency but `serde`. The tokens are `scour_core::Kind::token`'s and are
/// a contract: they are what a `kind:` term is written with.
pub fn never_for(token: &str) -> bool {
    matches!(token, "folder" | "code" | "build" | "exec" | "archive")
}

/// Is there a thumbnail for this file already, without reading it?
///
/// Asked once per row of every answer, which is why the kind gates it.
pub fn has(path: &str, token: &str) -> bool {
    !never_for(token) && cache::existing(path).is_some()
}

/// Nothing has made one — but could something be asked to?
///
/// **Zero I/O**: an extension looked up in the machine's MIME table and a MIME
/// type looked up in its thumbnailer table, both read once at start. Cheaper
/// than [`has`], and so it is asked first — a row nothing can draw never
/// touches the disk at all.
///
/// It says nothing about whether the attempt would *succeed*. That costs a
/// process, and the answer to it is the failure directory this crate keeps.
pub fn may(path: &str, token: &str) -> bool {
    !never_for(token) && can_make(path)
}

/// Is this file itself a thumbnail?
///
/// **A thumbnail of a thumbnail is a loop.** The cache is a directory of PNGs
/// like any other, so a search for pictures finds them, a grid asks for their
/// thumbnails, and making those writes more PNGs into the same directory — an
/// index watching that directory then sees it change, the list refreshes, and
/// the whole thing goes round again. Measured while the window learned to draw
/// pictures: twenty-four thumbnailers started for twenty-four files that were
/// already thumbnails.
pub fn is_one(path: &str) -> bool {
    match cache::dir().to_str() {
        Some(dir) => path.starts_with(dir),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    /// The gate is the cost, so the gate is what to test.
    ///
    /// Whether either question can ever say yes depends on what is installed;
    /// that these five kinds are answered without touching a disk does not,
    /// and it is the property every frontend's row build is written against.
    #[test]
    fn five_kinds_are_answered_without_looking_at_anything() {
        for token in ["folder", "code", "build", "exec", "archive"] {
            assert!(super::never_for(token), "{token} should never be asked");
            assert!(!super::has("/x/y", token));
            assert!(!super::may("/x/y", token));
        }
        // And the ones a desktop might have something to say about are not
        // refused here — what answers them is the machine's own table.
        for token in ["image", "video", "doc", "audio", "file", "data"] {
            assert!(!super::never_for(token), "{token} should still be asked");
        }
    }

    #[test]
    fn a_thumbnail_is_never_asked_to_have_a_thumbnail() {
        let inside = super::cache::dir().join("large").join("abc.png");
        let inside = inside.to_string_lossy().into_owned();
        assert!(super::is_one(&inside));
        assert!(!super::may(&inside, "image"), "that way is a loop");
        assert!(!super::is_one("/home/somebody/holiday.png"));
    }
}
