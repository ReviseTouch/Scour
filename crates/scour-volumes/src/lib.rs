//! Which disks this machine has, and what kind each is: built in, plugged in,
//! or on a network. A question about the machine, asked the same way for the
//! service and every face; it knows nothing about an index.

mod classify;
mod look;
mod mountinfo;
mod volume;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(windows)]
mod windows;

pub use classify::{Probe, volumes};
pub use look::look;
pub use mountinfo::{Mount, parse as parse_mountinfo};
pub use volume::{Kind, Volume};
