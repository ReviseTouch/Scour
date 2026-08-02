//! The index as a whole, checked against the truth.
//!
//! [`crate::smoke`](../smoke.rs) checks one segment. This checks the thing the
//! rest of Scour actually holds: several segments, removals that have not been
//! written yet, a generation sweep, a rebuild, and a restart — with every
//! answer compared against [`scour_mock::brute_force`], which looks at every
//! entry and cannot take a shortcut.
//!
//! Almost everything that can go wrong here is invisible to a benchmark. A
//! second segment that is searched but not merged, a removal that is hidden but
//! never erased, a re-upsert that leaves the old row alive — each of those
//! returns a fast, plausible answer.

use scour_core::{
    Change, Entry, EntryId, FacetBy, FacetRequest, Index, Maintenance, Meta, Page, SearchRequest,
    SortKey, SourceId,
};
use scour_index_native::NativeIndex;
use scour_mock::{MockOptions, brute_force, generate};
use scour_query::parse_at;

const NOW: i64 = 1_785_000_000;

struct Fixture {
    _tmp: tempfile::TempDir,
    index: NativeIndex,
    entries: Vec<Entry>,
}

impl Fixture {
    /// Build an index, committing every `chunk` entries so the result has as
    /// many segments as the test wants.
    fn new(files: usize, chunk: usize) -> Fixture {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        let fs = generate(&MockOptions {
            files,
            now: NOW,
            ..Default::default()
        });
        for part in fs.entries.chunks(chunk) {
            let mut it = part.iter().cloned().map(Change::Upsert);
            index.apply(&mut it).expect("apply");
            index.commit().expect("commit");
        }
        Fixture {
            _tmp: tmp,
            index,
            entries: fs.entries,
        }
    }

    fn search(&self, q: &str, sort: SortKey, desc: bool, limit: usize) -> Vec<String> {
        self.paged(q, sort, desc, 0, limit)
    }

    fn paged(
        &self,
        q: &str,
        sort: SortKey,
        desc: bool,
        offset: usize,
        limit: usize,
    ) -> Vec<String> {
        self.index
            .search(&SearchRequest {
                query: parse_at(q, NOW),
                sort,
                descending: desc,
                page: Page {
                    offset: offset as u32,
                    limit: limit as u32,
                    count_cap: 10_000_000,
                },
            })
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
            self.search(q, sort, desc, 50),
            self.expected(q, sort, desc, 50),
            "query {q:?} by {sort:?} (desc={desc}) disagrees with brute force"
        );
    }
}

fn entry(path: &str, mtime: i64, ino: u64) -> Entry {
    Entry {
        id: EntryId::inode(SourceId(0), 66_310, ino),
        path: path.into(),
        is_dir: false,
        meta: Meta {
            mtime,
            size: 1234,
            ..Meta::UNKNOWN
        },
    }
}

#[test]
fn many_segments_answer_exactly_what_one_would() {
    // Eight segments. Everything a search does across them — the merge, the
    // page, the count — has to produce the same list as a single pass over the
    // entries would.
    let f = Fixture::new(16_000, 2_000);
    assert!(
        f.index.stats().expect("stats").segments >= 8,
        "the fixture is supposed to be fragmented"
    );
    for q in [
        "",
        "rapor",
        "main",
        "ab",
        "ext:rs",
        "*.pdf",
        "kind:code",
        "size:>1mb",
        "under:/home/u/Projeler",
        "parent:/home/u",
        "under:/home/u/Projeler ext:rs",
        "rapor|belge",
        "ext:rs !main",
        "dm:30d",
        "İSTANBUL",
        "zzzznothing",
    ] {
        f.check(q, SortKey::Modified, true);
    }
    for key in [
        SortKey::Name,
        SortKey::Path,
        SortKey::Size,
        SortKey::Created,
        SortKey::Ext,
        SortKey::Kind,
    ] {
        f.check("ext:rs", key, false);
        f.check("ext:rs", key, true);
    }
    assert_eq!(
        f.index.stats().expect("stats").entries,
        f.entries.len() as u64
    );
}

