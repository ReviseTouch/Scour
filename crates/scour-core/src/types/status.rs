//! What the service is doing, and what a directory looks like.

use serde::{Deserialize, Serialize};

use super::{Kind, SourceId};

/// A snapshot of the running service. Every field is a number or a flag: several
/// frontends describe the same state, and only one of them chooses the words.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Status {
    pub scanning: bool,
    /// Which source, when one is being scanned.
    pub scanning_source: Option<SourceId>,
    /// Entries seen by the scan currently running.
    pub scanned: u64,
    /// How long the last completed scan took.
    pub last_scan_ms: u64,
    /// Changes accepted but not yet committed. Non-zero is normal — commits cost
    /// tens of milliseconds and are batched — growing without bound is not.
    pub pending: u64,
    /// Sources currently being watched for changes.
    pub watching: u32,
    pub sources: u32,
    pub entries: u64,
    pub index_bytes: u64,
    /// Entries outside the ordered part of the index. Every query reads all of them,
    /// which is what [`Status::rebuild_advised`] is decided from.
    pub unsorted: u64,
    pub rebuild_advised: bool,
    /// The index has never been built.
    pub cold: bool,
    /// Commits that have failed in a row. Non-zero means the changes are still in
    /// memory and the index on disk is behind; the count says blip or wall.
    pub unwritten: u32,
    /// What the index would answer, as a number: it moves whenever a repeated search
    /// could come back different. Compared with `!=`, never `>`, since a restarted
    /// service starts from zero. `Await` returns when it differs from a client's.
    pub revision: u64,
}

/// One node of a directory listing, answered from the index rather than the
/// filesystem: `children` is a count the index already holds, not a read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TreeNode {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
    pub kind: Kind,
    pub size: i64,
    pub mtime: i64,
    /// Entries directly inside, for a directory. `0` for a file.
    pub children: u64,
    /// Deeper levels, when they were asked for.
    pub nodes: Vec<TreeNode>,
    /// More entries exist at this level than were returned.
    pub truncated: bool,
}
