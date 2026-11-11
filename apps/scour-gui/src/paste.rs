//! What Ctrl+V and the menu's Paste put into a field. Read from X11's
//! CLIPBOARD through Xwayland first, since Slint's own Wayland read can fail
//! without a word; see the note on `copypasta` in Cargo.toml. `None` leaves it
//! to Slint, which is right on Windows and on a desktop with no X server.

/// The clipboard's text, or `None` if X11 cannot say.
pub fn text() -> Option<String> {
    #[cfg(all(unix, not(target_vendor = "apple")))]
    {
        use copypasta::ClipboardProvider;
        use copypasta::x11_clipboard::{Clipboard, X11ClipboardContext};
        std::env::var_os("DISPLAY")?;
        match X11ClipboardContext::<Clipboard>::new().and_then(|mut c| c.get_contents()) {
            Ok(text) => Some(text),
            Err(e) => {
                crate::trace(&format!("paste: X11 clipboard: {e}"));
                None
            }
        }
    }
    #[cfg(not(all(unix, not(target_vendor = "apple"))))]
    None
}

/// `text` with `pasted` in place of the bytes between `anchor` and `cursor`,
/// either way round, and where the caret goes after it. A one-line field takes
/// line breaks as spaces. `None` for offsets that do not fall on characters of
/// `text`.
pub fn splice(
    text: &str,
    anchor: usize,
    cursor: usize,
    pasted: &str,
    one_line: bool,
) -> Option<(String, usize)> {
    let (from, to) = (anchor.min(cursor), anchor.max(cursor));
    if !text.is_char_boundary(from) || !text.is_char_boundary(to) {
        return None;
    }
    let pasted = if one_line {
        pasted
            .trim_end_matches(['\r', '\n'])
            .replace("\r\n", " ")
            .replace(['\r', '\n'], " ")
    } else {
        pasted.to_owned()
    };
    let mut out = String::with_capacity(text.len() + pasted.len());
    out.push_str(&text[..from]);
    out.push_str(&pasted);
    out.push_str(&text[to..]);
    Some((out, from + pasted.len()))
}

#[cfg(test)]
mod tests {
    use super::splice;

    #[test]
    fn a_paste_replaces_the_selection_and_leaves_the_caret_after_it() {
        assert_eq!(splice("ab", 1, 1, "XY", true), Some(("aXYb".into(), 3)));
        // A selection made leftwards is the same selection.
        assert_eq!(splice("abcd", 3, 1, "Z", true), Some(("aZd".into(), 2)));
        assert_eq!(splice("", 0, 0, "dosya", true), Some(("dosya".into(), 5)));
        // Offsets are bytes, as Slint gives them.
        assert_eq!(splice("ğü", 2, 2, "ş", true), Some(("ğşü".into(), 4)));
        assert_eq!(splice("ğü", 1, 1, "x", true), None);
        assert_eq!(splice("ab", 0, 9, "x", true), None);
    }

    #[test]
    fn a_one_line_field_takes_line_breaks_as_spaces() {
        assert_eq!(
            splice("", 0, 0, "a\nb\r\nc\n", true),
            Some(("a b c".into(), 5))
        );
        assert_eq!(splice("", 0, 0, "a\nb", false), Some(("a\nb".into(), 3)));
    }
}
