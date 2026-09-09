//! What a source is, and what it can do.

use bitflags::bitflags;
use serde::{Deserialize, Serialize};

use super::SourceId;

bitflags! {
    /// What a source is capable of. Declared rather than assumed: nothing above a
    /// source asks "is this NTFS", only whether there is a journal or whether one
    /// watch covers a subtree.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    pub struct Caps: u32 {
        /// Can report changes as they happen.
        const WATCH = 1 << 0;
        /// One watch covers a whole subtree; when it is off, the engine walks the
        /// tree to install watches.
        const RECURSIVE_WATCH = 1 << 1;
        /// Has a durable change journal that can be replayed from a stored
        /// position after a restart: NTFS's USN journal, macOS's FSEvents
        /// history. A source with this can catch up without a full rescan.
        const JOURNAL = 1 << 2;
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
    /// Skip the per-entry `stat`. A directory read already knows each name and
    /// whether it is a directory; the rest costs a syscall each, filled in later.
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
    /// Paths that nothing overrides, [`ScanOptions::allow`] included. The index's own
    /// directory needs it: a user rule covering that makes the service index what it
    /// is writing while it writes it, at roughly 60% of two cores.
    pub deny: Vec<String>,
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
            deny: Vec::new(),
            subtree: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
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
    /// The roots this walk is willing to be reconciled against: those it could look
    /// at, on the same filesystem, for the whole walk. Empty is not a missing report
    /// but a walk nothing may be deleted on.
    pub vouched: Vec<String>,
    /// Subtrees the walk could not look inside — ordinary, mostly permissions. Named
    /// rather than counted so a sweep can spare them: their rows are not evidence of
    /// deletion, because the walk never looked.
    pub blind: Vec<String>,
}
