//! A set of paths, asked "is anything under you?"
//!
//! Two places need the same answer about the same kind of list, and both of
//! them get the list in the thousands. Deleting a tree reports a path per file
//! and a path per directory, and every one of them has to be checked against
//! every row of an index and against every entry waiting to be written. Done
//! one prefix at a time that is a product, and the product is what a `rm -rf`
//! turned into **2.24 seconds of held write lock** at four thousand paths.
//!
//! Two halves make it cheap.
//!
//! The list is **reduced**: a path under another path in the same list is
//! dropped, because removing or walking the ancestor covers it anyway. On a
//! real delete that is most of them.
//!
//! And the question is asked **from the path rather than from the list**. A
//! path has one parent, one grandparent and so on — a dozen or so ancestors,
//! however many thousand members the set has — so "is any ancestor a member"
//! is a dozen lookups and does not grow with the set at all.
//!
//! The obvious shape, sorting and binary-searching for the greatest member not
//! after the path, is *wrong*, and the test below is what said so. Sort order
//! does not put an ancestor next to its descendant: with `/pkg/lib` and
//! `/pkg/lib-old` both present, `/pkg/lib/deep/f.rs` sorts after `/pkg/lib-old`
//! — because `-` is below `/` — so the one comparison lands on the member that
//! does not match and misses the one that does.

use std::collections::HashSet;

/// Paths, reduced so that none is under another.
#[derive(Debug, Clone, Default)]
pub struct PrefixSet {
    /// Sorted and reduced, for iterating and for the small case.
    paths: Vec<String>,
    /// The same paths, for asking about one path's ancestors.
    lookup: HashSet<String>,
}

impl PrefixSet {
    pub fn new(paths: Vec<String>) -> PrefixSet {
        let mut set = PrefixSet {
            paths,
            lookup: HashSet::new(),
        };
        set.reduce();
        set
    }

    /// Add more paths and reduce again.
    ///
    /// Appending is cheap and the reduction is one sort, so this is called once
    /// per batch rather than once per path — which is also what keeps it from
    /// being the quadratic thing it exists to prevent.
    pub fn extend(&mut self, paths: impl IntoIterator<Item = String>) {
        self.paths.extend(paths);
        self.reduce();
    }

    fn reduce(&mut self) {
        // Normalised on the way in, so a lookup can be an exact comparison.
        // `/` becomes the empty string, which is what `under` already treats it
        // as: everything is below the root.
        for p in &mut self.paths {
            let trimmed = p.trim_end_matches('/');
            if trimmed.len() != p.len() {
                p.truncate(trimmed.len());
            }
        }
        self.paths.sort_unstable();
        self.paths.dedup();
        // Sorted, so an ancestor is always before its descendants — and is
        // always the last one kept when the first descendant is reached,
        // because anything between them is under it too.
        let mut kept: Vec<String> = Vec::with_capacity(self.paths.len());
        for p in self.paths.drain(..) {
            if kept.last().is_none_or(|k| !under(&p, k)) {
                kept.push(p);
            }
        }
        self.paths = kept;
        self.lookup.clear();
        self.lookup.extend(self.paths.iter().cloned());
    }

