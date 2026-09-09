//! What can be shown of a file: text, image, audio, video, PDF, or nothing.
//!
//! Input is a path and whether it is a directory; nothing else about Scour is
//! known here. The caller owns two things: whether the path may be read at all
//! (nothing here checks) and the response headers.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

/// The desktop's own quick-look command, if one is installed: GNOME's `sushi` or
/// macOS `qlmanage`. Found on `PATH`, not from `XDG_CURRENT_DESKTOP`.
pub fn quicklook(chosen: Option<&str>) -> Option<Vec<String>> {
    if let Some(cmd) = chosen {
        let parts: Vec<String> = cmd.split_whitespace().map(str::to_owned).collect();
        return (!parts.is_empty()).then_some(parts);
    }
    const CANDIDATES: &[&[&str]] = &[
        // `-p` previews; without it qlmanage writes thumbnails into the cwd.
        &["qlmanage", "-p"],
        // GNOME has shipped the binary under both names.
        &["sushi"],
        &["gnome-sushi"],
    ];
    CANDIDATES
        .iter()
        .find(|c| on_path(c[0]))
        .map(|c| c.iter().map(|s| (*s).to_owned()).collect())
}

/// Is this program on `PATH`? A lookup, never a shell — a shell would take the
/// name as a script.
fn on_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                let p = dir.join(name);
                p.is_file() && {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        std::fs::metadata(&p)
                            .map(|m| m.permissions().mode() & 0o111 != 0)
                            .unwrap_or(false)
                    }
                    #[cfg(not(unix))]
                    {
                        true
                    }
                }
            })
        })
        .unwrap_or(false)
}

/// How much text is worth sending: the head of any source file or config, far
/// short of what a log grows to.
const TEXT_CAP: u64 = 256 * 1024;

/// How much is read to decide whether a file is text at all.
const SNIFF: usize = 8 * 1024;

/// The largest picture sent whole. An image has no useful partial rendering, so
/// this is a refusal rather than a first page.
const IMAGE_CAP: u64 = 48 * 1024 * 1024;

/// How much is moved per write while streaming.
const CHUNK: usize = 64 * 1024;

