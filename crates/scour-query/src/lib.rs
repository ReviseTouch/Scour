//! The query language.
//!
//! Text goes in and an [`Ast`] comes out. Nothing here knows what will evaluate
//! that tree — that is the whole reason the language and the engine are
//! separate crates. The same syntax therefore means the same thing whether it
//! was typed into a search box, passed to `scour search`, or handed to a model
//! through the MCP server.
//!
//! The shape is Everything's, because people who want this tool already know
//! that syntax and because it has held up: whitespace is AND, `|` is OR, `!` is
//! NOT, quotes make a phrase, `*` and `?` are wildcards, and `field:value`
//! narrows. [`SYNTAX`] is the reference text, kept next to the parser so the
//! two cannot drift, and served verbatim to language models.
//!
//! [`Ast`]: scour_core::Ast

mod describe;
mod fields;
mod glob;
mod highlight;
mod parse;
mod syntax;
mod time;

pub use describe::describe;
pub use fields::{FIELDS, Field, Takes};
pub use glob::glob_matches;
pub use highlight::{complete, spans, spans_at};
pub use parse::{parse, parse_at};
pub use syntax::SYNTAX;
pub use time::now_secs;