    /// Is `path` at or below any member?
    pub fn covers(&self, path: &str) -> bool {
        match self.paths.len() {
            0 => false,
            // Below the point where hashing a dozen ancestors is worth it, and
            // this is the common case by far: nothing pending, or one folder
            // removed.
            1..=4 => self.paths.iter().any(|p| under(path, p)),
            _ => ancestors(path).any(|a| self.lookup.contains(a)),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.paths.iter().map(String::as_str)
    }

    pub fn clear(&mut self) {
        self.paths.clear();
        self.lookup.clear();
    }

    pub fn into_paths(self) -> Vec<String> {
        self.paths
    }
}

/// A path, then its parent, then its parent's parent, down to the empty string.
///
/// The empty string is deliberately the last one rather than skipped: it is
/// what a member of `/` normalises to, and it means "everything".
fn ancestors(path: &str) -> impl Iterator<Item = &str> {
    let path = path.trim_end_matches('/');
    std::iter::successors(Some(path), |p| {
        (!p.is_empty()).then(|| &p[..p.rfind('/').unwrap_or(0)])
    })
}

/// Is `path` at or below `prefix`?
///
/// A separator, not merely a prefix: `/home/u/Projeler-414` is not inside
/// `/home/u/Projeler`, and every place in Scour that has got this wrong has got
/// it wrong in exactly that way.
pub fn under(path: &str, prefix: &str) -> bool {
    let p = prefix.trim_end_matches('/');
    if p.is_empty() {
        return true;
    }
    path == p || (path.len() > p.len() && path.starts_with(p) && path.as_bytes()[p.len()] == b'/')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(v: &[&str]) -> PrefixSet {
        PrefixSet::new(v.iter().map(|s| (*s).to_owned()).collect())
    }

    #[test]
    fn a_path_is_under_its_own_prefix_but_not_under_a_longer_name() {
        assert!(under("/home/u/Projeler", "/home/u/Projeler"));
        assert!(under("/home/u/Projeler/a.rs", "/home/u/Projeler"));
        assert!(under("/home/u/Projeler/a.rs", "/home/u/Projeler/"));
        assert!(!under("/home/u/Projeler-414/a.rs", "/home/u/Projeler"));
        assert!(!under("/home/u/Proj", "/home/u/Projeler"));
        assert!(under("/anything", "/"));
    }

    #[test]
    fn an_ancestor_absorbs_everything_below_it() {
        assert_eq!(set(&["/a", "/a/b", "/a/b/c", "/a/d"]).len(), 1);
        assert_eq!(set(&["/a/b/c", "/a/b", "/a"]).into_paths(), vec!["/a"]);
    }

    #[test]
    fn siblings_survive_and_so_does_a_longer_name() {
        assert_eq!(set(&["/a/b", "/a/c"]).len(), 2);
        assert_eq!(set(&["/a/b", "/a/bc"]).len(), 2);
    }

    #[test]
    fn an_empty_path_means_everything() {
        let s = set(&["/a", "", "/b"]);
        assert_eq!(s.len(), 1);
        assert!(s.covers("/anywhere/at/all"));
        assert!(set(&["/"]).covers("/anywhere/at/all"));
    }

    #[test]
    fn the_fast_answer_agrees_with_the_slow_one() {
        // Past four members `covers` stops looking at every path, and the two
        // ways of answering have to give the same one.
        //
        // `lib` beside `lib-old` is the pair that killed the first attempt: `-`
        // sorts below `/`, so `/pkg0/lib/deep/f.rs` lands after `/pkg0/lib-old`
        // and a single binary-search comparison misses `/pkg0/lib` entirely.
        let members: Vec<String> = (0..64)
            .flat_map(|i| {
                [
                    format!("/corpus/pkg{i}/lib"),
                    format!("/corpus/pkg{i}/lib-old"),
                ]
            })
            .collect();
        let s = PrefixSet::new(members.clone());
        assert_eq!(s.len(), 128, "none of these is under another");
        let mut asked = 0;
        for m in &members {
            for probe in [
                m.clone(),
                format!("{m}/deep/file.rs"),
                format!("{m}x/file.rs"),
                format!("{m}-2/file.rs"),
                m.trim_end_matches("ib").to_owned(),
                format!("{m}/"),
            ] {
                let slow = members.iter().any(|p| under(&probe, p));
                assert_eq!(s.covers(&probe), slow, "disagreed about {probe:?}");
                asked += 1;
            }
        }
        assert!(asked > 700, "only asked {asked}");
        assert!(!s.covers("/corpus"), "a parent is not under its child");
        assert!(!s.covers("/somewhere/else"));
    }

    #[test]
    fn a_trailing_slash_on_a_member_is_the_same_member() {
        let s = set(&["/a/b/", "/a/b", "/c/"]);
        assert_eq!(s.len(), 2);
        assert!(s.covers("/a/b/deep"));
        assert!(s.covers("/c"));
    }

    #[test]
    fn nothing_pending_covers_nothing() {
        assert!(!PrefixSet::default().covers("/a/b"));
        assert!(PrefixSet::default().is_empty());
    }

    #[test]
    fn ancestors_stop_at_the_root() {
        assert_eq!(
            ancestors("/a/b/c").collect::<Vec<_>>(),
            vec!["/a/b/c", "/a/b", "/a", ""]
        );
        assert_eq!(ancestors("").collect::<Vec<_>>(), vec![""]);
        assert_eq!(ancestors("/").collect::<Vec<_>>(), vec![""]);
    }
}
