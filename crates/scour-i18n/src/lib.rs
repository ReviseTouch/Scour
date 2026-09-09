//! Translation: gettext catalogues compiled into the binary and keyed by the
//! English text itself, so an untranslated string degrades to correct English.
//! Every face looks strings up here rather than in its own toolkit, so a window
//! with no `.mo` files beside it is still translated. Nothing a machine reads is
//! translated — error codes, query fields, MCP tool descriptions are contract.

mod catalogue;
mod po;

pub use catalogue::{Catalogue, LANGUAGES, choose, system_language};
