//! A file's bytes for a preview: its head as text, or a range of it.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

use crate::{Encoding, decode, encoding_of};

/// How much text is worth sending: the head of any source file or config, far
/// short of what a log grows to.
pub(super) const TEXT_CAP: u64 = 256 * 1024;

/// The largest picture sent whole. An image has no useful partial rendering, so
/// this is a refusal rather than a first page.
const IMAGE_CAP: u64 = 48 * 1024 * 1024;

/// How much is moved per write while streaming.
const CHUNK: usize = 64 * 1024;

/// Open a regular file, and refuse anything else before opening it. Opening a
/// named pipe for reading waits for a writer — for ever, on a thread that
/// was answering a click — and a device such as `/dev/zero` never ends. The
/// `stat` follows a link, as the open would, and does not block.
pub fn open_file(path: &std::path::Path) -> std::io::Result<File> {
    if !std::fs::metadata(path)?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    File::open(path)
}

/// The head of a file as text, whether that is all of it, and the file's length.
/// Lossy: a file that sniffed as text can still hold one bad byte further in.
/// In the encoding its first bytes say: see [`crate::encoding_of`].
pub fn text_head(path: &std::path::Path) -> std::io::Result<(String, bool, u64)> {
    let f = open_file(path)?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut buf = Vec::with_capacity(TEXT_CAP.min(len.max(1)) as usize);
    f.take(TEXT_CAP).read_to_end(&mut buf)?;
    let encoding = encoding_at(&buf);
    Ok((decode(&buf, encoding, true), len <= TEXT_CAP, len))
}

/// What a file whose first bytes are `buf`'s is written in; UTF-8 when they
/// say nothing, as for a file that is not text but is shown as text anyway.
fn encoding_at(buf: &[u8]) -> Encoding {
    encoding_of(&buf[..buf.len().min(super::shape::SNIFF)]).unwrap_or(Encoding::Utf8)
}

/// A stretch of a text file around one place in it, and where that stretch
/// starts: what a preview shows of a file whose find lies past its head.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Near {
    pub text: String,
    /// More of the file follows the stretch.
    pub cut: bool,
    /// The file's length.
    pub len: u64,
    /// The byte the stretch starts at, and the line that byte is on, counted
    /// from one: mid-line when that line began more than [`BEFORE_BYTES`]
    /// before the place.
    pub from: u64,
    pub first_line: u64,
}

/// How much of what leads to the place a stretch shows: this many lines, and
/// never more than this many bytes of them.
const BEFORE_LINES: usize = 3;
const BEFORE_BYTES: u64 = 2 * 1024;

/// The text around a place in a file — `offset`, a byte, or else the start of
/// `line`, counted from one — from a few lines before it, for up to
/// [`TEXT_CAP`]. A line that began far before the place is entered mid-way, on
/// a character. With neither, the head, as [`text_head`] reads it.
pub fn text_near(
    path: &std::path::Path,
    offset: Option<u64>,
    line: Option<u64>,
) -> std::io::Result<Near> {
    use std::io::BufRead;
    if offset.is_none() && line.is_none_or(|l| l <= 1) {
        let (text, whole, len) = text_head(path)?;
        return Ok(Near {
            text,
            cut: !whole,
            len,
            from: 0,
            first_line: 1,
        });
    }
    let mut f = open_file(path)?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut head = Vec::with_capacity(super::shape::SNIFF);
    (&mut f)
        .take(super::shape::SNIFF as u64)
        .read_to_end(&mut head)?;
    let encoding = encoding_at(&head);
    f.seek(SeekFrom::Start(0))?;
    let mut reader = std::io::BufReader::with_capacity(CHUNK, f);
    // Walked to the place, counting newlines and keeping where the last few
    // lines began, as (byte, line).
    let mut pos: u64 = 0;
    let mut newlines: u64 = 0;
    let mut starts: std::collections::VecDeque<(u64, u64)> = [(0, 1)].into();
    'walk: loop {
        let reached = match (offset, line) {
            (Some(o), _) => pos >= o,
            (None, Some(l)) => newlines + 1 >= l,
            (None, None) => true,
        };
        if reached {
            break;
        }
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            break;
        }
        let take = offset.map_or(buf.len(), |o| buf.len().min((o - pos) as usize));
        let mut used = take;
        for (i, &b) in buf[..take].iter().enumerate() {
            if b != b'\n' {
                continue;
            }
            newlines += 1;
            starts.push_back((pos + i as u64 + 1, newlines + 1));
            if starts.len() > BEFORE_LINES + 1 {
                starts.pop_front();
            }
            if offset.is_none() && line.is_some_and(|l| newlines + 1 >= l) {
                used = i + 1;
                reader.consume(used);
                pos += used as u64;
                break 'walk;
            }
        }
        reader.consume(used);
        pos += used as u64;
    }
    let at = offset.unwrap_or(pos).min(len);
    let floor = at.saturating_sub(BEFORE_BYTES);
    // The earliest of the last few line starts that is close enough; when even
    // the place's own line began further back, inside it.
    let (mut from, first_line) = starts
        .iter()
        .copied()
        .find(|&(b, _)| b >= floor && b <= at)
        .unwrap_or((floor, newlines + 1));
    // UTF-16 is read two bytes at a time, from the first after the mark.
    if matches!(encoding, Encoding::Utf16Le | Encoding::Utf16Be) {
        from &= !1;
        if from > 0 && from < 2 {
            from = 2;
        }
    }
    let mut f = open_file(path)?;
    f.seek(SeekFrom::Start(from))?;
    let mut buf = Vec::with_capacity(TEXT_CAP.min(len.saturating_sub(from).max(1)) as usize);
    f.take(TEXT_CAP).read_to_end(&mut buf)?;
    // Entered mid-line: not in the middle of a character, which only UTF-8
    // has more than one byte of and does not say by its first.
    let skip = if encoding != Encoding::Utf8 || starts.iter().any(|&(b, _)| b == from) {
        0
    } else {
        buf.iter()
            .take(3)
            .take_while(|&&b| b & 0xC0 == 0x80)
            .count()
    };
    let read = buf.len() as u64;
    Ok(Near {
        text: decode(&buf[skip..], encoding, from == 0),
        cut: from + read < len,
        len,
        from: from + skip as u64,
        first_line,
    })
}

