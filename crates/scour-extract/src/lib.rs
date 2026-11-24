//! The text inside a file. A ZIP's directory and entries read under bounds,
//! an entry inflated, XML made plain, and a document's text part by part —
//! one reader for everything that searches or shows what is inside a file.

mod document;
mod inflate;
mod xml;
pub mod zip;

pub use document::*;
pub use inflate::*;
pub use xml::*;
