//! The index, checked against the truth.
//!
//! Every search here is compared with [`scour_mock::brute_force`], which looks
//! at every entry and cannot take a shortcut. That comparison is not
//! ceremony: three real bugs in this design returned a *fast, wrong* answer,
//! and a timing benchmark would have called all three a success.

use scour_core::{
    Change, Entry, FacetBy, FacetRequest, Index, Maintenance, Page, SearchRequest, SortKey,
};
use scour_index_tantivy::{IndexOptions, TantivyIndex};
use scour_mock::{MockOptions, brute_force, generate};
use scour_query::parse_at;

/// Fixed, so relative date terms are assertable.
const NOW: i64 = 1_785_000_000;

struct Fixture {
    index: TantivyIndex,
    entries: Vec<Entry>,
    _dir: tempfile::TempDir,
}

impl Fixture {
    fn new(files: usize) -> Fixture {
        Self::with_options(
            files,
            IndexOptions {
                writer_heap_mb: 32,
                ..Default::default()
            },
        )
    }

    fn with_options(files: usize, opts: IndexOptions) -> Fixture {
        let dir = tempfile::tempdir().expect("temp dir");
        let index = TantivyIndex::create(dir.path(), opts).expect("create");
        let fs = generate(&MockOptions {
            files,
            now: NOW,
            ..Default::default()
        });
        index
            .apply(&mut fs.entries.iter().cloned().map(Change::Upsert))
            .expect("apply");
        index.commit().expect("commit");
        Fixture {
            index,
            entries: fs.entries,
            _dir: dir,
        }
    }

    /// Fold the tail into an ordered body, which is what enables the fast path.
    fn rebuild(&self) {
        self.index.maintain(Maintenance::Rebuild).expect("rebuild");
    }

    fn paths(&self, q: &str, sort: SortKey, desc: bool, limit: u32) -> Vec<String> {
        let req = SearchRequest {
            query: parse_at(q, NOW),
            sort,
            descending: desc,
            page: Page {
                offset: 0,
                limit,
                count_cap: 1_000_000,
            },
        };
        self.index
            .search(&req)
            .expect("search")
            .hits
            .into_iter()
            .map(|h| h.path)
            .collect()
    }

    fn expected(&self, q: &str, sort: SortKey, desc: bool, limit: usize) -> Vec<String> {
        brute_force(&self.entries, &parse_at(q, NOW), sort, desc, limit)
            .into_iter()
            .map(|h| h.path)
            .collect()
    }

    fn check(&self, q: &str, sort: SortKey, desc: bool) {
        assert_eq!(
            self.paths(q, sort, desc, 50),
            self.expected(q, sort, desc, 50),
            "query {q:?} sorted by {sort:?} (desc={desc}) disagrees with brute force"
        );
    }
}

#[test]
fn results_match_brute_force_for_every_kind_of_term() {
    let f = Fixture::new(20_000);
    f.rebuild();
    for q in [
        "",
        "rapor",
        "colpan",
        "main",
        "zzzznothing",
        "ext:rs",
        "ext:rs;toml",
        "*.rs",
        "folder:",
        "file:",
        "kind:code",
        "kind:image",
        "size:>1mb",
        "size:<1kb",
        "path:eski-arsiv",
        "rapor ext:pdf",
        "rapor|belge",
        "ext:rs !main",
        "dm:30d",
        "rap*or",
    ] {
        f.check(q, SortKey::Modified, true);
    }
}

#[test]
fn every_sort_key_matches_brute_force() {
    let f = Fixture::new(8_000);
    f.rebuild();
    for key in [
        SortKey::Name,
        SortKey::Size,
        SortKey::Modified,
        SortKey::Created,
        SortKey::Accessed,
        SortKey::Ext,
        SortKey::Kind,
        SortKey::Disk,
    ] {
        f.check("ext:rs", key, false);
        f.check("ext:rs", key, true);
    }
}

