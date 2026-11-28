//! What Scour does with a file, by its extension: how the preview shows it,
//! whether its pages can be made, whether it is drawn as it flows, where its
//! words are read from, and what a content search makes of it inside a zip.
//! One row per extension, in [`FORMATS`]; every place that used to keep a
//! list of its own asks here.
//!
//! **A new format is a row.** What reads it is still code — a reader in
//! `scour-extract` or `scour-doctext`, a drawing in `scour-flow` — but which
//! extensions reach that code, and what the other places do with them, is
//! decided here once.
//!
//! What a file *is* for a search — a document, an image, code — is not here:
//! that is `scour-core`'s kind table, tuned on a measured corpus and read by
//! the query language.

mod table;
#[cfg(test)]
mod tests;

pub use table::FORMATS;

/// How the preview shows a file of this format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    /// By its bytes: text if they look like text, nothing otherwise. Every
    /// extension not in the table is this.
    Sniffed,
    /// Whole, by the face that draws it: a picture.
    Whole,
    /// Streamed, and read by the face as it needs it: a PDF, audio, video.
    Streamed,
}

/// Where a content search, and a document's text in the preview, read its
/// words from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Read {
    /// Its bytes, as text.
    Bytes,
    /// A reader in this process, which takes the parts a zip holds or the
    /// markup out (`scour-extract`).
    Here,
    /// The document reader in its own process (`scour-extractor`), under the
    /// name given; the file's first bytes decide in the end.
    Extractor(&'static str),
}

/// One extension, and what is done with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
    /// Lowercase, without its dot.
    pub ext: &'static str,
    /// The extension whose reader reads this one: a macro-enabled document or
    /// a template is its document's container, read the same way. Itself,
    /// for most.
    pub like: &'static str,
    /// The media type a face is told, or a thumbnailer is looked up by.
    pub mime: Option<&'static str>,
    pub shown: Shown,
    /// A type a desktop's thumbnailers are asked for when its own
    /// declarations leave it out.
    pub thumb: bool,
    /// A picture the window decodes itself, rather than asking for one.
    pub direct: bool,
    /// Its pages are made by LibreOffice or Microsoft Office, where that is
    /// allowed (`scour_preview::office`).
    pub converts: bool,
    /// Drawn as it flows, sheets as grids and slides as cards (`scour-flow`).
    pub flows: bool,
    pub read: Read,
    /// A zip a content search opens, inside another zip too.
    pub zip: bool,
    /// Markup whose words are between its tags, read so inside a zip.
    pub markup: bool,
    /// No words in its bytes: inside a zip a content search passes it by
    /// unless asked to read binary.
    pub binary: bool,
}

impl Format {
    /// Shown whole or streamed, by its extension alone: a face draws it.
    pub fn drawn(&self) -> bool {
        matches!(self.shown, Shown::Whole | Shown::Streamed)
    }

    /// Its words are read by the document reader in its own process.
    pub fn handed_over(&self) -> bool {
        matches!(self.read, Read::Extractor(_))
    }
}

/// The row for an extension, any case, without its dot.
pub fn of(ext: &str) -> Option<&'static Format> {
    of_bytes(ext.as_bytes())
}

/// The same, for an extension as bytes: a name inside a zip.
pub fn of_bytes(ext: &[u8]) -> Option<&'static Format> {
    // Longer than any row: not worth folding.
    if ext.is_empty() || ext.len() > 12 {
        return None;
    }
    FORMATS
        .binary_search_by(|f| {
            f.ext
                .bytes()
                .cmp(ext.iter().map(|b| b.to_ascii_lowercase()))
        })
        .ok()
        .map(|i| &FORMATS[i])
}

/// The row for a path's extension.
pub fn of_path(path: &std::path::Path) -> Option<&'static Format> {
    of(path.extension()?.to_str()?)
}

/// The table as `scour formats` prints it: a row per extension, a column per
/// question. Pages are made only where that is allowed — Windows and macOS,
/// with LibreOffice or Microsoft Office there.
pub fn table() -> String {
    let mut out = format!(
        "{:<9}{:<10}{:<7}{:<7}{:<15}{:<9}{}\n",
        "ext", "preview", "pages", "flows", "words", "in a zip", "thumbnail"
    );
    for f in FORMATS {
        let preview = match f.shown {
            Shown::Sniffed => "bytes",
            Shown::Whole => "whole",
            Shown::Streamed => "streamed",
        };
        let yes = |on: bool| if on { "yes" } else { "—" };
        let words = match f.read {
            Read::Bytes => "bytes".to_owned(),
            Read::Here => "here".to_owned(),
            Read::Extractor(name) => format!("extractor:{name}"),
        };
        let zip = if f.zip {
            "open"
        } else if f.markup {
            "markup"
        } else if f.binary {
            "skip"
        } else {
            "read"
        };
        let thumb = if f.thumb { f.mime.unwrap_or("") } else { "—" };
        out.push_str(&format!(
            "{:<9}{:<10}{:<7}{:<7}{:<15}{:<9}{}\n",
            f.ext,
            preview,
            yes(f.converts),
            yes(f.flows),
            words,
            zip,
            thumb
        ));
    }
    out
}