/// What to do with a file, and what to call it on the way out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// Send the head of it as `text/plain`, whatever it is really called.
    Text,
    /// Send it whole, under this type. Images and PDFs.
    Whole(&'static str),
    /// Send it by range, under this type: a browser cannot seek audio or video
    /// without ranges.
    Streamed(&'static str),
    /// Nothing a browser can draw. The panel shows what is known instead.
    Nothing,
}

impl Shape {
    /// The category a caller builds an element from, and the content type. The
    /// category is derived from the type, so `DRAWN` cannot name the wrong bucket.
    pub fn shown(self) -> (&'static str, &'static str) {
        match self {
            Shape::Text => ("text", "text/plain"),
            Shape::Whole(t) | Shape::Streamed(t) => {
                let what = match t.split('/').next().unwrap_or("") {
                    "image" => "image",
                    "audio" => "audio",
                    "video" => "video",
                    _ => "pdf",
                };
                (what, t)
            }
            Shape::Nothing => ("none", ""),
        }
    }
}

/// What can be shown of one file: the decision, and the head of it for text. The
/// bytes of everything else stay with the caller, which owns the transport.
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub struct Look {
    /// `text`, `image`, `audio`, `video`, `pdf`, or `none`.
    pub shape: String,
    /// The content type, empty when there is nothing to show.
    pub kind: String,
    /// The file's length in bytes.
    pub len: u64,
    /// For text, the head of it — up to [`TEXT_CAP`]. Empty otherwise.
    pub head: String,
    /// True when `head` stops short of the file's end. Kept beside the text: a
    /// note appended to it would read as part of the file.
    pub cut: bool,
}

/// Look at a file and say what can be shown of it.
pub fn look_at(path: &std::path::Path, is_dir: bool) -> Look {
    let shape = shape_of(path, is_dir);
    let (what, kind) = shape.shown();
    let mut look = Look {
        shape: what.to_owned(),
        kind: kind.to_owned(),
        len: len_of(path).unwrap_or(0),
        ..Look::default()
    };
    if shape == Shape::Text
        && let Ok((head, whole, len)) = text_head(path)
    {
        look.head = head;
        look.cut = !whole;
        look.len = len;
    }
    look
}

/// The formats a browser draws, by extension and not by content: the browser
/// decides by the type header, so sniffing would let anything claim to be one.
const DRAWN: &[(&str, Shape)] = &[
    ("png", Shape::Whole("image/png")),
    ("jpg", Shape::Whole("image/jpeg")),
    ("jpeg", Shape::Whole("image/jpeg")),
    ("jfif", Shape::Whole("image/jpeg")),
    ("gif", Shape::Whole("image/gif")),
    ("webp", Shape::Whole("image/webp")),
    ("avif", Shape::Whole("image/avif")),
    ("bmp", Shape::Whole("image/bmp")),
    ("ico", Shape::Whole("image/x-icon")),
    // Safe only with the caller's `sandbox` header: an `<img>` never runs an
    // SVG's script, and the sandboxed opaque origin stops a direct tab too.
    ("svg", Shape::Whole("image/svg+xml")),
    // Streamed: a PDF viewer reads the trailer first, then pages in what it needs.
    ("pdf", Shape::Streamed("application/pdf")),
    ("mp3", Shape::Streamed("audio/mpeg")),
    ("m4a", Shape::Streamed("audio/mp4")),
    ("aac", Shape::Streamed("audio/aac")),
    ("oga", Shape::Streamed("audio/ogg")),
    ("ogg", Shape::Streamed("audio/ogg")),
    ("opus", Shape::Streamed("audio/ogg")),
    ("wav", Shape::Streamed("audio/wav")),
    ("flac", Shape::Streamed("audio/flac")),
    // The container names the type; whether the codec inside plays is the
    // browser's business.
    ("mp4", Shape::Streamed("video/mp4")),
    ("m4v", Shape::Streamed("video/mp4")),
    ("webm", Shape::Streamed("video/webm")),
    ("ogv", Shape::Streamed("video/ogg")),
    ("mov", Shape::Streamed("video/quicktime")),
    ("mkv", Shape::Streamed("video/x-matroska")),
];

/// How this file should be shown, if at all. A directory is never previewed.
pub fn shape_of(path: &std::path::Path, is_dir: bool) -> Shape {
    if is_dir {
        return Shape::Nothing;
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if let Some((_, shape)) = DRAWN.iter().find(|(e, _)| *e == ext) {
        return *shape;
    }
    // Everything else is decided by the bytes, not by a list of every extension
    // anyone ever gave a text file.
    if looks_like_text(path) {
        Shape::Text
    } else {
        Shape::Nothing
    }
}

/// Is the head of this file readable: valid UTF-8 with no NUL? The NUL rejects
/// binaries, the UTF-8 check other encodings. An empty file is text.
fn looks_like_text(path: &std::path::Path) -> bool {
    let Ok(mut f) = File::open(path) else {
        return false;
    };
    let mut head = vec![0u8; SNIFF];
    let Ok(n) = f.read(&mut head) else {
        return false;
    };
    head.truncate(n);
    if head.contains(&0) {
        return false;
    }
    match std::str::from_utf8(&head) {
        Ok(_) => true,
        // A character cut by the buffer's edge is not a reason to refuse.
        Err(e) => e.valid_up_to() + 4 >= head.len(),
    }
}

/// The head of a file as text, whether that is all of it, and the file's length.
/// Lossy: a file that sniffed as text can still hold one bad byte further in.
pub fn text_head(path: &std::path::Path) -> std::io::Result<(String, bool, u64)> {
    let f = File::open(path)?;
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
    let len = File::open(path)?.metadata()?.len();
    Ok((len <= IMAGE_CAP).then_some(len))
}

/// How long is it? A range answer needs the length first.
pub fn len_of(path: &std::path::Path) -> std::io::Result<u64> {
    Ok(File::open(path)?.metadata()?.len())
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
    let mut f = File::open(path)?;
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
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp(name: &str, bytes: &[u8]) -> PathBuf {
        let p = std::env::temp_dir().join(format!("scour-preview-{name}"));
        std::fs::write(&p, bytes).expect("write");
        p
    }

    #[test]
    fn a_picture_is_named_by_its_extension() {
        let p = tmp("a.png", b"\x89PNG\r\n\x1a\n");
        assert_eq!(shape_of(&p, false), Shape::Whole("image/png"));
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn a_readable_file_is_text_whatever_it_is_called() {
        for name in ["notes.bak", "gitignore", "thing.weird", "x.log"] {
            let p = tmp(name, "bir iki üç\ndört\n".as_bytes());
            assert_eq!(shape_of(&p, false), Shape::Text, "{name}");
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn something_with_a_nul_in_it_is_not_text() {
        let p = tmp("weights.bin", b"\x93NUMPY\x01\x00\x00binary\x00stuff");
        assert_eq!(shape_of(&p, false), Shape::Nothing);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn a_file_in_another_encoding_is_not_offered_as_utf8() {
        // "Türkçe" in ISO-8859-9: no NUL, so only the UTF-8 check can reject it.
        let p = tmp("latin5.txt", b"T\xfcrk\xe7e metin");
        assert_eq!(shape_of(&p, false), Shape::Nothing);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn an_empty_file_is_text_rather_than_a_refusal() {
        let p = tmp("empty.txt", b"");
        assert_eq!(shape_of(&p, false), Shape::Text);
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn a_folder_is_never_previewed() {
        let p = std::env::temp_dir();
        assert_eq!(shape_of(&p, true), Shape::Nothing);
    }

    #[test]
    fn a_multibyte_character_split_by_the_sniff_boundary_is_still_text() {
        let mut bytes = vec![b'a'; SNIFF - 1];
        bytes.extend_from_slice("ş".as_bytes()); // two bytes, one of them past the edge
        bytes.extend_from_slice(&[b'b'; 64]);
        let p = tmp("edge.txt", &bytes);
        assert_eq!(shape_of(&p, false), Shape::Text);
        let _ = std::fs::remove_file(p);
    }

    /// Derived from the content type, so a format added to `DRAWN` cannot land
    /// in the wrong bucket.
    #[test]
    fn every_drawn_format_says_which_element_it_is() {
        for (ext, shape) in DRAWN {
            let (what, kind) = shape.shown();
            assert!(
                ["image", "audio", "video", "pdf"].contains(&what),
                "{ext} → {what}"
            );
            assert!(kind.contains('/'), "{ext} → {kind:?}");
        }
        assert_eq!(Shape::Text.shown(), ("text", "text/plain"));
        assert_eq!(Shape::Nothing.shown().0, "none");
    }

    /// Never `text/html`: a page served under its own type, from the origin
    /// whose URL carries the token, is stored cross-site scripting.
    #[test]
    fn a_web_page_is_offered_as_text_and_not_as_a_page() {
        for name in ["index.html", "page.htm", "thing.xhtml", "x.js"] {
            let p = tmp(name, b"<script>alert(1)</script>\n");
            assert_eq!(shape_of(&p, false), Shape::Text, "{name}");
            let _ = std::fs::remove_file(p);
        }
        assert!(!DRAWN.iter().any(|(_, s)| s.shown().1.contains("html")));
    }

    #[test]
    fn a_span_is_the_bytes_that_were_asked_for() {
        let p = tmp("span.bin", b"0123456789");
        let mut out = Vec::new();
        assert_eq!(write_span(&p, 3, 4, &mut out).expect("span"), 4);
        assert_eq!(out, b"3456");

        // A file shorter than the promise stops at its end rather than padding.
        let mut out = Vec::new();
        assert_eq!(write_span(&p, 8, 99, &mut out).expect("span"), 2);
        assert_eq!(out, b"89");
        let _ = std::fs::remove_file(p);
    }

    #[test]
    fn the_head_of_a_long_file_says_it_is_only_the_head() {
        let big = tmp("big.txt", &vec![b'x'; (TEXT_CAP + 10) as usize]);
        let (text, whole, len) = text_head(&big).expect("head");
        assert_eq!(text.len() as u64, TEXT_CAP);
        assert!(!whole);
        assert_eq!(len, TEXT_CAP + 10);
        let _ = std::fs::remove_file(big);

        let small = tmp("small.txt", b"iki satir\n");
        let (text, whole, len) = text_head(&small).expect("head");
        assert_eq!(text, "iki satir\n");
        assert!(whole);
        assert_eq!(len, 10);
        let _ = std::fs::remove_file(small);
    }

    #[test]
    fn a_chosen_quicklook_wins_and_an_empty_one_is_no_quicklook() {
        assert_eq!(
            quicklook(Some("gwenview --fullscreen")),
            Some(vec!["gwenview".into(), "--fullscreen".into()])
        );
        assert_eq!(quicklook(Some("   ")), None);
    }

    #[test]
    fn ranges_are_read_the_way_a_browser_writes_them() {
        assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
        assert_eq!(parse_range("bytes=500-", 1000), Some((500, 999)));
        // The suffix form, which is how a PDF viewer opens a file.
        assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
        // Clamped, not refused: a browser asks for more than there is before it
        // knows the length.
        assert_eq!(parse_range("bytes=0-99999", 1000), Some((0, 999)));
        // What is not understood means "all of it", not "nothing".
        assert_eq!(parse_range("bytes=0-10,20-30", 1000), None);
        assert_eq!(parse_range("items=0-10", 1000), None);
        assert_eq!(parse_range("bytes=abc-", 1000), None);
        // A start past the end is a request that cannot be met.
        assert_eq!(parse_range("bytes=2000-", 1000), None);
        assert_eq!(parse_range("bytes=0-0", 0), None);
    }
}
