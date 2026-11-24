//! What can be shown of a file: text, image, audio, video, PDF, or nothing.
//!
//! Input is a path and whether it is a directory; nothing else about Scour is
//! known here. The caller owns two things: whether the path may be read at all
//! (nothing here checks) and the response headers.

mod helpers;
mod read;
mod shape;
pub use helpers::*;
pub use read::*;
pub use shape::*;

pub mod doc;
pub mod office;
mod pdf_macos;
mod pdf_win;
mod ppm;

#[cfg(test)]
mod tests;
