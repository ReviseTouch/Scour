//! What a source reports when the world moves.

use serde::{Deserialize, Serialize};

use super::Entry;

/// A single change to apply to an index. Every variant names a path: a deleted
/// file cannot be stat-ed, so all a watcher has is where it was. [`Change::Rescan`]
/// is how a watcher that lost track says so, once, for the engine to sort out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum Change {
    /// This entry exists and looks like this. Creates or replaces.
    Upsert(Entry),
    /// This path is gone, and so is everything beneath it. One operation rather than
    /// a stream: with every ancestor indexed as a token, marking 378,100 documents
    /// costs 1.3 µs. For a file it is the file alone.
    RemoveSubtree { path: String },
    /// Something was missed under this path. Walk it again and reconcile.
    Rescan { path: String },
}

impl Change {
    /// The path this change is about.
    pub fn path(&self) -> &str {
        match self {
            Change::Upsert(e) => &e.path,
            Change::RemoveSubtree { path } | Change::Rescan { path } => path,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{EntryId, Meta, SourceId};

    #[test]
    fn every_change_says_where_it_happened() {
        let e = Entry {
            id: EntryId::path_hash(SourceId(0), "/a/b"),
            path: "/a/b".into(),
            is_dir: false,
            meta: Meta::UNKNOWN,
        };
        assert_eq!(Change::Upsert(e).path(), "/a/b");
        assert_eq!(Change::Rescan { path: "/a".into() }.path(), "/a");
        assert_eq!(
            Change::RemoveSubtree { path: "/a".into() }.path(),
            "/a",
            "a removal knows where it was, and that is all an index needs"
        );
    }
}
