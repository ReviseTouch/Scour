//! What runs the thing.
//!
//! Sources produce entries and changes; an index answers questions about them.
//! This crate is what puts the two together, and its most important property is
//! what it does not know: it is handed a `Box<dyn Source>` and an
//! `Arc<dyn Index>` and has no way of discovering that one of them is a
//! filesystem or that the other is tantivy. Choosing those is one function call
//! in `scourd`, and swapping either is a one-line change there.
//!
//! Three pieces of behaviour live here because they belong to neither side:
//!
//! * **Commits are batched.** A commit costs tens of milliseconds, so
//!   committing per change would make a `git checkout` unusable. Changes
//!   accumulate for about a second. Removals are exempt — they take effect
//!   immediately, because a deleted file that is still listed is the more
//!   annoying of the two failures.
//! * **A rescan reconciles rather than adds.** A scan reports what it found;
//!   the interesting part is what it did not. Entries are stamped with the
//!   pass that saw them and anything older under the scanned subtree is swept.
//! * **`Change::Rescan` is honoured.** When a watcher says it lost track, the
//!   subtree is walked again. Every platform loses track differently, and this
//!   is the one place that has to care.

mod engine;
mod reconcile;
mod tree;

pub use engine::{Engine, EngineOptions, Explained};