#[test]
fn paging_across_segments_reconstructs_the_list() {
    // The offset belongs to the merged list, not to any one segment. Applying
    // it per segment would drop a row from each and quietly return a page that
    // is short in a way nothing reports.
    let f = Fixture::new(6_000, 500);
    let mut got = f.paged("ext:rs", SortKey::Name, false, 0, 20);
    got.extend(f.paged("ext:rs", SortKey::Name, false, 20, 20));
    got.extend(f.paged("ext:rs", SortKey::Name, false, 40, 20));
    assert_eq!(got, f.expected("ext:rs", SortKey::Name, false, 60));
}

#[test]
fn a_removal_is_invisible_before_it_is_written() {
    // The one thing that may not wait for a commit. Deleting a file and still
    // seeing it reads as a broken program, so the removal takes effect in the
    // overlay first and in the files afterwards.
    let f = Fixture::new(2_000, 2_000);
    let victim = f.search("", SortKey::Modified, true, 1)[0].clone();
    let id = f
        .entries
        .iter()
        .find(|e| e.path == victim)
        .expect("victim")
        .id
        .clone();

    f.index
        .apply(&mut std::iter::once(Change::Remove(id)))
        .expect("apply");
    let after = f.search("", SortKey::Modified, true, 5);
    assert!(!after.contains(&victim), "{victim} is still visible");
    assert_eq!(f.index.stats().expect("stats").pending_removals, 1);

    f.index.commit().expect("commit");
    let after = f.search("", SortKey::Modified, true, 5);
    assert!(
        !after.contains(&victim),
        "{victim} came back after the commit"
    );
    let s = f.index.stats().expect("stats");
    assert_eq!(s.entries, f.entries.len() as u64 - 1);
    assert_eq!(s.pending_removals, 0);
}

#[test]
fn removing_a_subtree_takes_its_contents_and_itself() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    let mut es = vec![
        Entry {
            is_dir: true,
            ..entry("/home/u/Projeler", 500, 1)
        },
        entry("/home/u/Projeler/a.rs", 400, 2),
        entry("/home/u/Projeler/deep/b.rs", 300, 3),
        // The sibling that starts with the same letters and must survive.
        entry("/home/u/Projeler-414/c.rs", 200, 4),
        entry("/home/u/other.txt", 100, 5),
    ];
    index
        .apply(&mut es.drain(..).map(Change::Upsert))
        .expect("apply");
    index.commit().expect("commit");

    index
        .apply(&mut std::iter::once(Change::RemoveSubtree {
            path: "/home/u/Projeler".into(),
        }))
        .expect("apply");

    let left = |index: &NativeIndex| -> Vec<String> {
        index
            .search(&SearchRequest {
                page: Page::new(0, 100),
                ..Default::default()
            })
            .expect("search")
            .hits
            .into_iter()
            .map(|h| h.path)
            .collect()
    };
    let want = vec![
        "/home/u/Projeler-414/c.rs".to_owned(),
        "/home/u/other.txt".to_owned(),
    ];
    assert_eq!(left(&index), want, "before the commit");
    index.commit().expect("commit");
    assert_eq!(left(&index), want, "after the commit");
    assert_eq!(index.stats().expect("stats").entries, 2);
}

