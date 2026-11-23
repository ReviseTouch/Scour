//! A capsule's frame — `!`, the field name, `:`, and enclosing quotes — is part
//! of the capsule rather than editable text. Only the value after `:` is typed
//! into; moving, selecting, or deleting treats the frame as one indivisible unit.

use crate::lens::{self, Term};

fn clamp_boundary(s: &str, mut b: usize) -> usize {
    b = b.min(s.len());
    while !s.is_char_boundary(b) {
        b -= 1;
    }
    b
}

/// Byte offset right after `:` when `t` is a recognised filter capsule.
pub fn head_of(query: &str, t: &Term, mut known_field: impl FnMut(&Term) -> bool) -> Option<usize> {
    let raw = query.get(t.start..t.end)?;
    let bare = raw.strip_prefix('!').unwrap_or(raw);
    if bare.starts_with('"') {
        return None;
    }
    let colon = bare.find(':')?;
    let name = &bare[..colon];
    if name.chars().count() < 2 || !name.chars().all(char::is_alphabetic) {
        return None;
    }
    if !(t.field.is_some() || known_field(t)) {
        return None;
    }
    Some(t.start + usize::from(t.not) + colon + 1)
}

/// Where editable value characters begin inside a capsule (past an opening `"`).
pub fn value_start(query: &str, head: usize, end: usize) -> usize {
    if head < end && query.as_bytes().get(head) == Some(&b'"') {
        head + 1
    } else {
        head
    }
}

/// Keep the caret and any selection out of a capsule's locked `field:` header.
pub fn clamp_selection(
    query: &str,
    prev_cursor: usize,
    anchor: usize,
    cursor: usize,
    mut known_field: impl FnMut(&Term) -> bool,
) -> (usize, usize) {
    let (a, c) = (clamp_boundary(query, anchor), clamp_boundary(query, cursor));
    let prev = clamp_boundary(query, prev_cursor);
    if a == c {
        for t in lens::terms(query) {
            let Some(head) = head_of(query, &t, &mut known_field) else {
                continue;
            };
            let entry = value_start(query, head, t.end);
            if c > t.start && c < entry {
                let snapped = if prev >= entry && c < prev {
                    t.start
                } else {
                    entry
                };
                return (snapped, snapped);
            }
        }
        return (a, c);
    }
    let (mut lo, mut hi) = (a.min(c), a.max(c));
    for t in lens::terms(query) {
        let Some(head) = head_of(query, &t, &mut known_field) else {
            continue;
        };
        if lo < head && hi > t.start {
            lo = lo.min(t.start);
            hi = hi.max(t.end);
        }
    }
    if a <= c { (lo, hi) } else { (hi, lo) }
}

/// Snap a pointer press onto the capsule's value rather than its `field:` badge.
pub fn snap_press(query: &str, offset: usize, mut known_field: impl FnMut(&Term) -> bool) -> usize {
    let off = clamp_boundary(query, offset);
    for t in lens::terms(query) {
        let Some(head) = head_of(query, &t, &mut known_field) else {
            continue;
        };
        let entry = value_start(query, head, t.end);
        if off >= t.start && off < entry {
            return entry;
        }
    }
    off
}

/// Double-click range: on the `field:` badge selects the whole capsule; on the
/// value selects only the value so typing immediately replaces `docx` in `ext:docx`.
pub fn word_range(
    query: &str,
    at: usize,
    mut known_field: impl FnMut(&Term) -> bool,
) -> (usize, usize) {
    let at = clamp_boundary(query, at);
    for t in lens::terms(query) {
        if at < t.start || at > t.end {
            continue;
        }
        if let Some(head) = head_of(query, &t, &mut known_field) {
            if at < head {
                return (t.start, t.end);
            }
            let s = value_start(query, head, t.end);
            let e = if t.end > s && query.as_bytes().get(t.end - 1) == Some(&b'"') {
                t.end - 1
            } else {
                t.end
            };
            return (s, e);
        }
        return (t.start, t.end);
    }
    (at, at)
}

/// Backspace at a capsule's frame: never breaks `field:`, removes the whole
/// capsule when its value is empty, and avoids merging the preceding word into `field:`.
pub fn backspace(
    query: &str,
    cursor: usize,
    mut known_field: impl FnMut(&Term) -> bool,
) -> Option<(String, usize)> {
    let c = clamp_boundary(query, cursor);
    let b = query.as_bytes();
    for t in lens::terms(query) {
        let Some(head) = head_of(query, &t, &mut known_field) else {
            continue;
        };
        let quoted = b.get(head) == Some(&b'"');
        if quoted && c == t.end && c > head + 1 && b[c - 1] == b'"' {
            return Some((query.to_string(), c - 1));
        }
        if quoted && c == head + 1 && b.get(c) == Some(&b'"') {
            return Some((format!("{}{}", &query[..head], &query[head + 2..]), head));
        }
        if c == head && c == t.end {
            return Some(lens::replace(query, &t, ""));
        }
        let entry = value_start(query, head, t.end);
        if c > t.start && c <= entry {
            return Some((query.to_string(), entry));
        }
        if c == t.start && c >= 2 && b[c - 1] == b' ' && !b[c - 2].is_ascii_whitespace() {
            return Some((query.to_string(), c - 1));
        }
    }
    None
}

