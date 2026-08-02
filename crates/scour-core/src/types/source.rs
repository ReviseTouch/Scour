//! What a source is, and what it can do.

use bitflags::bitflags;
use serde::{Deserialize, Serialize};

use super::SourceId;

bitflags! {
    /// What a source is capable of.
    ///
    /// Capabilities are declared rather than assumed so that the engine can
    /// adapt without knowing what it is talking to. Nothing above a source ever
    /// asks "is this NTFS"; it asks whether there is a journal, whether ids
    /// survive renames, whether one watch covers a subtree.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Caps: u32 {
        /// Can report changes as they happen.
        const WATCH = 1 << 0;
        /// One watch covers a whole subtree.
        ///
        /// True on Windows and macOS, false on Linux, where inotify needs one
        /// watch per directory and runs out. The difference decides whether
        /// the engine walks the tree to install watches or not — which is the
        /// single largest piece of platform-shaped code in a naive design.
        const RECURSIVE_WATCH = 1 << 1;
        /// Has a durable change journal that can be replayed from a stored
        /// position after a restart: NTFS's USN journal, macOS's FSEvents
        /// history. A source with this can catch up without a full rescan.
        const JOURNAL = 1 << 2;
        /// Entry ids survive a rename or a move, so a move can be an update
        /// rather than a delete plus an add.
        const STABLE_IDS = 1 << 3;
        /// `open` works, so content can be extracted.
        const CONTENT = 1 << 4;
        /// Paths differ by case. Affects deduplication, not matching — search
        /// is case-folded either way.
        const CASE_SENSITIVE = 1 << 5;
    }
}

/// What kind of thing is behind a source. Frontends use it to pick an icon and
/// to explain latency; the engine uses it for nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Local,
    Removable,
    Network,
    Cloud,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    pub id: SourceId,
    /// Stable machine name, used in configuration and on the wire.
    pub name: String,
    pub kind: SourceKind,
    pub roots: Vec<String>,
    pub caps: Caps,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanOptions {
    /// Include entries the platform considers hidden.
    pub hidden: bool,
    pub follow_symlinks: bool,
    /// Skip the per-entry `stat`.
    ///
    /// A directory read already knows each name and whether it is a directory;
    /// everything else costs an extra syscall per entry. Scanning without it
    /// and filling the rest in afterwards gets a usable index far sooner on a
    /// cold cache.
    pub skip_metadata: bool,
    /// Worker threads; zero means "decide from the hardware".
    pub threads: usize,
    /// Prefix paths to skip entirely.
    pub exclude_paths: Vec<String>,
    /// Directory names to skip anywhere they appear.
    pub exclude_dirs: Vec<String>,
    /// File names to skip anywhere they appear.
    pub exclude_files: Vec<String>,
    /// Paths that override the exclusions above.
    pub allow: Vec<String>,
    /// Restrict the walk to this subtree instead of the source's roots.
    pub subtree: Option<String>,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            hidden: true,
            follow_symlinks: false,
            skip_metadata: false,
            threads: 0,
            exclude_paths: Vec::new(),
            exclude_dirs: Vec::new(),
            exclude_files: Vec::new(),
            allow: Vec::new(),
            subtree: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ScanReport {
    pub entries: u64,
    pub dirs: u64,
    /// Entries skipped by an exclusion rule.
    pub excluded: u64,
    /// Directories that could not be read. Permission, mostly.
    pub unreadable: u64,
    pub took_ms: u64,
    /// True when the walk stopped early because the sink asked it to.
    pub cancelled: bool,
}
