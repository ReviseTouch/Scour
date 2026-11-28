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
    /// category is derived from the type, so the table cannot name the wrong bucket.
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
    /// Where `head` starts when it was asked for around a place: the byte, and
    /// the line that byte is on, counted from one. Zero for the head.
    #[serde(default)]
    pub from: u64,
    #[serde(default)]
    pub first_line: u64,
    /// A word processor's document that can be drawn as it flows, with its
    /// paragraphs, tables and pictures: asked for with `Request::Flow`. The
    /// text in `head` stands until it comes. Set by the service, which holds
    /// the reader.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub flow: bool,
}

/// [`look_at`], and when it is text, the stretch around a place in it rather
/// than its head: `offset`, a byte, or else `line`. See [`crate::text_near`].
pub fn look_near(
    path: &std::path::Path,
    is_dir: bool,
    offset: Option<u64>,
    line: Option<u64>,
) -> Look {
    let mut look = look_at(path, is_dir);
    if look.shape == "text"
        && (offset.is_some() || line.is_some_and(|l| l > 1))
        && shape_of(path, is_dir) == Shape::Text
        && let Ok(near) = crate::text_near(path, offset, line)
    {
        look.head = near.text;
        look.cut = near.cut;
        look.len = near.len;
        look.from = near.from;
        look.first_line = near.first_line;
    }
    look
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

/// A format a browser draws, by extension and not by content: the browser
/// decides by the type header, so sniffing would let anything claim to be one.
/// Which they are is `scour_formats`' table. SVG among them is safe only with
/// the caller's `sandbox` header: an `<img>` never runs an SVG's script, and
/// the sandboxed opaque origin stops a direct tab too.
pub(super) fn drawn(f: &scour_formats::Format) -> Option<Shape> {
    match (f.shown, f.mime) {
        (scour_formats::Shown::Whole, Some(t)) => Some(Shape::Whole(t)),
        // A PDF viewer reads the trailer first, then pages in what it needs;
        // audio and video cannot seek without ranges.
        (scour_formats::Shown::Streamed, Some(t)) => Some(Shape::Streamed(t)),
        _ => None,
    }
}

/// Every format drawn, and how: what the tests hold the written rule to.
#[cfg(test)]
pub(super) fn every_drawn() -> impl Iterator<Item = (&'static str, Shape)> {
    scour_formats::FORMATS
        .iter()
        .filter_map(|f| Some((f.ext, drawn(f)?)))
}

/// How this file should be shown, if at all. A directory is never previewed.
pub fn shape_of(path: &std::path::Path, is_dir: bool) -> Shape {
    if is_dir {
        return Shape::Nothing;
    }
    if let Some(shape) = scour_formats::of_path(path).and_then(drawn) {
        return shape;
    }
    // An Office document is never its raw bytes — RTF would show its markup —
    // but its pages where they are drawn, else its text: [`look_at`] reads it.
    if office::is_office(path) {
        return if office::can_convert() {
            Shape::Streamed("application/pdf")
        } else {
            Shape::Nothing
        };
    }
    // Everything else is decided by the bytes, not by a list of every extension
    // anyone ever gave a text file.
    if looks_like_text(path) {
        Shape::Text
    } else {
        Shape::Nothing
    }
}

/// How a text file is written, as its first bytes say.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Encoding {
    Utf8,
    /// With its byte-order mark: Windows' Notepad wrote these as "Unicode".
    Utf16Le,
    Utf16Be,
    /// Not UTF-8 and no NUL in it, and few control characters: an older
    /// Turkish machine's code page, read as content search reads it.
    Cp1254,
}

/// Is the head of this file text, and in what? A NUL rejects binaries, a
/// byte-order mark says UTF-16, valid UTF-8 is UTF-8, and anything else with
/// almost no control characters is 1254. Before, a file of 1254 showed
/// nothing in the preview, though content search read it (28 Nov).
fn looks_like_text(path: &std::path::Path) -> bool {
    let Ok(mut f) = open_file(path) else {
        return false;
    };
    let mut head = vec![0u8; SNIFF];
    let Ok(n) = f.read(&mut head) else {
        return false;
    };
    head.truncate(n);
    encoding_of(&head).is_some()
}

/// What `head`, a file's first bytes, is written in; None when it is not text.
pub fn encoding_of(head: &[u8]) -> Option<Encoding> {
    if head.starts_with(&[0xFF, 0xFE]) {
        return Some(Encoding::Utf16Le);
    }
    if head.starts_with(&[0xFE, 0xFF]) {
        return Some(Encoding::Utf16Be);
    }
    if head.contains(&0) {
        return None;
    }
    match std::str::from_utf8(head) {
        Ok(_) => return Some(Encoding::Utf8),
        // A character cut by the buffer's edge is not a reason to refuse.
        Err(e) if e.valid_up_to() + 4 >= head.len() => return Some(Encoding::Utf8),
        Err(_) => {}
    }
    let controls = head
        .iter()
        .filter(|&&b| b < 0x20 && !matches!(b, b'\t' | b'\n' | b'\r' | 0x0C | 0x1A))
        .count();
    (controls * 100 <= head.len()).then_some(Encoding::Cp1254)
}

/// Bytes of a file written in `encoding` as text. `from_start`: they begin
/// the file, so a byte-order mark is skipped.
pub fn decode(bytes: &[u8], encoding: Encoding, from_start: bool) -> String {
    let units = |be: bool| {
        let body = if from_start {
            &bytes[2.min(bytes.len())..]
        } else {
            bytes
        };
        let units = body.as_chunks::<2>().0.iter().map(move |&pair| {
            if be {
                u16::from_be_bytes(pair)
            } else {
                u16::from_le_bytes(pair)
            }
        });
        char::decode_utf16(units)
            .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
            .collect()
    };
    match encoding {
        Encoding::Utf8 => String::from_utf8_lossy(bytes).into_owned(),
        Encoding::Cp1254 => scour_extract::cp1254_decode(bytes),
        Encoding::Utf16Le => units(false),
        Encoding::Utf16Be => units(true),
    }
}

/// How much of a document's text a preview shows.
const DOCUMENT_TEXT: usize = 128 * 1024;
