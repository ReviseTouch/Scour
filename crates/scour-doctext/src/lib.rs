//! The words of documents the in-process readers cannot stream: PDF, the
//! legacy Office formats and e-mail. Bytes in, plain text out, no file opened
//! here — the caller reads the file under its own identity and bounds. And a
//! word processor's document as it flows in a preview, from the same library.

mod flow;
mod format;
mod odf;
mod text;

pub use flow::flow;
pub use format::Format;
pub use text::{Text, text};
