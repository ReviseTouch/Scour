//! Asking the filesystem what it can actually promise.
//!
//! The questions themselves — medium, disk, case, modes — are `surefoot`'s;
//! what is here is what this service does with the answers.

pub use surefoot::probe::{FsTraits, Medium, disk_of, medium_of, traits_of};

/// What a medium means to the scan and to the watcher.
pub trait MediumExt {
    /// How many walker threads this mount is worth — capped by the one indexing
    /// consumer, not the device: `/mnt/depo`'s 1,565,781 entries cost 13.3 core-s at
    /// two and 176.3 at twenty. Spinning is measured (docs/MEASUREMENTS.md); network is a guess.
    fn threads(self, cores: usize) -> usize;

    /// How long to let filesystem events settle before acting on them.
    /// A network mount reports late and in bursts, and each reaction is a round trip.
    fn debounce_ms(self) -> u64;
}

impl MediumExt for Medium {
    fn threads(self, cores: usize) -> usize {
        match self {
            Medium::Solid | Medium::Memory => 2,
            // No wall time from threads under a seek limit; a read's p90 goes 160 → 820 ms from 1 to 8.
            Medium::Spinning => 1,
            // Some concurrency hides round trips; too much floods an unseen link.
            Medium::Network => 4,
            Medium::Unknown => (cores / 2).clamp(2, 8),
        }
    }

    fn debounce_ms(self) -> u64 {
        match self {
            Medium::Solid | Medium::Memory => 200,
            Medium::Spinning => 500,
            Medium::Network => 5_000,
            Medium::Unknown => 500,
        }
    }
}
