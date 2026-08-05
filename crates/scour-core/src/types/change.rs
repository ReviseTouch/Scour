//! What a source reports when the world moves.

use serde::{Deserialize, Serialize};

use super::Entry;

/// A single change to apply to an index.
///
/// **Every change names a path**, and that is not an accident of the current
/// sources — it is what a watcher can actually say. A file that has just been
/// deleted cannot be stat-ed, so nothing reporting its disappearance knows its
/// inode, its etag, or whatever else it was identified by; all it has is where
/// it was. There used to be a `Remove(EntryId)` here for the other case, and
/// the other case never arrived: nothing in the tree ever emitted it, and
/// [`Change::path`] had to answer `None` for it, which is the mistake showing
/// through the type.
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
    /// This path is gone, and so is everything beneath it.
    ///
    /// One operation rather than a stream of removals because an index can do
    /// it in one step: with every ancestor directory indexed as a token,
    /// marking 378,100 documents was measured at 1.3 µs. For a file it is the
    /// file alone, which is why there is no second, narrower variant.
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
