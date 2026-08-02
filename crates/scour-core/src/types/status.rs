//! What the service is doing, and what a directory looks like.

use serde::{Deserialize, Serialize};

use super::{Kind, SourceId};

/// A snapshot of the running service.
///
/// Every field is a number or a flag. Nothing here is a sentence, because
/// three frontends in several languages have to describe the same state and
/// only one of them should be choosing the words.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Status {
    /// A scan is running.
    pub scanning: bool,
    /// Which source, when one is being scanned.
    pub scanning_source: Option<SourceId>,
    /// Entries seen by the scan currently running.
    pub scanned: u64,
    /// How long the last completed scan took.
    pub last_scan_ms: u64,
    /// Changes accepted but not yet committed.
    ///
    /// Non-zero is normal: commits are batched because they cost tens of
    /// milliseconds each. Growing without bound is not.
    pub pending: u64,
    /// Sources currently being watched for changes.
    pub watching: u32,
    /// Sources configured.
    pub sources: u32,
    pub entries: u64,
    pub index_bytes: u64,
    /// Entries outside the ordered part of the index.
    ///
    /// Every query reads all of them, so this is what decides when a rebuild
    /// is due — and [`Status::rebuild_advised`] says when it is.
    pub unsorted: u64,
    pub rebuild_advised: bool,
    /// The index has never been built.
    pub cold: bool,
}

/// One node of a directory listing.
///
/// Answered from the index rather than the filesystem, which is what makes it
/// instant and what makes it usable on a directory holding a million files:
/// `children` is a count the index already knows, not a read.
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
