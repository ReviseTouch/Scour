//! The desktop's thumbnail cache: [`cache`] finds a picture, [`known`] says what
//! this machine can draw, [`make`] runs it, [`png`] writes the required chunks.
//! Nothing is decoded here — every picture is made by a program the machine
//! declared, in the freedesktop location, so other viewers share it. That
//! standard is a Unix one; elsewhere [`make::can_make`] simply answers false.

pub mod cache;
pub mod known;
pub mod make;
mod png;

pub use make::{Made, Maker, Wanted, can_make};

/// The kinds no thumbnailer will ever be asked about. Asked first because the
/// alternative is four `stat` calls per row, one per size directory. A token and
/// not a `Kind`, so this crate depends on nothing but `serde`.
pub fn never_for(token: &str) -> bool {
    matches!(token, "folder" | "code" | "build" | "exec" | "archive")
}

/// Is there a thumbnail for this file already, without reading it? Asked once per
/// row of every answer, which is why the kind gates it.
pub fn has(path: &str, token: &str) -> bool {
    !never_for(token) && cache::existing(path).is_some()
}

/// Nothing has made one — but could something be asked to? Zero I/O: two table
/// lookups, both read once at start, so a row nothing can draw never touches the
/// disk. It says nothing about whether the attempt would succeed.
pub fn may(path: &str, token: &str) -> bool {
    !never_for(token) && can_make(path)
}

/// Is this file itself a thumbnail? A thumbnail of a thumbnail is a loop: the
/// cache is a directory of PNGs, so drawing them writes more PNGs into the
/// directory an index is watching, and the list refreshes into another round.
pub fn is_one(path: &str) -> bool {
    match cache::dir().to_str() {
        Some(dir) => path.starts_with(dir),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    /// Whether either question says yes depends on what is installed; that these
    /// five are answered without touching a disk does not.
    #[test]
    fn five_kinds_are_answered_without_looking_at_anything() {
        for token in ["folder", "code", "build", "exec", "archive"] {
            assert!(super::never_for(token), "{token} should never be asked");
            assert!(!super::has("/x/y", token));
            assert!(!super::may("/x/y", token));
        }
        // The rest are answered by the machine's own table, not refused here.
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
