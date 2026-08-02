//! A fake filesystem, and the truth to check an index against.
//!
//! Two things live here, and the second is why the first exists.
//!
//! [`generate`] builds a tree shaped like a real one. Benchmarks are worth what
//! their data is worth, and uniform random names and timestamps flatter every
//! structure: they compress badly, spread evenly across any sort order, and
//! never produce the case that hurts. Real filesystems are lumpy. The real
//! index measured **571,334 files sharing only 4,977 distinct modification
//! times** — 115 files per timestamp, because package installs, git checkouts
//! and archive extractions stamp thousands of files at the same instant. So
//! that is what this generates, along with deep repetitive directories and one
//! enormous old tree planted specifically to break anything that walks
//! newest-first and stops early.
//!
//! [`brute_force`] answers a query by looking at every entry. It is the
//! reference: slow, obviously correct, and the only reason three real bugs in
//! the index were ever found. All three were invisible to a benchmark, because
//! all three returned a *fast* wrong answer.

mod generate;
mod reference;

pub use generate::{MockFs, MockOptions, Rng, describe, generate};
pub use reference::{brute_force, matches};
