//! The local filesystem, as a source.
//!
//! Portable APIs only; each platform's fast path — NTFS's `$MFT`, Linux's
//! `fanotify`, macOS's `getattrlistbulk` — goes behind this same `Source` trait.
//! Walking rather than reading volume metadata costs minutes on a first scan.

/// One mark a filesystem instead of one watch a directory. Linux only, and only
/// when a privileged helper has handed the descriptor over.
#[cfg(target_os = "linux")]
mod fanotify;
mod pulse;
/// Looking again at what a mapped write never announced.
mod revisit;
mod rules;
mod scan;
mod watch;

pub mod fs;
mod path;

pub use rules::{Rules, platform_defaults};
pub use scan::FsSource;
