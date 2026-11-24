//! What is shown of a find: the line it is on, or the bytes around it.

use std::borrow::Cow;

use memchr::{memchr, memrchr};

/// One place the needle was found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Bytes from the start of the file.
    pub offset: u64,
    /// Counted from one. None in a file that has no lines: not text.
    pub line: Option<u64>,
    /// The line, or the neighbourhood, with nothing a terminal would obey.
    pub text: String,
    /// Where in `text` the find itself is: byte offset and length.
    pub at: (u32, u32),
}

/// What a file's first bytes say it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Text,
    /// It opens with a UTF-16 byte-order mark; `true` is big-endian.
    Wide(bool),
    Binary,
}

impl Kind {
    /// A NUL in the first 8 KiB is what `grep` and `git` call binary too.
    pub(crate) fn sniff(head: &[u8]) -> Kind {
        match head {
            [0xFF, 0xFE, ..] => Kind::Wide(false),
            [0xFE, 0xFF, ..] => Kind::Wide(true),
            _ if memchr(0, &head[..head.len().min(8 << 10)]).is_some() => Kind::Binary,
            _ => Kind::Text,
        }
    }
}

/// The text around `start..end` of `hay`, at most about `room` bytes of it.
pub(crate) fn around(
    hay: &[u8],
    start: usize,
    end: usize,
    kind: Kind,
    room: usize,
) -> (String, (u32, u32)) {
    let lead = room / 3;
    let end = end.min(start + room).max(start);
    let (mut from, mut to) = (
        start.saturating_sub(lead),
        (start.saturating_sub(lead) + room).max(end).min(hay.len()),
    );
    // Whether the line goes on past what is shown, at either end.
    let (mut cut_before, mut cut_after) = (false, false);
    let read: fn(&[u8]) -> String = match kind {
        Kind::Text => {
            let line = (
                memrchr(b'\n', &hay[..start]).map_or(0, |i| i + 1),
                memchr(b'\n', &hay[end..]).map_or(hay.len(), |i| end + i),
            );
            (cut_before, cut_after) = (from > line.0, to < line.1);
            (from, to) = (from.max(line.0), to.min(line.1));
            // Not through the middle of a character.
            while from < start && hay[from] & 0xC0 == 0x80 {
                from += 1;
            }
            while to > end && to < hay.len() && hay[to] & 0xC0 == 0x80 {
                to -= 1;
            }
            // A line that ends `\r\n` ends before both.
            if to > end && hay[to - 1] == b'\r' {
                to -= 1;
            }
            // Not UTF-8: then the code page older Turkish files are in.
            if std::str::from_utf8(&hay[from..to]).is_ok() {
                |b| tame(String::from_utf8_lossy(b))
            } else {
                |b| tame(Cow::Owned(scour_extract::cp1254_decode(b)))
            }
        }
        Kind::Wide(big) => {
            // Whole code units, counted from the find outwards.
            from += (start - from) % 2;
            to -= (to - end) % 2;
            if big {
                |b| wide(b, true)
            } else {
                |b| wide(b, false)
            }
        }
        Kind::Binary => |b| {
            let kept: Vec<u8> = b.iter().copied().filter(|&c| c != 0).collect();
            tame(String::from_utf8_lossy(&kept))
        },
    };
    let (mut before, found, mut after) = (
        read(&hay[from..start]),
        read(&hay[start..end]),
        read(&hay[end..to]),
    );
    if matches!(kind, Kind::Wide(_)) {
        // Its lines end where a decoded newline is, not where a byte is.
        before = before.rsplit('\n').next().unwrap_or("").to_owned();
        after = after.split('\n').next().unwrap_or("").to_owned();
    }
    if cut_before {
        before.insert(0, '…');
    }
    if cut_after {
        after.push('…');
    }
    let at = (before.len() as u32, found.len() as u32);
    (before + &found + &after, at)
}

fn wide(bytes: &[u8], big: bool) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|p| {
            if big {
                u16::from_be_bytes([p[0], p[1]])
            } else {
                u16::from_le_bytes([p[0], p[1]])
            }
        })
        .collect();
    String::from_utf16_lossy(&units)
        .chars()
        .filter(|c| *c != '\r')
        .map(|c| if c.is_control() && c != '\n' { ' ' } else { c })
        .collect()
}

/// A file's bytes are not a terminal's to obey: an escape is shown as a dot.
fn tame(text: Cow<'_, str>) -> String {
    text.chars()
        .map(|c| match c {
            '\t' => ' ',
            c if c.is_control() || c == '\u{FFFD}' => '·',
            c => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_cut_at_its_ends_and_at_characters() {
        let hay = "first\nbir şey burada\nlast".as_bytes();
        let start = memchr::memmem::find(hay, "şey".as_bytes()).unwrap();
        let (text, at) = around(hay, start, start + "şey".len(), Kind::Text, 160);
        assert_eq!(text, "bir şey burada");
        assert_eq!(&text[at.0 as usize..(at.0 + at.1) as usize], "şey");
        let (short, at) = around(hay, start, start + "şey".len(), Kind::Text, 6);
        assert_eq!(&short[at.0 as usize..(at.0 + at.1) as usize], "şey");
        // Cut at both ends, and it says so.
        assert_eq!(short, "…r şey…");
    }

    #[test]
    fn a_line_that_is_not_utf8_is_read_as_1254() {
        let hay = b"ilk\nbu \xFEehir g\xFCzel\nson";
        let (text, at) = around(hay, 7, 12, Kind::Text, 160);
        assert_eq!(text, "bu şehir güzel");
        assert_eq!(&text[at.0 as usize..(at.0 + at.1) as usize], "şehir");
    }

    #[test]
    fn an_escape_in_a_file_is_not_passed_on() {
        let hay = b"a\x1b[31mneedle\x07";
        let (text, _) = around(hay, 6, 12, Kind::Text, 160);
        assert!(!text.contains('\x1b') && !text.contains('\x07'), "{text:?}");
    }

    #[test]
    fn binary_shows_utf16_text_without_its_nuls() {
        let hay: Vec<u8> = "\u{1}key=value"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let (text, at) = around(&hay, 2, 8, Kind::Binary, 160);
        assert_eq!(text, "·key=value");
        assert_eq!(&text[at.0 as usize..(at.0 + at.1) as usize], "key");
    }

    #[test]
    fn the_first_bytes_say_what_a_file_is() {
        assert_eq!(Kind::sniff(b"plain"), Kind::Text);
        assert_eq!(Kind::sniff(b"\xFF\xFEa\0"), Kind::Wide(false));
        assert_eq!(Kind::sniff(b"\xFE\xFF\0a"), Kind::Wide(true));
        assert_eq!(Kind::sniff(b"\x7fELF\0\0"), Kind::Binary);
    }
}
