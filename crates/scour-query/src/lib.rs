//! The query language: text in, an [`Ast`](scour_core::Ast) out.
//!
//! Nothing here evaluates the tree, so the same syntax means the same thing in a
//! search box, in `scour search`, and over MCP. The shape is Everything's:
//! whitespace is AND, `|` OR, `!` NOT, quotes a phrase, `*`/`?` wildcards, `field:value`.

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
pub use highlight::{complete, spans, spans_at, without, without_at};
pub use parse::{parse, parse_at};
pub use syntax::SYNTAX;
pub use time::now_secs;
