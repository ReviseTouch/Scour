//! A file's bytes for a preview: its head as text, or a range of it.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

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
pub fn text_head(path: &std::path::Path) -> std::io::Result<(String, bool, u64)> {
    let f = open_file(path)?;
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let mut buf = Vec::with_capacity(TEXT_CAP.min(len.max(1)) as usize);
    f.take(TEXT_CAP).read_to_end(&mut buf)?;
    Ok((
        String::from_utf8_lossy(&buf).into_owned(),
        len <= TEXT_CAP,
        len,
    ))
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