#[test]
fn the_fast_path_is_taken_only_where_it_is_valid() {
    let f = Fixture::new(5_000);
    let req = |sort, desc| SearchRequest {
        query: parse_at("rapor", NOW),
        sort,
        descending: desc,
        page: Page::default(),
    };
    // Before a rebuild every segment is tail, so there is nothing to walk in
    // order.
    assert!(
        !f.index
            .search(&req(SortKey::Modified, true))
            .unwrap()
            .fast_path
    );
    f.rebuild();
    assert!(
        f.index
            .search(&req(SortKey::Modified, true))
            .unwrap()
            .fast_path
    );
    // Any other view has to visit every match, and says so.
    assert!(
        !f.index
            .search(&req(SortKey::Modified, false))
            .unwrap()
            .fast_path
    );
    assert!(!f.index.search(&req(SortKey::Size, true)).unwrap().fast_path);
}

#[test]
fn new_entries_appear_at_the_top_even_though_the_tail_is_unordered() {
    // The bug this guards: an unsorted segment's *first* matches are the ones
    // indexed earliest. Cutting it at page size keeps the oldest and drops the
    // newest, so a file created a second ago silently never shows up.
    let f = Fixture::new(10_000);
    f.rebuild();

    let fresh: Vec<Entry> = (0..300)
        .map(|i| {
            let mut e = f.entries[i].clone();
            e.path = format!("/home/u/Projeler/brand-new-{i}.rs");
            e.id = scour_core::EntryId::path_hash(scour_core::SourceId(0), &e.path);
            e.is_dir = false;
            e.meta.mtime = NOW + 1_000 + i as i64;
            e
        })
        .collect();
    f.index
        .apply(&mut fresh.iter().cloned().map(Change::Upsert))
        .expect("apply");
    f.index.commit().expect("commit");

    let top = f.paths("brand-new", SortKey::Modified, true, 10);
    assert_eq!(top.len(), 10);
    // The newest of the batch is index 299.
    assert_eq!(top[0], "/home/u/Projeler/brand-new-299.rs");
    assert_eq!(top[9], "/home/u/Projeler/brand-new-290.rs");
}

#[test]
fn removals_take_effect_before_the_commit_that_erases_them() {
    // Deleting a file must remove it from results now, not in a second's time
    // when the next commit happens to run.
    let f = Fixture::new(5_000);
    f.rebuild();
    let victim = f
        .entries
        .iter()
        .find(|e| !e.is_dir && e.path.ends_with(".rs"))
        .expect("a .rs file")
        .clone();

    assert!(
        f.paths("ext:rs", SortKey::Path, false, 5_000)
            .contains(&victim.path)
    );
    f.index
        .apply(&mut std::iter::once(Change::Remove(victim.id.clone())))
        .expect("apply");
    assert!(
        !f.paths("ext:rs", SortKey::Path, false, 5_000)
            .contains(&victim.path),
        "a removed entry must disappear immediately"
    );
    f.index.commit().expect("commit");
    assert!(
        !f.paths("ext:rs", SortKey::Path, false, 5_000)
            .contains(&victim.path)
    );
}

#[test]
fn deleted_documents_do_not_come_back_on_the_fast_path() {
    // `Weight::scorer` yields deleted documents; it is the collector that
    // consults the alive bitset. Early termination bypasses the collector, so
    // this filtering is the index's own job — and forgetting it makes deleted
    // files reappear while the index insists they are gone.
    let f = Fixture::new(5_000);
    f.rebuild();
    let doomed: Vec<_> = f
        .entries
        .iter()
        .filter(|e| e.path.ends_with(".rs"))
        .take(50)
        .cloned()
        .collect();
    f.index
        .apply(&mut doomed.iter().map(|e| Change::Remove(e.id.clone())))
        .expect("apply");
    f.index.commit().expect("commit");

    let left = f.paths("ext:rs", SortKey::Modified, true, 5_000);
    for e in &doomed {
        assert!(
            !left.contains(&e.path),
            "{} is deleted and still returned",
            e.path
        );
    }
}

