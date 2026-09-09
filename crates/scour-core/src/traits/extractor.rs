//! Turning a file's bytes into text an index can hold. Nothing implements this
//! yet; the trait fixes the schema field and the `content:` term in advance.
//!
//! Extractors parse untrusted input: run them under a timeout and a memory
//! bound, and sniff the leading bytes rather than trusting the extension.

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

/// Where extracted text goes: a sink rather than a returned `String`, so a large
/// document streams into the index instead of being assembled in memory.
pub trait ContentSink {
    fn text(&mut self, chunk: &str);

    /// Optional structure. Defaults to plain text, so a simple index may ignore it.
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
