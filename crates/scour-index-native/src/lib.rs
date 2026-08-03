//! An index written for this workload, and for no other.
//!
//! The workload is narrow enough to be worth naming: about a million entries,
//! each a file name, a directory, and eleven numbers. No body text. Queries
//! are substrings of the name, plus filters on those numbers, returning forty
//! rows ordered by one of them. Entries change constantly.
//!
//! A general search engine carries machinery for a different problem —
//! relevance, phrases, stemming, positions — and that machinery is what costs.
//! Measured on a real home directory of 1,209,503 entries, through the same
//! service and the same command line as tantivy: **60.6** bytes an entry here
//! against **186.6**, a first scan of 7.5 seconds against 34.5, a rebuild of
//! 2.5 seconds against forty, and about 100 MB resident against 203.
//!
//! A page of forty counting to five hundred — what a search box asks for —
//! answers in **0.26 to 3.2 ms** across every query measured, including the
//! two-character term a trigram index has to refuse outright. See
//! `docs/MEASUREMENTS.md`.
//!
//! ## Where the space goes, and why
//!
//! Three measurements shaped the layout, all taken on the real corpus rather
//! than guessed at:
//!
//! * **A path is mostly its directory.** 631,008 entries live in 73,434
//!   directories — 8.6 files each. Storing the directories once, front-coded,
//!   and giving each entry a number into that table costs **3.63 bytes** where
//!   the raw path costs 117.7. It also makes renaming a folder a change to one
//!   row, which is the operation a search engine is worst at.
//! * **The eleven numbers cost 8.85 bytes together**, bit-packed in blocks
//!   against a per-block minimum. `mtime` alone costs 0.29, because the rows
//!   are already in its order. Delta-coding them makes it *worse* — 23.4 — so
//!   it is not done.
//! * **The row order is the sort order.** Rows are stored newest-first, so
//!   answering "the newest forty" means reading the first forty that match and
//!   stopping. This is the whole design; everything else follows from it.
//!
//! ## What is deliberately absent
//!
//! There is no inverted index over rows here. Names are scanned. That sounds
//! wrong until the arithmetic: the name arena is 17.6 MB at 631,008 entries,
//! and a linear scan of memory runs at gigabytes a second.
//!
//! What the measurement then said is that the arithmetic is right for most
//! queries and wrong for one — a term matching fifteen files out of a million
//! has no page to fill and no count to reach, so the walk runs to the end. That
//! is what [`trigram`] narrows, and it narrows *blocks of rows*: it says where
//! to look and never what the answer is, so it can be slow and cannot be
//! wrong.
//!
//! It is also the reason this is worth writing at all. An index can be
//! silently wrong; a scan can only be slow. Three bugs in the engine this
//! replaces returned fast, wrong answers, and none of them were visible to a
//! benchmark.
//!
//! [`trigram`]: crate::TrigramIndex

mod build;
mod columns;
mod dirs;
mod ids;
mod index;
mod names;
mod search;
mod segment;
mod trigram;
pub(crate) mod varint;

pub use build::{SegmentBytes, build, build_sorted};
pub use columns::{ColumnBlocks, ColumnWriter, Field};
pub use dirs::{DirScope, DirTable, DirWriter};
pub use ids::{IdMap, IdWriter, digest};
pub use index::NativeIndex;
pub use names::{Folded, NameArena, NameWriter};
pub use search::{Found, Plan, Segment, Wanted, run, run_with};
pub use segment::Live;
pub use trigram::{TrigramIndex, TrigramWriter};
