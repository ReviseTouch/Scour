//! An index written for this workload, and for no other: about a million
//! entries of a name, a directory and eleven numbers; substring queries on the
//! name returning forty rows ordered by one number. Rows are stored
//! newest-first, so "the newest forty" is a prefix read. 60.6 bytes an entry
//! against tantivy's 186.6; see `docs/MEASUREMENTS.md`.
//!
//! Names are scanned rather than inverted — the name arena is 17.6 MB at
//! 631,008 entries. [`trigram`] narrows *which blocks of rows* to scan; it
//! never says what the answer is, so it can be slow and cannot be wrong.
//!
//! [`trigram`]: crate::TrigramIndex

mod build;
mod columns;
mod directory_bytes;
mod dirs;
mod durable;
mod extension_order;
mod ids;
mod index;
mod lock;
mod name_order;
mod names;
mod order;
mod rank;
mod search;
mod segment;
mod sizes;
mod trigram;
mod usage;
pub(crate) mod varint;

pub use build::{SegmentBytes, build, build_sorted};
pub use columns::{ColumnBlocks, ColumnWriter, Field};
pub use dirs::{DirScope, DirTable, DirWriter};
pub use extension_order::ExtensionOrder;
pub use ids::{IdMap, IdWriter, digest};
pub use index::NativeIndex;
pub use name_order::NameOrder;
pub use names::{Folded, NameArena, NameWriter};
pub use order::PathOrder;
pub use search::{Found, Plan, Segment, Wanted, run, run_with};
pub use segment::Live;
pub use sizes::Cache as SizeCache;
pub use trigram::{TrigramIndex, TrigramWriter};
