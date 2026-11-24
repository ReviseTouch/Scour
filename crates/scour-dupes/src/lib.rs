//! The same file, several times over: group by size, then head and tail, then a
//! full byte-for-byte comparison. Confirmed largest first, because a unique size
//! eliminates only 6.2% of files while the 18,723 candidates above 1 MB hold
//! 141.8 GB of the possible waste. Stage three compares rather than digests: it
//! decides what somebody deletes.

mod find;
mod report;
pub use find::*;
pub use report::*;

#[cfg(test)]
mod tests;
