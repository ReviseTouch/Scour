//! The local filesystem, as a source.
//!
//! Portable APIs only; each platform's fast path — NTFS's `$MFT`, Linux's
//! `fanotify`, macOS's `getattrlistbulk` — goes behind this same `Source` trait.
//! Walking rather than reading volume metadata costs minutes on a first scan.

/// One mark a filesystem instead of one watch a directory. Linux only, and only
/// when a privileged helper has handed the descriptor over.
#[cfg(target_os = "linux")]
mod fanotify;
/// Which filesystems that descriptor can hear.
#[cfg(target_os = "linux")]
mod marks;
mod pulse;
/// Looking again at what a mapped write never announced.
mod revisit;
mod rules;
mod scan;
/// Other sources' roots, which a source does not go into.
mod skip;
mod watch;

pub mod fs;
mod path;

#[cfg(target_os = "linux")]
pub use fanotify::keep_from_children;
pub use rules::{Rules, platform_defaults};
pub use scan::FsSource;
