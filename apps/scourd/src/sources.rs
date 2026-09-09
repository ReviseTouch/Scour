//! Which number belongs to which source, across restarts: every row carries a
//! `SourceId`, so a mapping kept beside the index is what makes that number
//! mean the same thing twice. It lives here rather than in the index because a
//! source's name and roots are configuration.
//!
//! The name is the identity: changed roots keep the number, a changed name is
//! a new source and the old one's rows are dropped.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

const FILE: &str = "sources.json";

#[derive(Debug, Default, Serialize, Deserialize)]
struct Map {
    /// Name to the number its rows carry.
    ids: BTreeMap<String, u32>,
}

/// What changed since the last run.
#[derive(Debug, Default)]
pub struct Assigned {
    /// The number each configured source should use, in configuration order.
    pub ids: Vec<u32>,
    /// Numbers whose source is no longer configured.
    pub dropped: Vec<u32>,
}

fn path_of(dir: &Path) -> PathBuf {
    dir.join(FILE)
}

/// Give every configured source the number it had last time, and say which
/// numbers are now orphaned.
///
/// A file that cannot be read is treated as absent: renumbering once, which the
/// caller turns into a rescan, beats a service that will not start over a cache.
pub fn assign(dir: &Path, names: &[String]) -> Assigned {
    let mut map: Map = std::fs::read_to_string(path_of(dir))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();

    let mut next = map.ids.values().copied().max().map_or(0, |m| m + 1);
    let mut ids = Vec::with_capacity(names.len());
    for name in names {
        let id = *map.ids.entry(name.clone()).or_insert_with(|| {
            let id = next;
            next += 1;
            id
        });
        ids.push(id);
    }
    let dropped = map
        .ids
        .iter()
        .filter(|(name, _)| !names.contains(name))
        .map(|(_, id)| *id)
        .collect();
    map.ids.retain(|name, _| names.contains(name));

    if let Ok(json) = serde_json::to_string_pretty(&map) {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(path_of(dir), json);
    }
    Assigned { ids, dropped }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn a_number_survives_the_source_being_moved_in_the_file() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let first = assign(tmp.path(), &names(&["ev", "depo"]));
        assert_eq!(first.ids, vec![0, 1]);
        assert!(first.dropped.is_empty());

        // The same two sources, the other way round.
        let again = assign(tmp.path(), &names(&["depo", "ev"]));
        assert_eq!(again.ids, vec![1, 0], "reordering renumbered the rows");
        assert!(again.dropped.is_empty());
    }

    #[test]
    fn a_source_that_is_removed_is_reported_so_its_rows_can_go() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        assign(tmp.path(), &names(&["ev", "depo", "yedek"]));
        let after = assign(tmp.path(), &names(&["ev", "yedek"]));
        assert_eq!(after.ids, vec![0, 2], "the survivors kept their numbers");
        assert_eq!(after.dropped, vec![1], "nothing would ever sweep these");

        // A number that once meant something else is never reused: rows
        // outlive a commit, and a reused number is a wrong answer.
        let added = assign(tmp.path(), &names(&["ev", "yedek", "yeni"]));
        assert_eq!(added.ids, vec![0, 2, 3]);
    }

    #[test]
    fn a_missing_file_is_a_first_run_rather_than_a_failure() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        std::fs::write(path_of(tmp.path()), b"{ not json").expect("write");
        let a = assign(tmp.path(), &names(&["ev"]));
        assert_eq!(a.ids, vec![0]);
    }
}
