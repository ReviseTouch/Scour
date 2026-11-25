//! The words of documents the in-process readers cannot stream: PDF, the
//! legacy Office formats and e-mail. Bytes in, plain text out, no file opened
//! here — the caller reads the file under its own identity and bounds.

mod format;
mod text;

pub use format::Format;
pub use text::{Text, text};
