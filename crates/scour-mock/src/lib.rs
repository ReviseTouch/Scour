//! A fake filesystem, and the truth to check an index against. [`generate`]
//! builds a lumpy tree: the real index measured 571,334 files over 4,977 distinct
//! modification times, so timestamps cluster at 115 files each, and one enormous
//! old tree breaks anything walking newest-first that stops early.
//! [`brute_force`] answers a query by looking at every entry.

mod generate;
mod reference;

pub use generate::{MockFs, MockOptions, Rng, describe, generate};
pub use reference::{brute_force, matches};
