//! The obviously-correct answer.
//!
//! Every predicate here is written the slow, direct way: look at the entry,
//! decide. Nothing is indexed, nothing terminates early, nothing is capped.
//! That is the point — this is what a real index's answer is compared against,
//! and a reference implementation that shares a trick with the thing it checks
//! is not a reference implementation.

use scour_core::text::{DefaultFolder, Folder};
use scour_core::{Ast, Cmp, Entry, Hit, Match, SortKey, TimeField};
use scour_query::glob_matches;

/// Does this entry satisfy the query?
pub fn matches(e: &Entry, ast: &Ast) -> bool {
    ast.groups
        .iter()
        .all(|g| g.alts.iter().any(|(neg, m)| matches_one(e, m) != *neg))
}

fn matches_one(e: &Entry, m: &Match) -> bool {
    let name = DefaultFolder.fold(e.name());
    match m {
        Match::NameContains(t) => name.contains(t.as_str()),
        Match::NameGlob(p) => glob_matches(p, &name),
        Match::PathContains(t) => DefaultFolder.fold(&e.path).contains(t.as_str()),
        Match::Under(d) => {
            let d = d.trim_end_matches('/');
            e.path.len() > d.len()
                && e.path.starts_with(d)
                && (d.is_empty() || e.path.as_bytes().get(d.len()) == Some(&b'/'))
        }
        Match::ParentIs(d) => {
            e.parent() == d.trim_end_matches('/') || (d == "/" && e.parent() == "/")
        }
        Match::Ext(list) => list.contains(&e.ext()),
        Match::IsDir(want) => e.is_dir == *want,
        Match::Size(cmp, v) => cmp.holds(e.meta.size, *v),
        Match::Kind(k) => k.contains(&e.kind()),
        Match::Time(f, cmp, v) => {
            let got = match f {
                TimeField::Modified => e.meta.mtime,
                TimeField::Created => e.meta.ctime,
                TimeField::Accessed => e.meta.atime,
            };
            match cmp {
                // Equality on a timestamp means the calendar day it names.
                Cmp::Eq => got >= *v && got < *v + 86_400,
                c => c.holds(got, *v),
            }
        }
        // Nothing extracts content yet, so nothing can contain any.
        Match::ContentContains(_) => false,
    }
}

/// The first `limit` results, in the requested order, computed by looking at
/// everything.
pub fn brute_force(
    entries: &[Entry],
    ast: &Ast,
    sort: SortKey,
    desc: bool,
    limit: usize,
) -> Vec<Hit> {
    let mut hits: Vec<Hit> = entries
        .iter()
        .filter(|e| matches(e, ast))
        .map(Hit::from)
        .collect();
    hits.sort_unstable_by(|a, b| {
        let o = match sort {
            // The reference does not model relevance: scoring belongs to the
            // index, and a second implementation of it here would verify that
            // two copies of one idea agree rather than that the idea is right.
            // Queries sorted this way are not compared against this.
            SortKey::Relevance => std::cmp::Ordering::Equal,
            SortKey::Name => DefaultFolder
                .fold(a.name())
                .cmp(&DefaultFolder.fold(b.name())),
            SortKey::Path => a.path.cmp(&b.path),
            SortKey::Ext => scour_core::ext_of(a.name()).cmp(&scour_core::ext_of(b.name())),
            SortKey::Size => a.meta.size.cmp(&b.meta.size),
            SortKey::Modified => a.meta.mtime.cmp(&b.meta.mtime),
            SortKey::Created => a.meta.ctime.cmp(&b.meta.ctime),
            SortKey::Accessed => a.meta.atime.cmp(&b.meta.atime),
            SortKey::Kind => a.kind.cmp(&b.kind),
            SortKey::Items => a.meta.items.cmp(&b.meta.items),
            SortKey::Mode => a.meta.mode.cmp(&b.meta.mode),
            SortKey::Uid => a.meta.uid.cmp(&b.meta.uid),
            SortKey::Gid => a.meta.gid.cmp(&b.meta.gid),
            SortKey::Disk => a.meta.disk.cmp(&b.meta.disk),
        };
        let o = if desc { o.reverse() } else { o };
        // Ties break the way the index stores rows: newest first, then path.
        //
        // Not path alone, which is what this said first and which is only
        // sensible for a high-cardinality key. Sorting by *kind* on a real disk
        // puts a hundred thousand rows in one tie group, and breaking that
        // group on the path means an engine has to look at every one of them to
        // name the first forty. Newest-first is both cheaper — it is the order
        // the rows are already in — and the better answer: "code files, newest
        // first" is what someone sorting by kind wanted.
        o.then_with(|| b.meta.mtime.cmp(&a.meta.mtime))
            .then_with(|| a.path.cmp(&b.path))
    });
    hits.truncate(limit);
    hits
}

#[cfg(test)]
mod tests {
    use super::*;
    use scour_core::{EntryId, Meta, SourceId};
    use scour_query::parse_at;

    fn e(path: &str, is_dir: bool, size: i64, mtime: i64) -> Entry {
        Entry {
            id: EntryId::path_hash(SourceId(0), path),
            path: path.into(),
            is_dir,
            meta: Meta {
                size,
                mtime,
                ctime: mtime,
                atime: mtime,
                mode: if is_dir { 0o40755 } else { 0o100644 },
                ..Meta::UNKNOWN
            },
        }
    }

    fn tree() -> Vec<Entry> {
        vec![
            e("/p/src", true, 0, 300),
            e("/p/src/main.rs", false, 2_000, 200),
            e("/p/src/lib.rs", false, 5_000, 100),
            e("/p/RAPOR.pdf", false, 9_000_000, 400),
            e("/p/tmp.log", false, 10, 500),
        ]
    }

    #[test]
    fn the_reference_agrees_with_the_language() {
        let t = tree();
        let hits = |q: &str| -> Vec<String> {
            brute_force(&t, &parse_at(q, 1_000), SortKey::Path, false, 100)
                .into_iter()
                .map(|h| h.path)
                .collect()
        };
        assert_eq!(hits("ext:rs"), vec!["/p/src/lib.rs", "/p/src/main.rs"]);
        assert_eq!(hits("folder:"), vec!["/p/src"]);
        assert_eq!(hits("*.pdf"), vec!["/p/RAPOR.pdf"]);
        assert_eq!(
            hits("rapor"),
            vec!["/p/RAPOR.pdf"],
            "matching is case-folded"
        );
        assert_eq!(hits("size:>1mb"), vec!["/p/RAPOR.pdf"]);
        assert_eq!(hits("ext:rs !main"), vec!["/p/src/lib.rs"]);
        assert_eq!(hits("path:src ext:rs").len(), 2);
        assert_eq!(hits("content:anything"), Vec::<String>::new());
    }

    #[test]
    fn ordering_is_total_and_reversible() {
        let t = tree();
        let by = |k, desc| -> Vec<String> {
            brute_force(&t, &parse_at("", 0), k, desc, 100)
                .into_iter()
                .map(|h| h.path)
                .collect()
        };
        let asc = by(SortKey::Size, false);
        let mut desc = by(SortKey::Size, true);
        desc.reverse();
        assert_eq!(asc, desc, "reversing the order must reverse the list");
        assert_eq!(by(SortKey::Modified, true)[0], "/p/tmp.log", "newest first");
    }
}
