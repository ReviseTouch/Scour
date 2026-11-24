//! The text inside a file. A ZIP's directory and entries read under bounds,
//! an entry inflated, XML made plain, RTF read through its code page, and a
//! document's text part by part — one reader for everything that searches
//! or shows what is inside a file.

mod codepage;
mod document;
mod inflate;
mod normal;
mod rtf;
mod xml;
pub mod zip;

pub use codepage::*;
pub use document::*;
pub use inflate::*;
pub use normal::*;
pub use rtf::*;
pub use xml::*;