/// Delete at a capsule's leading edge removes the whole capsule instead of its first letter.
pub fn delete_forward(
    query: &str,
    cursor: usize,
    mut known_field: impl FnMut(&Term) -> bool,
) -> Option<(String, usize)> {
    let c = clamp_boundary(query, cursor);
    let b = query.as_bytes();
    for t in lens::terms(query) {
        let Some(head) = head_of(query, &t, &mut known_field) else {
            continue;
        };
        if c == t.start {
            return Some(lens::replace(query, &t, ""));
        }
        if c > t.start && c < head {
            return Some((query.to_string(), value_start(query, head, t.end)));
        }
        if c + 1 == t.start && b.get(c) == Some(&b' ') && c > 0 && !b[c - 1].is_ascii_whitespace() {
            return Some((query.to_string(), t.start));
        }
    }
    None
}

/// Typing right before a capsule (`|ext:docx`) keeps a separating space so the
/// capsule's `field:` badge is never merged with new text.
pub fn insert_before(
    query: &str,
    cursor: usize,
    inserted: &str,
    mut known_field: impl FnMut(&Term) -> bool,
) -> Option<(String, usize)> {
    // A key that is not a letter — an arrow, End, F2 — arrives as text too, in
    // the private use area: written in, it was a blank before the capsule.
    let key = |ch: char| ch.is_control() || ('\u{E000}'..='\u{F8FF}').contains(&ch);
    if inserted.is_empty() || inserted.chars().any(key) {
        return None;
    }
    if inserted.ends_with(char::is_whitespace) {
        return None;
    }
    let c = clamp_boundary(query, cursor);
    for t in lens::terms(query) {
        if c == t.start && head_of(query, &t, &mut known_field).is_some() {
            let out = format!("{}{inserted} {}", &query[..c], &query[c..]);
            return Some((out, c + inserted.len()));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn known(t: &Term) -> bool {
        t.field.is_some()
    }

    /// An arrow or End pressed in front of a capsule moves the caret; it is
    /// not text to keep apart from the capsule.
    #[test]
    fn a_key_that_is_not_a_letter_is_not_written_before_a_capsule() {
        let q = "ext:docx";
        assert!(insert_before(q, 0, "a", known).is_some());
        for key in ["\u{F72B}", "\u{F703}", "\u{1b}"] {
            assert_eq!(insert_before(q, 0, key, known), None, "{key:?}");
        }
    }

    #[test]
    fn caret_never_stands_inside_capsule_header() {
        let q = "ext:docx";
        assert_eq!(clamp_selection(q, 0, 2, 2, known), (4, 4));
        assert_eq!(clamp_selection(q, 4, 3, 3, known), (0, 0));
        assert_eq!(clamp_selection(q, 0, 1, 1, known), (4, 4));
        assert_eq!(snap_press(q, 0, known), 4);
        assert_eq!(snap_press(q, 2, known), 4);
        assert_eq!(snap_press(q, 6, known), 6);
    }

    #[test]
    fn backspace_and_delete_protect_capsule_header() {
        assert_eq!(
            backspace("ext:docx", 4, known),
            Some(("ext:docx".into(), 4))
        );
        assert_eq!(backspace("ext:", 4, known), Some((String::new(), 0)));
        assert_eq!(
            backspace("rapor ext:docx", 6, known),
            Some(("rapor ext:docx".into(), 5))
        );
        assert_eq!(
            delete_forward("rapor ext:docx", 6, known),
            Some(("rapor".into(), 5))
        );
        assert_eq!(
            insert_before("ext:docx", 0, "a", known),
            Some(("a ext:docx".into(), 1))
        );
    }

    #[test]
    fn selection_and_double_click_respect_capsule_boundary() {
        let q = "ext:docx";
        assert_eq!(clamp_selection(q, 8, 6, 2, known), (8, 0));
        assert_eq!(clamp_selection(q, 4, 4, 7, known), (4, 7));
        assert_eq!(word_range(q, 6, known), (4, 8));
        assert_eq!(word_range(q, 1, known), (0, 8));
    }
}