#[test]
fn re_indexing_a_file_replaces_it_instead_of_doubling_it() {
    // What a rescan does to every unchanged file, and the failure mode is a
    // list with each path in it twice.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    let first = entry("/a/report.pdf", 100, 7);
    index
        .apply(&mut std::iter::once(Change::Upsert(first.clone())))
        .expect("apply");
    index.commit().expect("commit");

    // Same identity, new metadata, in its own segment.
    let second = entry("/a/report.pdf", 900, 7);
    index
        .apply(&mut std::iter::once(Change::Upsert(second)))
        .expect("apply");
    index.commit().expect("commit");

    let res = index
        .search(&SearchRequest {
            page: Page::new(0, 10),
            ..Default::default()
        })
        .expect("search");
    assert_eq!(res.hits.len(), 1, "the file is indexed twice");
    assert_eq!(res.hits[0].meta.mtime, 900, "the older row won");
    assert_eq!(res.total, 1);

    // And twice within one batch, which a watcher does routinely.
    index
        .apply(
            &mut [
                Change::Upsert(entry("/a/report.pdf", 1000, 7)),
                Change::Upsert(entry("/a/report.pdf", 1100, 7)),
            ]
            .into_iter(),
        )
        .expect("apply");
    index.commit().expect("commit");
    let res = index
        .search(&SearchRequest {
            page: Page::new(0, 10),
            ..Default::default()
        })
        .expect("search");
    assert_eq!(res.hits.len(), 1);
    assert_eq!(res.hits[0].meta.mtime, 1100);
}

#[test]
fn a_sweep_removes_what_a_rescan_did_not_find() {
    // The case a rescan cannot report: a file deleted while nothing was
    // watching. The scan finds three of four; the fourth is only identifiable
    // as the one carrying an older stamp.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    let originals = vec![
        entry("/w/a.rs", 400, 1),
        entry("/w/b.rs", 300, 2),
        entry("/w/gone.rs", 200, 3),
        entry("/elsewhere/keep.rs", 100, 4),
    ];
    index
        .apply(&mut originals.clone().into_iter().map(Change::Upsert))
        .expect("apply");
    index.commit().expect("commit");

    let g = index.begin_generation().expect("generation");
    index
        .apply(
            &mut [
                Change::Upsert(entry("/w/a.rs", 400, 1)),
                Change::Upsert(entry("/w/b.rs", 300, 2)),
            ]
            .into_iter(),
        )
        .expect("apply");
    let gone = index.sweep("/w", g).expect("sweep");
    assert_eq!(gone, 1, "exactly the file the rescan did not see");

    let paths: Vec<String> = index
        .search(&SearchRequest {
            page: Page::new(0, 10),
            ..Default::default()
        })
        .expect("search")
        .hits
        .into_iter()
        .map(|h| h.path)
        .collect();
    assert_eq!(
        paths,
        vec![
            "/w/a.rs".to_owned(),
            "/w/b.rs".to_owned(),
            "/elsewhere/keep.rs".to_owned(),
        ],
        "a sweep of /w must not touch anything outside it"
    );
}

#[test]
fn a_compaction_folds_the_head_and_leaves_the_body() {
    // What a search pays for is the number of segments, so a compaction only
    // has to get that number down — and rewriting the body to do it would cost
    // a pass over the whole index for nothing.
    let f = Fixture::new(8_000, 800);
    let before = f.search("ext:rs", SortKey::Modified, true, 60);
    let started = f.index.stats().expect("stats");
    assert!(
        started.segments >= 10,
        "the fixture is supposed to be fragmented"
    );

    f.index.maintain(Maintenance::Compact).expect("compact");
    let after = f.index.stats().expect("stats");
    assert_eq!(after.segments, 2, "one body, one folded head");
    assert_eq!(after.entries, started.entries);
    assert_eq!(f.search("ext:rs", SortKey::Modified, true, 60), before);
    f.check("", SortKey::Modified, true);
    f.check("rapor", SortKey::Name, false);

    // And doing it again changes nothing, rather than folding the body in.
    f.index.maintain(Maintenance::Compact).expect("compact");
    assert_eq!(f.index.stats().expect("stats").segments, 2);
}

