//! What a source reports when the world moves.

use serde::{Deserialize, Serialize};

use super::{Entry, EntryId};

/// A single change to apply to an index.
///
/// The vocabulary is deliberately small, and one member of it is the reason
/// the rest can stay small: [`Change::Rescan`]. Every watching mechanism gives
/// up under load in its own way — inotify runs out of watches, Windows
/// overflows its kernel buffer, FSEvents coalesces, a cloud poll misses a
/// window — and each of them then needs to say "I lost track of this subtree,
/// look again". Naming that case makes it the engine's problem once, instead of
/// each source's problem separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "change", rename_all = "snake_case")]
pub enum Change {
    /// This entry exists and looks like this. Creates or replaces.
    Upsert(Entry),
    /// This entry is gone.
    Remove(EntryId),
    /// This directory and everything beneath it is gone.
    ///
    /// A separate operation rather than a stream of removals because an index
    /// can do it in one step: with every ancestor directory indexed as a token,
    /// marking 378,100 documents was measured at 1.3 µs.
    RemoveSubtree { path: String },
    /// Something was missed under this path. Walk it again and reconcile.
    Rescan { path: String },
}

impl Change {
    /// The path this change is about, when it has one.
    pub fn path(&self) -> Option<&str> {
        match self {
            Change::Upsert(e) => Some(&e.path),
            Change::RemoveSubtree { path } | Change::Rescan { path } => Some(path),
            Change::Remove(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Meta, SourceId};

    #[test]
    fn paths_are_reported_where_they_exist() {
        let e = Entry {
            id: EntryId::path_hash(SourceId(0), "/a/b"),
            path: "/a/b".into(),
            is_dir: false,
            meta: Meta::UNKNOWN,
        };
        assert_eq!(Change::Upsert(e.clone()).path(), Some("/a/b"));
        assert_eq!(Change::Rescan { path: "/a".into() }.path(), Some("/a"));
        assert_eq!(Change::Remove(e.id).path(), None);
    }
}