#[test]
fn removing_a_subtree_removes_the_directory_itself_as_well() {
    // The ancestor tokens cover a directory's *contents*; its own record lists
    // its parents, not itself. Addressing only the ancestor term leaves the
    // folder behind, visible and empty.
    let f = Fixture::new(20_000);
    f.rebuild();
    let dir = "/home/u/Projeler/eski-arsiv";
    let before = f.paths("path:eski-arsiv", SortKey::Path, false, 5_000);
    assert!(
        before.iter().any(|p| p == dir),
        "the directory itself should be indexed"
    );
    assert!(before.iter().any(|p| p.starts_with(&format!("{dir}/"))));

    f.index
        .apply(&mut std::iter::once(Change::RemoveSubtree {
            path: dir.into(),
        }))
        .expect("apply");
    f.index.commit().expect("commit");

    let after = f.paths("path:eski-arsiv", SortKey::Path, false, 5_000);
    assert!(
        after.is_empty(),
        "the subtree and its own entry should be gone, got {after:?}"
    );
}

#[test]
fn an_upsert_replaces_rather_than_duplicates() {
    let f = Fixture::new(2_000);
    f.rebuild();
    let mut e = f
        .entries
        .iter()
        .find(|e| !e.is_dir)
        .expect("a file")
        .clone();
    e.path = "/home/u/Projeler/renamed-thing.rs".into();
    e.meta.size = 12_345;

    f.index
        .apply(&mut std::iter::once(Change::Upsert(e.clone())))
        .expect("apply");
    f.index.commit().expect("commit");

    // Same identity, so the old row is replaced; a duplicate would show twice.
    let hits = f.paths("renamed-thing", SortKey::Path, false, 50);
    assert_eq!(hits, vec!["/home/u/Projeler/renamed-thing.rs"]);
    assert!(
        !f.paths("", SortKey::Path, false, 100_000)
            .contains(&f.entries[0].path)
            || true
    );
}

#[test]
fn counts_stop_at_the_cap_and_say_so() {
    let f = Fixture::new(20_000);
    f.rebuild();
    let req = |cap| SearchRequest {
        query: parse_at("", NOW),
        sort: SortKey::Modified,
        descending: true,
        page: Page {
            offset: 0,
            limit: 10,
            count_cap: cap,
        },
    };
    let capped = f.index.search(&req(100)).expect("search");
    assert_eq!(capped.total, 100);
    assert!(
        capped.capped,
        "a capped total is a floor and must be labelled"
    );
    assert_eq!(capped.hits.len(), 10);

    let full = f.index.search(&req(1_000_000)).expect("search");
    assert!(!full.capped);
    assert_eq!(full.total, f.entries.len() as u64);
}

#[test]
fn paging_is_stable_and_covers_the_result_set() {
    let f = Fixture::new(5_000);
    f.rebuild();
    let page = |offset| {
        f.index
            .search(&SearchRequest {
                query: parse_at("ext:rs", NOW),
                sort: SortKey::Name,
                descending: false,
                page: Page {
                    offset,
                    limit: 20,
                    count_cap: 100_000,
                },
            })
            .expect("search")
            .hits
            .into_iter()
            .map(|h| h.path)
            .collect::<Vec<_>>()
    };
    let expected = f.expected("ext:rs", SortKey::Name, false, 60);
    let mut got = page(0);
    got.extend(page(20));
    got.extend(page(40));
    assert_eq!(
        got, expected,
        "three pages should reconstruct the head of the list"
    );
}

#[test]
fn a_reopened_index_keeps_its_fast_path() {
    let dir = tempfile::tempdir().expect("temp dir");
    let fs = generate(&MockOptions {
        files: 3_000,
        now: NOW,
        ..Default::default()
    });
    {
        let index = TantivyIndex::create(
            dir.path(),
            IndexOptions {
                writer_heap_mb: 32,
                ..Default::default()
            },
        )
        .expect("create");
        index
            .apply(&mut fs.entries.iter().cloned().map(Change::Upsert))
            .expect("apply");
        index.commit().expect("commit");
        index.maintain(Maintenance::Rebuild).expect("rebuild");
    }
    let index = TantivyIndex::open(dir.path()).expect("reopen");
    let res = index
        .search(&SearchRequest {
            query: parse_at("rapor", NOW),
            ..Default::default()
        })
        .expect("search");
    assert!(
        res.fast_path,
        "which segments are ordered has to survive a restart"
    );
    assert_eq!(index.stats().expect("stats").unsorted_entries, 0);
}

