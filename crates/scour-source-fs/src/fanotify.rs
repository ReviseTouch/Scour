//! Watching a whole filesystem with one mark.
//!
//! `FAN_MARK_FILESYSTEM` costs one mark a superblock: 0.005 ms and no measurable
//! kernel memory. Events merge — a create, write, close and delete arrive as one
//! mask — so the mask is never a description; a path is stated instead.

mod events;
mod handle;
mod mounts;
mod names;
mod watch;
use events::*;
use handle::*;
use mounts::*;
use names::*;
pub use watch::*;

#[cfg(test)]
mod tests;