/// The length, or `None` past [`IMAGE_CAP`] — where the caller should report the
/// size instead of sending the file.
pub fn whole_len(path: &std::path::Path) -> std::io::Result<Option<u64>> {
    let len = open_file(path)?.metadata()?.len();
    Ok((len <= IMAGE_CAP).then_some(len))
}

/// How long is it? A range answer needs the length first.
pub fn len_of(path: &std::path::Path) -> std::io::Result<u64> {
    Ok(open_file(path)?.metadata()?.len())
}

/// Write `count` bytes starting at `from` into `out`, and never more: the length
/// is already in a header. A short file stops early and reports what moved, so
/// the response has to carry `Connection: close`.
pub fn write_span(
    path: &std::path::Path,
    from: u64,
    count: u64,
    out: &mut dyn Write,
) -> std::io::Result<u64> {
    let mut f = open_file(path)?;
    if from > 0 {
        f.seek(SeekFrom::Start(from))?;
    }
    let mut buf = vec![0u8; CHUNK.min(count.max(1) as usize)];
    let mut left = count;
    while left > 0 {
        let want = buf.len().min(left as usize);
        let n = f.read(&mut buf[..want])?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n])?;
        left -= n as u64;
    }
    Ok(count - left)
}

/// `bytes=a-b`, `bytes=a-`, `bytes=-n`. Inclusive, like the specification. One
/// range only; `None` means the caller should send the whole file.
pub fn parse_range(header: &str, len: u64) -> Option<(u64, u64)> {
    if len == 0 {
        return None;
    }
    let spec = header.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let last = len - 1;
    let (from, to) = match (a.trim(), b.trim()) {
        // The suffix form: the final n bytes, which is how a PDF viewer opens.
        ("", n) => {
            let n: u64 = n.parse().ok()?;
            (len.saturating_sub(n.max(1)), last)
        }
        (s, "") => (s.parse().ok()?, last),
        (s, e) => (s.parse().ok()?, e.parse::<u64>().ok()?.min(last)),
    };
    if from > to || from > last {
        return None;
    }
    Some((from, to))
}

#[cfg(test)]
mod near_tests {
    use super::*;

    /// A file of its own, removed when the guard goes.
    struct Gone(std::path::PathBuf);
    impl Drop for Gone {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    fn file(text: &str) -> (Gone, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "scour-near-{}-{}.txt",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&p, text).expect("write");
        (Gone(p.clone()), p)
    }

    /// A few lines before the place, and the number of the first of them.
    #[test]
    fn a_stretch_starts_a_few_lines_before_the_place() {
        let text: String = (1..=50).map(|n| format!("line {n}\n")).collect();
        let (_d, p) = file(&text);
        let by_line = text_near(&p, None, Some(20)).expect("near");
        assert_eq!(by_line.first_line, 17);
        assert!(
            by_line.text.starts_with("line 17\n"),
            "{:?}",
            &by_line.text[..20]
        );
        let at = text.find("line 20\n").expect("there") as u64 + 2;
        let by_offset = text_near(&p, Some(at), None).expect("near");
        assert_eq!(by_offset.first_line, 17);
        assert_eq!(
            by_offset.from,
            text.find("line 17\n").expect("there") as u64
        );
        assert!(by_offset.text.starts_with("line 17\n"));
        let head = text_near(&p, None, None).expect("head");
        assert_eq!((head.first_line, head.from, head.cut), (1, 0, false));
    }

    /// A find deep in one long line: entered mid-way, on a character, and
    /// still counted as that line.
    #[test]
    fn a_long_line_is_entered_near_the_place_on_a_character() {
        let long = format!(
            "{}ğ{}FIND{}",
            "a".repeat(9_000),
            "b".repeat(1_100),
            "c".repeat(10)
        );
        let text = format!("first\n{long}\nlast\n");
        let (_d, p) = file(&text);
        let at = text.find("FIND").expect("there") as u64;
        let near = text_near(&p, Some(at), None).expect("near");
        assert_eq!(near.first_line, 2);
        assert!(near.text.contains("FIND"));
        assert!(near.text.len() < 3_000, "{}", near.text.len());
        assert!(!near.text.starts_with('\u{FFFD}'));
    }
}
