//! What can be shown of a file: text, image, audio, video, PDF, or nothing.
//!
//! Input is a path and whether it is a directory; nothing else about Scour is
//! known here. The caller owns two things: whether the path may be read at all
//! (nothing here checks) and the response headers.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};

pub mod doc;
pub mod office;
pub mod ooxml;
mod pdf_macos;
mod pdf_win;
mod ppm;

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
        // GNOME, Cinnamon, and GTK quick-look viewers.
        &["sushi"],
        &["gnome-sushi"],
        &["nemo-preview"],
        &["gloobus-preview"],
        &["QuickLook.exe"],
    ];
    if let Some(found) = CANDIDATES
        .iter()
        .find(|c| on_path(c[0]))
        .map(|c| c.iter().map(|s| (*s).to_owned()).collect())
    {
        return Some(found);
    }
    #[cfg(windows)]
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let ql = std::path::PathBuf::from(local).join(r"Programs\QuickLook\QuickLook.exe");
        if ql.is_file() {
            return Some(vec![ql.to_string_lossy().into_owned()]);
        }
    }
    None
}

/// Is this program on `PATH`? A lookup, never a shell — a shell would take the
/// name as a script.
fn on_path(name: &str) -> bool {
    find_on_path(name).is_some()
}

pub(crate) fn find_on_path(name: &str) -> Option<std::path::PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|p| {
            p.is_file() && {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::metadata(p)
                        .map(|m| m.permissions().mode() & 0o111 != 0)
                        .unwrap_or(false)
                }
                #[cfg(not(unix))]
                {
                    true
                }
            }
        })
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
    } else if office::is_office(path)
        && let Some((head, cut)) = ooxml::extract_text(path)
    {
        if shape == Shape::Nothing {
            look.shape = "text".to_owned();
            look.kind = "text/plain".to_owned();
        }
        look.head = head;
        look.cut = cut;
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
    if office::is_office(path) && office::can_convert() {
        return Shape::Streamed("application/pdf");
    }
    // Everything else is decided by the bytes, not by a list of every extension
    // anyone ever gave a text file.
    if looks_like_text(path) {
        Shape::Text
    } else {
        Shape::Nothing
    }
}

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

/// Is the head of this file readable: valid UTF-8 with no NUL? The NUL rejects
/// binaries, the UTF-8 check other encodings. An empty file is text.
fn looks_like_text(path: &std::path::Path) -> bool {
    let Ok(mut f) = open_file(path) else {
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

#[cfg(test)]
mod tests;