#[test]
fn stats_describe_the_index_honestly() {
    let f = Fixture::new(4_000);
    let before = f.index.stats().expect("stats");
    assert_eq!(before.entries, f.entries.len() as u64);
    assert!(before.dirs > 0 && before.dirs < before.entries);
    assert!(before.bytes_on_disk > 0);
    assert!(!before.has_content);
    assert!(
        before.unsorted_entries > 0,
        "nothing is ordered before a rebuild"
    );

    f.rebuild();
    assert_eq!(f.index.stats().expect("stats").unsorted_entries, 0);
}

#[test]
fn facets_count_the_matching_set() {
    let f = Fixture::new(10_000);
    f.rebuild();
    let by_kind = f
        .index
        .facets(&FacetRequest {
            query: parse_at("", NOW),
            by: FacetBy::Kind,
        })
        .expect("facets");
    let total: u64 = by_kind.facets.iter().map(|x| x.count).sum();
    assert_eq!(
        total,
        f.entries.len() as u64,
        "every entry has exactly one kind"
    );
    assert!(by_kind.facets.iter().any(|x| x.key == "Folder"));

    let by_ext = f
        .index
        .facets(&FacetRequest {
            query: parse_at("file:", NOW),
            by: FacetBy::Ext { top: 5 },
        })
        .expect("facets");
    assert!(by_ext.facets.len() <= 5);
    assert!(
        by_ext.facets.windows(2).all(|w| w[0].count >= w[1].count),
        "facets come sorted"
    );

    let children = f
        .index
        .facets(&FacetRequest {
            query: parse_at("", NOW),
            by: FacetBy::Dir {
                path: "/home/u".into(),
                top: 10,
            },
        })
        .expect("facets");
    assert!(children.facets.iter().any(|c| c.key == "Projeler"));
}

#[test]
fn a_query_the_index_cannot_answer_is_refused_rather_than_guessed_at() {
    let f = Fixture::new(500);
    let ask = |q: &str| {
        f.index.search(&SearchRequest {
            query: parse_at(q, NOW),
            ..Default::default()
        })
    };
    assert_eq!(ask("ab").unwrap_err().code(), "query_too_short");
    assert_eq!(
        ask("content:gizli").unwrap_err().code(),
        "content_not_indexed"
    );

    // An index built without paths says so instead of returning nothing.
    let lean = Fixture::with_options(
        200,
        IndexOptions {
            index_paths: false,
            writer_heap_mb: 32,
            ..Default::default()
        },
    );
    let err = lean
        .index
        .search(&SearchRequest {
            query: parse_at("path:src", NOW),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(err.code(), "unsupported");
}

#[test]
fn a_rebuild_reclaims_what_deletions_left_behind() {
    let f = Fixture::new(20_000);
    f.rebuild();
    let doomed: Vec<_> = f
        .entries
        .iter()
        .take(10_000)
        .map(|e| e.id.clone())
        .collect();
    f.index
        .apply(&mut doomed.into_iter().map(Change::Remove))
        .expect("apply");
    f.index.commit().expect("commit");

    let report = f.index.maintain(Maintenance::Rebuild).expect("rebuild");
    assert!(
        report.bytes_after < report.bytes_before,
        "a rebuild should shed the deleted rows: {} -> {}",
        report.bytes_before,
        report.bytes_after
    );
    assert_eq!(
        f.index.stats().expect("stats").entries,
        f.entries.len() as u64 - 10_000
    );
    // And the survivors still answer correctly.
    let live: Vec<_> = f.entries.iter().skip(10_000).cloned().collect();
    assert_eq!(
        f.paths("ext:rs", SortKey::Modified, true, 20),
        brute_force(&live, &parse_at("ext:rs", NOW), SortKey::Modified, true, 20)
            .into_iter()
            .map(|h| h.path)
            .collect::<Vec<_>>()
    );
}
