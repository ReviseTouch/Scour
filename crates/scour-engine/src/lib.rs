//! What runs the thing: sources produce entries and changes, an index answers
//! questions about them, and this crate is what joins the two.
//!
//! It is handed a `Box<dyn Source>` and an `Arc<dyn Index>` and has no way to
//! discover that either is a filesystem or a tantivy index; `scourd` picks both.

mod engine;
mod reconcile;
mod tree;

pub use engine::{Engine, EngineOptions, Explained};
