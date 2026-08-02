//! The search index, on tantivy.
//!
//! Every decision here came out of a measurement, and the ones that look odd
//! are the ones that were measured hardest. In order of how much they matter:
//!
//! * **Documents are stored newest-first.** Document order is then date order,
//!   so the default view can walk the postings and stop at the first page
//!   instead of visiting every match: 14.8 ms became 0.022 ms.
//! * **That order can only be rebuilt, never maintained.** Tantivy appends in
//!   arrival order and its merges concatenate segments. So the index is two
//!   things at once — a **sorted body** written by a rebuild, and an
//!   **unsorted tail** of everything indexed since. The body is walked with
//!   early termination, the tail is read in full, and the two are merged. A
//!   rebuild folds the tail back in.
//! * **Automatic merging is off.** It would silently concatenate segments and
//!   destroy the ordering the whole design rests on. Compaction is the rebuild.
//! * **Displayed text lives in the document store, not in columns.** A
//!   dictionary-encoded string column materialises a row in 14.33 µs; the
//!   document store does it in 0.32 µs. A column is right for what you *sort*
//!   on and wrong for what you only *show*.
//! * **Every ancestor directory is a token**, so deleting a subtree is one
//!   term: 378,100 documents marked in 1.3 µs.
//! * **Counts stop.** With early termination an exact total is the only work
//!   left that grows with the number of hits.
//!
//! Three bugs found while building this were invisible to timing and only
//! caught by comparing against brute force, which is why the tests still do:
//! a cap that bounded the count's value but not its work; a tail cut at page
//! size, so an unsorted segment contributed its *oldest* matches; and deleted
//! documents reappearing, because `Weight::scorer` yields them and it is the
//! collector — which early termination bypasses — that consults the alive
//! bitset.

mod engine;
mod lower;
mod schema;

pub use engine::TantivyIndex;
pub use schema::{IndexOptions, PositionalTrigram};
