//! The local filesystem, as a source.
//!
//! One implementation for Linux, Windows and macOS, written against portable
//! APIs only. That is a deliberate starting point rather than a limitation:
//! the platform-specific fast paths that a file indexer eventually wants —
//! reading NTFS's `$MFT` and its USN journal, Linux's `fanotify`, macOS's
//! `getattrlistbulk` and the FSEvents history — each need privileges, each
//! need their own testing, and each go behind this same [`Source`] trait when
//! they arrive. Nothing above has to change to accept them, because nothing
//! above knows this one exists.
//!
//! What the portable version gives up is worth naming. It walks directories
//! rather than reading a volume's metadata wholesale, so the first scan is
//! minutes rather than seconds on a large disk. It watches through `notify`,
//! which on Linux costs one inotify watch per directory and runs out — the
//! answer to that is [`Change::Rescan`], not a bigger number.
//!
//! [`Source`]: scour_core::Source
//! [`Change::Rescan`]: scour_core::Change::Rescan

/// One mark a filesystem instead of one watch a directory. Linux only, and
/// only when a privileged helper has handed the descriptor over — otherwise
/// [`watch`] falls back to inotify and nothing in it runs.
#[cfg(target_os = "linux")]
mod fanotify;
mod pulse;
mod rules;
mod scan;
mod watch;

pub mod fs;
mod path;

pub use rules::{Rules, platform_defaults};
pub use scan::FsSource;
