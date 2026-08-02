//! Turning a file's bytes into text an index can hold.
//!
//! Nothing in this workspace implements this yet, and that is deliberate. The
//! trait exists now so that the shape of everything around it — the schema
//! field, the `content:` query term, the configuration table, the error variant
//! for asking a content-free index about contents — is settled while it is
//! cheap to settle. Adding a PDF reader later should be a new crate and a
//! cargo feature, not a change to the index format and the query language.
//!
//! Two constraints are already known and should be honoured by the first
//! implementation:
//!
//! * **Extractors run untrusted input through parsers.** Every one of them will
//!   eventually panic or hang on a malformed file. They belong behind a
//!   timeout and a memory bound, or in a separate process — not inline in a
//!   daemon that must stay responsive.
//! * **Never trust the extension.** Sniff the leading bytes.

use std::fmt::Debug;
use std::io::Read;

use crate::types::Result;

/// What is known about a file before opening it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaHint {
    /// Case-folded extension without the dot. May be empty.
    pub ext: String,
    /// MIME type, if something has already sniffed it.
    pub mime: Option<String>,
    pub size: i64,
}

/// Where extracted text goes.
///
/// A sink rather than a returned `String` because a large document should be
/// streamed into the index instead of assembled in memory first, and because
/// an extractor that finds structure — headings, tables, page numbers — should
/// be able to say so without every caller having to care.
pub trait ContentSink {
    fn text(&mut self, chunk: &str);

    /// Optional structure. The default implementation ignores it, so an
    /// extractor can always emit it and a simple index can always ignore it.
    fn heading(&mut self, text: &str) {
        self.text(text);
    }

    /// A page or section boundary, for locating a hit later.
    fn boundary(&mut self, _ordinal: u32) {}
}

pub trait Extractor: Send + Sync + Debug {
    /// Stable machine name, for configuration and diagnostics.
    fn name(&self) -> &str;

    fn accepts(&self, hint: &MediaHint) -> bool;

    fn extract(&self, input: &mut dyn Read, out: &mut dyn ContentSink) -> Result<()>;
}