#[test]
fn a_generation_is_never_folded_into_another_one() {
    // The merged segment can only carry one stamp. Folding across a boundary
    // would give old rows a new one, and the next sweep would walk straight
    // past exactly the rows it exists to remove.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    for i in 0..4u64 {
        index
            .apply(&mut std::iter::once(Change::Upsert(entry(
                &format!("/w/old{i}.rs"),
                100 + i as i64,
                i,
            ))))
            .expect("apply");
        index.commit().expect("commit");
    }
    let g = index.begin_generation().expect("generation");
    for i in 10..14u64 {
        index
            .apply(&mut std::iter::once(Change::Upsert(entry(
                &format!("/w/new{i}.rs"),
                200 + i as i64,
                i,
            ))))
            .expect("apply");
        index.commit().expect("commit");
    }
    index.maintain(Maintenance::Rebuild).expect("rebuild");
    assert_eq!(
        index.stats().expect("stats").segments,
        2,
        "one segment a generation, not one overall"
    );

    // The sweep still finds the older pass.
    assert_eq!(index.sweep("/w", g).expect("sweep"), 4);
    let left: Vec<String> = index
        .search(&SearchRequest {
            page: Page::new(0, 20),
            ..Default::default()
        })
        .expect("search")
        .hits
        .into_iter()
        .map(|h| h.path)
        .collect();
    assert_eq!(left.len(), 4);
    assert!(left.iter().all(|p| p.contains("new")), "{left:?}");

    // And the generation the sweep emptied folds to nothing rather than to an
    // empty segment. An empty one would be permanent: no rows means no dead
    // rows, so it would never qualify to be folded again — which is how a real
    // index ended up reporting three segments where one held everything.
    index.maintain(Maintenance::Rebuild).expect("rebuild");
    assert_eq!(index.stats().expect("stats").segments, 1);
}

#[test]
fn a_rebuild_folds_everything_into_one_segment_and_changes_no_answer() {
    let f = Fixture::new(8_000, 1_000);
    let before: Vec<String> = f.search("ext:rs", SortKey::Modified, true, 60);
    assert!(f.index.stats().expect("stats").unsorted_entries > 0);

    f.index.maintain(Maintenance::Rebuild).expect("rebuild");
    let s = f.index.stats().expect("stats");
    assert_eq!(s.segments, 1);
    assert_eq!(s.entries, f.entries.len() as u64);
    assert_eq!(s.unsorted_entries, 0);

    assert_eq!(f.search("ext:rs", SortKey::Modified, true, 60), before);
    for key in [SortKey::Name, SortKey::Size, SortKey::Path] {
        f.check("ext:rs", key, true);
    }
    f.check("", SortKey::Modified, true);
}

#[test]
fn a_rebuild_drops_the_rows_nobody_can_see() {
    let f = Fixture::new(4_000, 1_000);
    let doomed: Vec<EntryId> = f.entries.iter().take(500).map(|e| e.id.clone()).collect();
    f.index
        .apply(&mut doomed.into_iter().map(Change::Remove))
        .expect("apply");
    f.index.commit().expect("commit");
    let before = f.index.stats().expect("stats");
    assert_eq!(before.entries, f.entries.len() as u64 - 500);

    f.index.maintain(Maintenance::Rebuild).expect("rebuild");
    let after = f.index.stats().expect("stats");
    assert_eq!(after.entries, f.entries.len() as u64 - 500);
    assert!(
        after.bytes_on_disk < before.bytes_on_disk,
        "the dead rows are still taking room: {} then {}",
        before.bytes_on_disk,
        after.bytes_on_disk
    );
}

#[test]
fn an_index_reopened_from_disk_answers_the_same_way() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let fs = generate(&MockOptions {
        files: 3_000,
        now: NOW,
        ..Default::default()
    });
    let want = {
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        for part in fs.entries.chunks(1_000) {
            index
                .apply(&mut part.iter().cloned().map(Change::Upsert))
                .expect("apply");
            index.commit().expect("commit");
        }
        // A removal, so the liveness bits have something to remember.
        index
            .apply(&mut std::iter::once(Change::Remove(
                fs.entries[0].id.clone(),
            )))
            .expect("apply");
        index.commit().expect("commit");
        index
            .search(&SearchRequest {
                query: parse_at("ext:rs", NOW),
                page: Page::new(0, 50),
                ..Default::default()
            })
            .expect("search")
    };

    let index = NativeIndex::open_or_create(tmp.path()).expect("reopen");
    let got = index
        .search(&SearchRequest {
            query: parse_at("ext:rs", NOW),
            page: Page::new(0, 50),
            ..Default::default()
        })
        .expect("search");
    assert_eq!(got.hits, want.hits);
    assert_eq!(got.total, want.total);
    assert_eq!(
        index.stats().expect("stats").entries,
        fs.entries.len() as u64 - 1
    );
}

