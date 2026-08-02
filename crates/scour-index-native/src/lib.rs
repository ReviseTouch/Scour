//! An index written for this workload, and for no other.
//!
//! The workload is narrow enough to be worth naming: about a million entries,
//! each a file name, a directory, and eleven numbers. No body text. Queries
//! are substrings of the name, plus filters on those numbers, returning forty
//! rows ordered by one of them. Entries change constantly.
//!
//! A general search engine carries machinery for a different problem —
//! relevance, phrases, stemming, positions — and that machinery is what costs.
//! Measured on the same 1,197,514-entry home directory, through the same
//! service and the same command line: **49.5** bytes an entry here against
//! **186.6** in tantivy, a first scan of 6.7 seconds against 34.5, a rebuild of
//! four seconds against forty, and 113 MB resident against 203.
//!
//! It loses one thing, and it is the thing an inverted index is for: a
//! *selective* substring. `rapor` matches fifteen files out of 1.2 million, so
//! there is no page to fill and the walk runs to the end — 28.9 ms against 1.7.
//! Everything that filters on a number, a scope or an extension is two to eight
//! times faster here, and a two-character term is answerable at all, which a
//! trigram index cannot do. See `docs/MEASUREMENTS.md`.
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
//! There is no inverted index here. Names are scanned. That sounds wrong until
//! the arithmetic: the name arena is 17.6 MB at 631,008 entries, and a linear
//! scan of memory runs at gigabytes a second — so a query that cannot stop
//! early costs single-digit milliseconds, and one that can costs nothing.
//!
//! It is also the reason this is worth writing at all. An index can be
//! silently wrong; a scan can only be slow. Three bugs in the engine this
//! replaces returned fast, wrong answers, and none of them were visible to a
//! benchmark.
//!
//! A trigram layer can be added later without changing any of these files —
//! it adds two more and turns step three of the search into "start from the
//! candidates" instead of "start from row zero".

mod build;
mod columns;
mod dirs;
mod ids;
mod index;
mod names;
mod search;
mod segment;
pub(crate) mod varint;

pub use build::{SegmentBytes, build, build_sorted};
pub use columns::{ColumnBlocks, ColumnWriter, Field};
pub use dirs::{DirScope, DirTable, DirWriter};
pub use ids::{IdMap, IdWriter, digest};
pub use index::NativeIndex;
pub use names::{Folded, NameArena, NameWriter};
pub use search::{Found, Plan, Segment, Wanted, run, run_with};
pub use segment::Live;
