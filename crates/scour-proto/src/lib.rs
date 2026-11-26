//! What a client and the service say to each other: types and nothing else — no
//! socket, no threads, no serialisation format chosen.
//!
//! Queries cross as **text**, not as a parsed tree. The service parses, so the
//! language cannot come to mean three things in three callers.

mod call;
mod disks;
mod grep;
mod request;
mod resources;
mod response;
pub use call::*;
pub use disks::*;
pub use grep::*;
pub use request::*;
pub use resources::*;
pub use response::*;

/// What a disk is, named here so a face that reads a [`Disk`] needs no other crate.
pub use scour_volumes::{Kind as VolumeKind, Volume};

#[cfg(test)]
mod tests;