#[test]
fn the_stored_order_still_stops_early_with_several_segments() {
    // The claim the layout rests on has to survive fragmentation: each segment
    // is in date order, so each can stop at its own page and the merge picks
    // the winners.
    let f = Fixture::new(40_000, 5_000);
    let res = f
        .index
        .search(&SearchRequest {
            query: parse_at("", NOW),
            sort: SortKey::Modified,
            descending: true,
            page: Page {
                offset: 0,
                limit: 40,
                count_cap: 500,
            },
        })
        .expect("search");
    assert!(res.fast_path, "every segment should have stopped");
    assert_eq!(res.hits.len(), 40);
    assert!(res.capped);
    assert_eq!(
        res.hits.iter().map(|h| h.path.clone()).collect::<Vec<_>>(),
        f.expected("", SortKey::Modified, true, 40),
        "stopping early must not change which forty"
    );

    // And an order it cannot serve says so rather than pretending.
    let res = f
        .index
        .search(&SearchRequest {
            query: parse_at("", NOW),
            sort: SortKey::Size,
            descending: true,
            page: Page {
                offset: 0,
                limit: 40,
                count_cap: 500,
            },
        })
        .expect("search");
    assert!(!res.fast_path);
    assert_eq!(
        res.hits.iter().map(|h| h.path.clone()).collect::<Vec<_>>(),
        f.expected("", SortKey::Size, true, 40),
        "a cap may bound the total, never the result"
    );
}

#[test]
fn facets_count_what_a_search_would_have_returned() {
    let f = Fixture::new(6_000, 1_500);
    let by_ext = f
        .index
        .facets(&FacetRequest {
            query: parse_at("kind:code", NOW),
            by: FacetBy::Ext { top: 5 },
        })
        .expect("facets");
    assert!(!by_ext.facets.is_empty());
    assert!(
        by_ext.facets.windows(2).all(|w| w[0].count >= w[1].count),
        "facets come back most-common first"
    );

    // Cross-check the top one against the query that names it.
    let top = &by_ext.facets[0];
    let counted = f
        .index
        .search(&SearchRequest {
            query: parse_at(&format!("kind:code ext:{}", top.key), NOW),
            page: Page {
                offset: 0,
                limit: 1,
                count_cap: 10_000_000,
            },
            ..Default::default()
        })
        .expect("search");
    assert_eq!(counted.total, top.count, "ext:{} disagrees", top.key);

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
    assert!(
        children.facets.iter().all(|c| !c.key.contains('/')),
        "a child is one component, not a path: {:?}",
        children.facets
    );
}

#[test]
fn a_query_the_index_cannot_answer_is_refused_rather_than_guessed_at() {
    let f = Fixture::new(200, 200);
    let err = f
        .index
        .search(&SearchRequest {
            query: parse_at("content:gizli", NOW),
            ..Default::default()
        })
        .unwrap_err();
    assert_eq!(err.code(), "content_not_indexed");
}

#[test]
fn an_empty_index_answers_nothing_rather_than_failing() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    let res = index
        .search(&SearchRequest {
            page: Page::new(0, 10),
            ..Default::default()
        })
        .expect("search");
    assert!(res.hits.is_empty());
    assert_eq!(res.total, 0);
    assert!(!res.fast_path, "there was no fast path; there was no index");
    let s = index.stats().expect("stats");
    assert_eq!(s.entries, 0);
    assert_eq!(s.segments, 0);
    index.commit().expect("an empty commit is not an error");
    index.maintain(Maintenance::Rebuild).expect("empty rebuild");
}
