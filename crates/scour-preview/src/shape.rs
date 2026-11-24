//! What a file is, for showing: by its name, then by its first bytes.

use super::*;
use std::io::Read;

/// How much is read to decide whether a file is text at all.
pub(super) const SNIFF: usize = 8 * 1024;

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
        && let Ok(file) = open_file(path)
        && let Some((head, cut)) = scour_extract::document_text(path, file, DOCUMENT_TEXT)
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
pub(super) const DRAWN: &[(&str, Shape)] = &[
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

/// How much of a document's text a preview shows.
const DOCUMENT_TEXT: usize = 128 * 1024;
