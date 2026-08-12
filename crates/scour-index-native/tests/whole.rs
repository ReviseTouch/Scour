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

use std::os::unix::fs::PermissionsExt;

use scour_core::{
    Change, Entry, EntryId, FacetBy, FacetRequest, Index, Maintenance, Meta, Page, PrefixSet,
    SearchRequest, SortKey, SourceId,
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

/// A page deep in the list is the same rows the list has there.
///
/// **The guard over what a deep offset is allowed to become.** Reaching row
/// 100,000 costs 1.43 s and 401,438 reconstructed paths to return sixty, because
/// every segment materialises `offset + limit` rows and the merge throws all but
/// the window away. Anything that fixes that changes where the offset is
/// applied — which is exactly the change that can quietly return *a* page
/// instead of *the* page.
///
/// So: every order, both directions, offsets that land inside a segment and
/// across a boundary.
///
/// The reference is the index's **own** full answer rather than brute force,
/// and that is the point rather than a weakening. Paging is a statement about
/// self-consistency: whatever order the index chose, the window at 900 has to
/// be that order's rows 900 to 960. Brute force cannot say — where a sort ties,
/// two orders are both right, and `ext:rs` sorted by relevance ties on every
/// row, because a filter gives relevance nothing to score. Comparing against it
/// there fails on a disagreement that is not an error, and the tests that do
/// compare orders against brute force already exist.
#[test]
fn a_page_deep_in_the_list_holds_the_rows_the_list_holds_there() {
    // Twelve segments, so an offset of 900 is several boundaries in.
    let f = Fixture::new(6_000, 500);
    const ORDERS: [SortKey; 8] = [
        SortKey::Relevance,
        SortKey::Name,
        SortKey::Path,
        SortKey::Size,
        SortKey::Modified,
        SortKey::Created,
        SortKey::Ext,
        SortKey::Kind,
    ];
    // The empty query is what a window opens on and the one that pages
    // furthest; the other two page across a filtered list, where the offset
    // counts matches rather than rows.
    for q in ["", "ext:rs", "size:>1k"] {
        for sort in ORDERS {
            for desc in [true, false] {
                let whole = f.paged(q, sort, desc, 0, 1_000);
                for &(offset, limit) in &[(0, 20), (19, 3), (200, 40), (499, 2), (900, 60)] {
                    if offset >= whole.len() {
                        continue;
                    }
                    let end = (offset + limit).min(whole.len());
                    assert_eq!(
                        f.paged(q, sort, desc, offset, limit),
                        whole[offset..end],
                        "{q:?} by {sort:?} (desc={desc}) at {offset}+{limit}"
                    );
                }
            }
        }
    }
}

/// Asking for one row at a time gives the same list as asking for all of them.
///
/// The same invariant from the other side, and it catches what a slice
/// comparison cannot: an offset applied per segment loses a row per segment
/// rather than shifting the window, so every page is subtly different but each
/// one on its own looks reasonable.
#[test]
fn a_list_read_one_row_at_a_time_is_the_list() {
    let f = Fixture::new(2_000, 200);
    for sort in [SortKey::Name, SortKey::Size, SortKey::Modified] {
        let whole = f.expected("ext:rs", sort, true, 60);
        let one: Vec<String> = (0..whole.len())
            .flat_map(|i| f.paged("ext:rs", sort, true, i, 1))
            .collect();
        assert_eq!(one, whole, "one row at a time by {sort:?}");
    }
}

#[test]
fn saving_over_a_file_leaves_one_row_however_the_source_names_it() {
    // **The duplicate bug, pinned.** Everything that saves carefully writes a
    // temporary file and renames it over the target: the path survives and
    // whatever the filesystem called the object does not. When the index keyed
    // rows on the source's identity, each save added a row and nothing removed
    // the old one — 267 rows at one path on the live index, growing for as
    // long as the service ran.
    //
    // So each round here hands over a *different* identity for the same path,
    // which is precisely what a rename-over produces, and the index has to
    // answer with one row every time.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    let path = "/home/u/Belgeler/rapor-2026.md";

    for round in 1..=8u64 {
        let e = Entry {
            // A new inode every save, as the filesystem would report it.
            id: EntryId::inode(SourceId(0), 66_310, round),
            path: path.into(),
            is_dir: false,
            meta: Meta {
                mtime: NOW + round as i64,
                size: round as i64 * 100,
                ..Meta::UNKNOWN
            },
        };
        index
            .apply(&mut std::iter::once(Change::Upsert(e)))
            .expect("apply");
        index.commit().expect("commit");

        let hits = index
            .search(&SearchRequest {
                query: parse_at("rapor-2026", NOW),
                page: Page::new(0, 50),
                ..Default::default()
            })
            .expect("search");
        assert_eq!(
            hits.total, 1,
            "after {round} saves the index holds {} rows for one path",
            hits.total
        );
        // And it is the newest one that survived, not the first.
        assert_eq!(hits.hits[0].meta.size, round as i64 * 100);
        assert_eq!(index.stats().expect("stats").entries, 1);
    }
}

#[test]
fn a_commit_that_cannot_be_written_keeps_what_it_was_carrying() {
    // **The failure that used to be silent and total.** The staged rows are
    // lifted out of the buffer before anything is written and are the only
    // copy; a full disk, a permission change or a volume going away used to
    // drop them on the floor while the engine was told the commit had
    // succeeded. Everything written since the last commit, gone, with a status
    // line saying all was well.
    //
    // A directory nothing may write to is the deterministic way to produce an
    // I/O failure; `ENOSPC` and `EIO` take the same path.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    index
        .apply(&mut std::iter::once(Change::Upsert(entry(
            "/home/u/hayatta-kalmali.md",
            NOW,
            1,
        ))))
        .expect("apply");

    let mut mode = std::fs::metadata(tmp.path()).expect("stat").permissions();
    let was = mode.mode();
    mode.set_mode(0o500);
    std::fs::set_permissions(tmp.path(), mode.clone()).expect("chmod");
    let refused = index.commit();
    mode.set_mode(was);
    std::fs::set_permissions(tmp.path(), mode).expect("chmod back");

    assert!(refused.is_err(), "a read-only directory took a write");
    assert_eq!(
        index.stats().expect("stats").entries,
        0,
        "nothing is indexed yet, which is the honest state"
    );

    // And now that it can be written, the row is still there to write.
    index.commit().expect("the retry");
    assert_eq!(
        index.stats().expect("stats").entries,
        1,
        "the entry was lost by the commit that failed"
    );
    let hits = index
        .search(&SearchRequest {
            query: parse_at("hayatta-kalmali", NOW),
            page: Page::new(0, 5),
            ..Default::default()
        })
        .expect("search");
    assert_eq!(hits.total, 1);
}

#[test]
fn one_source_cannot_sweep_away_another_source_rows() {
    // Two sources whose roots overlap — a home directory and a project
    // directory inside it, which is a configuration people really write. A
    // scan of one says "I walked here and did not find these rows"; that is a
    // statement about its own rows, and it was being applied to everyone's.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    let at = |source: u32, path: &str| Entry {
        id: EntryId::path_hash(SourceId(source), path),
        path: path.into(),
        is_dir: false,
        meta: Meta {
            mtime: NOW,
            size: 1,
            ..Meta::UNKNOWN
        },
    };
    index
        .apply(
            &mut [
                Change::Upsert(at(0, "/ortak/dosya.txt")),
                Change::Upsert(at(1, "/ortak/dosya.txt")),
            ]
            .into_iter(),
        )
        .expect("apply");
    index.commit().expect("commit");
    assert_eq!(index.stats().expect("stats").entries, 2);

    // Source 0 walks and finds nothing, so it sweeps its own row away.
    let g = index.begin_generation().expect("generation");
    let gone = index
        .sweep(SourceId(0), "/ortak", g, &PrefixSet::default())
        .expect("sweep");
    index.commit().expect("commit");
    assert_eq!(gone, 1, "a source swept more than its own rows");
    assert_eq!(
        index.stats().expect("stats").entries,
        1,
        "source 1's row was taken by source 0's walk"
    );
}

#[test]
fn a_removal_is_invisible_before_it_is_written() {
    // The one thing that may not wait for a commit. Deleting a file and still
    // seeing it reads as a broken program, so the removal takes effect in the
    // overlay first and in the files afterwards.
    let f = Fixture::new(2_000, 2_000);
    let victim = f.search("", SortKey::Modified, true, 1)[0].clone();

    f.index
        .apply(&mut std::iter::once(Change::RemoveSubtree {
            path: victim.clone(),
        }))
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
    let gone = index
        .sweep(SourceId(0), "/w", g, &PrefixSet::default())
        .expect("sweep");
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
fn a_sweep_takes_the_walked_directory_itself_and_spares_its_neighbour() {
    // The sweep used to answer this by rebuilding a path for every row that the
    // directory scope did not already accept and comparing strings. That is a
    // `String` per row per sweep — bearable when a sweep followed a full
    // rescan, and not bearable now that creating a folder queues a walk. What
    // replaces it has to give the same two answers:
    //
    //   * the swept directory's **own** row goes, even though it lives in its
    //     parent and carries the parent's number, so the range check misses it;
    //   * a sibling whose name merely starts with the same letters stays.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    let dir = |path: &str, ino: u64| {
        let mut e = entry(path, 100, ino);
        e.is_dir = true;
        e
    };
    let originals = vec![
        dir("/w/proj", 1),
        entry("/w/proj/inside.rs", 200, 2),
        entry("/w/proj/deep/deeper.rs", 210, 3),
        dir("/w/proj-414", 4),
        entry("/w/proj-414/other.rs", 220, 5),
        entry("/w/loose.rs", 230, 6),
    ];
    index
        .apply(&mut originals.into_iter().map(Change::Upsert))
        .expect("apply");
    index.commit().expect("commit");

    // A walk of `/w/proj` that finds nothing: the directory was removed.
    let g = index.begin_generation().expect("generation");
    let gone = index
        .sweep(SourceId(0), "/w/proj", g, &PrefixSet::default())
        .expect("sweep");
    assert_eq!(gone, 3, "the directory, its file and the one below it");

    let mut paths: Vec<String> = index
        .search(&SearchRequest {
            page: Page::new(0, 20),
            ..Default::default()
        })
        .expect("search")
        .hits
        .into_iter()
        .map(|h| h.path)
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        vec![
            "/w/loose.rs".to_owned(),
            "/w/proj-414".to_owned(),
            "/w/proj-414/other.rs".to_owned(),
        ]
    );
}

#[test]
fn deleting_a_tree_gives_the_same_answer_however_it_is_reported() {
    // A delete arrives as one removed path per file and one per directory, and
    // a commit lands once a second, so a few thousand of them are in one batch.
    // The obvious loop asks every row about every path, which on a million rows
    // was **2.24 seconds** of held write lock at four thousand paths — with a
    // search queued behind it for every one of those seconds.
    //
    // What the replacement must not do is change the answer. Reporting a
    // subtree as one prefix and reporting it as every path inside it are two
    // descriptions of the same delete, so the index has to end up in the same
    // place either way.
    let tree = || -> Vec<Entry> {
        let mut v = Vec::new();
        for pkg in 0..40 {
            let mut d = entry(
                &format!("/corpus/node_modules/pkg{pkg}"),
                10,
                100_000 + pkg as u64,
            );
            d.is_dir = true;
            v.push(d);
            for f in 0..25 {
                let n = pkg * 25 + f;
                v.push(entry(
                    &format!("/corpus/node_modules/pkg{pkg}/lib/file{n}.js"),
                    100 + n,
                    n as u64,
                ));
            }
        }
        // The sibling whose name merely starts the same, and something else
        // entirely. Neither may be touched.
        v.push(entry("/corpus/node_modules-old/keep.js", 5, 90_001));
        v.push(entry("/other/keep.rs", 6, 90_002));
        v
    };

    let run = |as_one_prefix: bool| -> Vec<String> {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        let entries = tree();
        index
            .apply(&mut entries.clone().into_iter().map(Change::Upsert))
            .expect("apply");
        index.commit().expect("commit");

        let removals: Vec<Change> = if as_one_prefix {
            vec![Change::RemoveSubtree {
                path: "/corpus/node_modules".into(),
            }]
        } else {
            // Leaves first and the directory last, which is the order a
            // filesystem reports them in.
            entries
                .iter()
                .filter(|e| e.path.starts_with("/corpus/node_modules/"))
                .map(|e| Change::RemoveSubtree {
                    path: e.path.clone(),
                })
                .chain(std::iter::once(Change::RemoveSubtree {
                    path: "/corpus/node_modules".into(),
                }))
                .collect()
        };
        index.apply(&mut removals.into_iter()).expect("apply");
        index.commit().expect("commit");

        let mut left: Vec<String> = index
            .search(&SearchRequest {
                page: Page::new(0, 5_000),
                ..Default::default()
            })
            .expect("search")
            .hits
            .into_iter()
            .map(|h| h.path)
            .collect();
        left.sort();
        left
    };

    let one = run(true);
    assert_eq!(
        one,
        vec![
            "/corpus/node_modules-old/keep.js".to_owned(),
            "/other/keep.rs".to_owned(),
        ],
        "the sibling with the longer name and the unrelated file both stay"
    );
    assert_eq!(run(false), one, "one prefix and a thousand have to agree");
}

#[test]
fn nothing_is_left_on_disk_for_a_segment_that_was_erased() {
    // A segment swept empty is both *touched* — its bitmap changed — and
    // *gone*, and the commit that erases it writes the bitmaps after releasing
    // the lock. So the file came back, for a segment nothing would ever open
    // and nothing would ever remove. Counted on the live index: **182 orphan
    // segments against 55 real ones**, almost all a lone `.alive`, and because
    // `bytes_on_disk` is the size of the directory the status line counted
    // them.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    let first: Vec<Entry> = (0..200)
        .map(|i| entry(&format!("/w/old{i}.rs"), 1_000 + i, i as u64))
        .collect();
    index
        .apply(&mut first.into_iter().map(Change::Upsert))
        .expect("apply");
    index.commit().expect("commit");

    // Remove every one of them, so the commit both *touches* that segment —
    // its bitmap changed — and *empties* it. `commit` is the path that matters:
    // it erases the files under the lock and writes the bitmaps after
    // releasing it.
    index
        .apply(
            &mut [
                Change::RemoveSubtree { path: "/w".into() },
                Change::Upsert(entry("/elsewhere/kept.rs", 9_000, 9_999)),
            ]
            .into_iter(),
        )
        .expect("apply");
    index.commit().expect("commit");
    assert_eq!(index.stats().expect("stats").entries, 1);

    // Checked against the files rather than the manifest, because `.names` is
    // what `Live::open` reads first: a segment number with any other part but
    // no `.names` is a number nothing can open.
    let files = segment_files(tmp.path());
    let real: std::collections::HashSet<&str> = files
        .iter()
        .filter(|n| n.ends_with(".names"))
        .map(|n| &n[..12])
        .collect();
    let strays: Vec<&String> = files.iter().filter(|n| !real.contains(&n[..12])).collect();
    assert!(
        strays.is_empty(),
        "files left for a segment nothing can open: {strays:?}"
    );
}

/// Every `seg-*` file in an index directory.
fn segment_files(dir: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .expect("read_dir")
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("seg-"))
        .collect();
    v.sort();
    v
}

#[test]
fn an_index_forgets_files_the_manifest_never_named() {
    // The other way orphans appear, and the one no ordering fixes: a segment is
    // eight files written one at a time, and a kill in the middle leaves a
    // partial set. Safe to remove because the manifest is written before
    // anything is unlinked and rewritten before anything is added, so a file it
    // does not name is a file nothing can reach.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    for (path, ino) in [("/w/a.rs", 1u64), ("/w/b.rs", 2)] {
        index
            .apply(&mut [Change::Upsert(entry(path, ino as i64, ino))].into_iter())
            .expect("apply");
        index.commit().expect("commit");
    }
    // Folds 1 and 2 away, so those two numbers are behind `next_segment` and
    // belong to nothing.
    index.maintain(Maintenance::Rebuild).expect("rebuild");
    assert_eq!(index.stats().expect("stats").segments, 1);
    drop(index);

    // A crash partway through writing a segment leaves exactly this: the first
    // files of the eight, and no manifest entry.
    std::fs::write(tmp.path().join("seg-00000001.names"), b"junk").expect("write");
    std::fs::write(tmp.path().join("seg-00000002.cols"), b"junk").expect("write");

    let index = NativeIndex::open_or_create(tmp.path()).expect("reopen");
    assert_eq!(index.stats().expect("stats").entries, 2);
    let left = segment_files(tmp.path());
    assert!(
        !left
            .iter()
            .any(|n| n.starts_with("seg-00000001.") || n.starts_with("seg-00000002.")),
        "files the manifest does not name should not survive an open: {left:?}"
    );
}

#[test]
fn a_rebuild_finishes_even_while_the_index_is_being_written_to() {
    // A fold releases the lock while it builds — that is what makes it safe
    // against searches — so a commit lands during it and appends a segment to
    // the very generation just folded. The rebuild loop saw a group of two
    // again and folded the whole index a second time, and a third. Measured on
    // 2.1 M entries: `scour maintain rebuild` ran for **over ten minutes** and
    // was still at 38 segments when it was given up on.
    //
    // The work is decided once now, so this has to finish while a writer is
    // going as hard as it can.
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    // Large enough that a fold takes long enough for a commit to land inside
    // it, which is the whole mechanism.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = Arc::new(NativeIndex::open_or_create(tmp.path()).expect("create"));
    for chunk in 0..8 {
        let part: Vec<Entry> = (0..20_000)
            .map(|i| {
                let n = chunk * 20_000 + i;
                entry(&format!("/w/some/where/file{n}.rs"), 1_000 + n, n as u64)
            })
            .collect();
        index
            .apply(&mut part.into_iter().map(Change::Upsert))
            .expect("apply");
        index.commit().expect("commit");
    }

    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let (index, stop) = (Arc::clone(&index), Arc::clone(&stop));
        std::thread::spawn(move || {
            let mut n = 100_000u64;
            while !stop.load(Ordering::Relaxed) {
                let _ = index.apply(
                    &mut [Change::Upsert(entry(
                        &format!("/w/churn{n}.rs"),
                        n as i64,
                        n,
                    ))]
                    .into_iter(),
                );
                let _ = index.commit();
                n += 1;
            }
        })
    };

    let (tx, rx) = std::sync::mpsc::channel();
    let rebuilder = {
        let index = Arc::clone(&index);
        std::thread::spawn(move || {
            let r = index.maintain(Maintenance::Rebuild);
            let _ = tx.send(r.is_ok());
        })
    };
    let finished = rx.recv_timeout(std::time::Duration::from_secs(60));
    stop.store(true, Ordering::Relaxed);
    writer.join().expect("writer");
    rebuilder.join().expect("rebuilder");
    assert_eq!(
        finished,
        Ok(true),
        "a rebuild has to stop chasing the commits that arrive during it"
    );
}

/// Folding a segment a walk has marked must not lose the files it marked.
///
/// **The invariant behind the guard in `fold`, and it had none.** A walk that
/// finds a row unchanged does not rewrite it; it marks it instead, and the mark
/// is keyed on the number of the segment the row is in. A fold consumes
/// segments and writes one with a *new* number — so folding a marked segment
/// leaves every one of its marks pointing at a segment that no longer exists.
/// The sweep that follows then cannot tell those rows were seen, and deletes
/// files that are on the disk. Silently, reporting success.
///
/// Checked by removal: with the guard taken out entirely this test fails and
/// `a_generation_is_never_folded_into_another_one` — which sounds like it
/// covers this and does not — still passes.
#[test]
fn folding_a_marked_segment_does_not_delete_what_it_marked() {
    let f = Fixture::new(6_000, 500);
    let held = f.index.stats().expect("stats").entries;
    assert!(
        f.index.stats().expect("stats").segments >= 10,
        "the fixture is supposed to be fragmented"
    );

    // A walk that finds everything exactly as it left it: nothing is written,
    // every row is marked instead.
    let g = f.index.begin_generation().expect("generation");
    let mut again = f.entries.iter().cloned().map(Change::Upsert);
    f.index.apply(&mut again).expect("apply");
    f.index.commit().expect("commit");

    // Housekeeping, arriving in the middle of it. This is not contrived — the
    // engine compacts on the commit boundary and a walk of a real disk holds a
    // generation open for seconds.
    f.index
        .maintain(Maintenance::Compact)
        .expect("compaction should decline, not fail");

    // And now the sweep, judging rows by marks the fold may have invalidated.
    f.index
        .sweep(SourceId(0), "", g, &PrefixSet::default())
        .expect("sweep");

    assert_eq!(
        f.index.stats().expect("stats").entries,
        held,
        "the sweep deleted rows the walk had seen — the marks were folded away"
    );
}

/// Two paths that hash to the same key are still two files.
///
/// **Not a hypothetical.** The identity table is keyed on half a digest — 32
/// bits — and the birthday bound on 2.2 million entries puts the expected
/// number of colliding pairs at about **576**. There are hundreds of them in
/// the index on this machine right now, and the only thing that makes them
/// harmless is that a probe answers with *candidates* which `Segment::is_at`
/// then confirms against the directory and the spelled name the row carries.
///
/// Deleting any of those three confirmations — name, directory, source — leaves
/// the whole suite passing, because a fixture never produces a collision by
/// accident. So this produces one on purpose: a short search over generated
/// paths until two of them agree on the key, which takes a few tens of
/// thousands of tries.
///
/// What it would look like if the confirmation went: a search finds a file and
/// hands back a different file's row. Not a crash, not a missing result — a
/// wrong one.
#[test]
fn two_paths_that_collide_on_the_key_are_still_two_files() {
    let key = |p: &str| (scour_core::path_digest(SourceId(0), p) >> 32) as u32;

    let mut seen: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
    let mut pair: Option<(String, String)> = None;
    for i in 0..4_000_000u64 {
        let path = format!("/w/carpisma/dosya-{i}.txt");
        if let Some(other) = seen.insert(key(&path), path.clone()) {
            pair = Some((other, path));
            break;
        }
    }
    let (a, b) = pair.expect("32 bitlik anahtarda carpisma bulunamadi");
    assert_ne!(a, b);
    assert_eq!(key(&a), key(&b), "the whole point of the pair");

    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    // Different sizes, so a row that answers for the wrong path says so.
    for (i, p) in [&a, &b].iter().enumerate() {
        let mut e = entry(p, NOW + i as i64, i as u64 + 1);
        e.meta.size = 100 + i as i64;
        index
            .apply(&mut std::iter::once(Change::Upsert(e)))
            .expect("apply");
    }
    index.commit().expect("commit");
    assert_eq!(
        index.stats().expect("stats").entries,
        2,
        "a collision must not merge two files into one row"
    );

    // **Saving over one of them must not take the other.** This is where the
    // confirmation earns its place: replacing a row means finding the old one
    // by identity, the identity table answers with *both* of these, and only
    // the name and directory comparison says which. Without it the wrong row
    // dies and a file disappears from the index while it is still on the disk.
    let mut again = entry(&a, NOW + 50, 1);
    again.meta.size = 999;
    index
        .apply(&mut std::iter::once(Change::Upsert(again)))
        .expect("apply");
    index.commit().expect("commit");
    assert_eq!(
        index.stats().expect("stats").entries,
        2,
        "saving over one of a colliding pair took the other"
    );

    // And each name finds its own row, with its own size.
    for (i, p) in [&a, &b].iter().enumerate() {
        let name = p.rsplit('/').next().expect("name");
        let hits = index
            .search(&SearchRequest {
                query: parse_at(name, NOW),
                sort: SortKey::Modified,
                descending: true,
                page: Page {
                    offset: 0,
                    limit: 10,
                    count_cap: 100,
                },
            })
            .expect("search")
            .hits;
        assert_eq!(hits.len(), 1, "{name} should find exactly itself");
        assert_eq!(&hits[0].path, *p);
        assert_eq!(
            hits[0].meta.size,
            if i == 0 { 999 } else { 101 },
            "{name} came back with the other file's row"
        );
    }
}

/// Two colliding paths that share a name are told apart by their directory.
///
/// The narrower half of the same guard. When a collision happens between two
/// paths with different names, comparing the name settles it — and that is the
/// common case, so a test that only covers it leaves the directory comparison
/// untested, which it was. This forces the case the name cannot settle: same
/// basename, different folder, same 32-bit key.
///
/// A search tool that answers `rapor.pdf` with the wrong `rapor.pdf` is worse
/// than one that answers nothing.
#[test]
fn colliding_paths_with_one_name_are_told_apart_by_their_folder() {
    let key = |p: &str| (scour_core::path_digest(SourceId(0), p) >> 32) as u32;
    let mut seen: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
    let mut pair: Option<(String, String)> = None;
    for i in 0..8_000_000u64 {
        let path = format!("/w/klasor-{i}/ayni-ad.txt");
        if let Some(other) = seen.insert(key(&path), path.clone()) {
            pair = Some((other, path));
            break;
        }
    }
    let (a, b) = pair.expect("ayni adli carpisma bulunamadi");
    assert_eq!(
        a.rsplit('/').next(),
        b.rsplit('/').next(),
        "the pair has to share a name or this tests nothing"
    );

    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    for (i, p) in [&a, &b].iter().enumerate() {
        let mut e = entry(p, NOW + i as i64, i as u64 + 1);
        e.meta.size = 100 + i as i64;
        index
            .apply(&mut std::iter::once(Change::Upsert(e)))
            .expect("apply");
    }
    index.commit().expect("commit");
    assert_eq!(index.stats().expect("stats").entries, 2);

    let mut again = entry(&a, NOW + 50, 1);
    again.meta.size = 999;
    index
        .apply(&mut std::iter::once(Change::Upsert(again)))
        .expect("apply");
    index.commit().expect("commit");

    assert_eq!(
        index.stats().expect("stats").entries,
        2,
        "saving over one folder's copy took the other folder's"
    );
    let hits = index
        .search(&SearchRequest {
            query: parse_at("ayni-ad", NOW),
            sort: SortKey::Size,
            descending: false,
            page: Page {
                offset: 0,
                limit: 10,
                count_cap: 100,
            },
        })
        .expect("search")
        .hits;
    assert_eq!(hits.len(), 2, "both copies must still be findable");
    assert_eq!(hits[0].meta.size, 101, "the untouched one kept its size");
    assert_eq!(hits[1].meta.size, 999, "and the saved one took the new one");
}

/// The same path under two sources stays two rows when one is saved over.
///
/// Written to cover the source comparison in `Segment::is_at`, and what it
/// found instead is that the comparison cannot change an answer — see the note
/// there. Kept because the *behaviour* is worth pinning whatever enforces it:
/// two volumes holding the same path is ordinary, and a save on one taking the
/// other's row would be a file vanishing from the index.
#[test]
fn one_path_under_two_sources_is_two_rows() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    let path = "/ortak/rapor.pdf";

    for src in [0u32, 1] {
        let mut e = entry(path, NOW + src as i64, src as u64 + 1);
        e.id = EntryId::path_hash(SourceId(src), path);
        e.meta.size = 100 + src as i64;
        index
            .apply(&mut std::iter::once(Change::Upsert(e)))
            .expect("apply");
    }
    index.commit().expect("commit");
    assert_eq!(
        index.stats().expect("stats").entries,
        2,
        "two sources holding one path are two rows"
    );

    let mut again = entry(path, NOW + 50, 1);
    again.id = EntryId::path_hash(SourceId(0), path);
    again.meta.size = 999;
    index
        .apply(&mut std::iter::once(Change::Upsert(again)))
        .expect("apply");
    index.commit().expect("commit");

    assert_eq!(
        index.stats().expect("stats").entries,
        2,
        "saving one source's copy took the other source's row"
    );
}

/// A pass that is never swept still lets housekeeping run afterwards.
///
/// **The leak behind the 241 segments, pinned at its source.** A walk that
/// could not look must not sweep — deleting on no evidence is how a directory
/// that lost its read permission loses its files too — but the sweep was the
/// only thing that ended a generation. So the pass stayed open, the notes it
/// made about unchanged rows stayed with it, and a noted segment cannot be
/// folded. One walk of a directory a watcher had just seen deleted was enough
/// to stop compaction for good.
///
/// The fix is that ending a pass and reconciling it are two things.
#[test]
fn a_pass_that_is_never_swept_does_not_block_compaction() {
    let f = Fixture::new(8_000, 800);
    let started = f.index.stats().expect("stats");
    let before = started.segments;
    assert!(before >= 10, "the fixture is supposed to be fragmented");

    // A walk that found everything exactly as it was: nothing written, every
    // row noted instead.
    let g = f.index.begin_generation().expect("generation");
    let mut again = f.entries.iter().cloned().map(Change::Upsert);
    f.index.apply(&mut again).expect("apply");
    f.index.commit().expect("commit");

    // And then it turns out the walk could not be trusted, so there is no
    // sweep — only an ending.
    f.index.abandon_generation(g).expect("abandon");

    f.index.maintain(Maintenance::Compact).expect("compact");
    let after = f.index.stats().expect("stats");
    assert!(
        after.segments < before,
        "compaction is still blocked by a pass nobody ended: {before} -> {}",
        after.segments
    );
    assert_eq!(after.entries, started.entries, "and it lost nothing");
}

/// Compaction still folds when every segment has a generation of its own.
///
/// **The shape a real machine produces, and the one nothing tested.** A walk
/// bumps the generation, a commit writes a segment, and a watcher on a busy
/// disk puts a walk between almost every pair of commits — so each segment ends
/// up alone in its own generation. Grouping candidates *by* that number then
/// never finds three to fold, and compaction dies: measured on the live index
/// at **241 segments across 205 generations, largest group 2**, against a
/// threshold of three. Every search opened all 241.
///
/// It is a one-way trap. Once the stamps are spread there is no state the index
/// can reach on its own where three of them agree again, so the count only ever
/// goes up. The existing compaction tests all build their fixture in one
/// generation, which is the one shape that cannot show it.
#[test]
fn compaction_still_folds_when_every_segment_has_its_own_generation() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");

    for i in 0..12u64 {
        let g = index.begin_generation().expect("generation");
        let e = entry(&format!("/w/dosya-{i}.txt"), NOW + i as i64, i + 1);
        index
            .apply(&mut std::iter::once(Change::Upsert(e)))
            .expect("apply");
        index.commit().expect("commit");
        // Closes the generation without judging anything: the scope holds no
        // rows, so this is the bump and nothing else.
        index
            .sweep(SourceId(0), "/baska-yer", g, &PrefixSet::default())
            .expect("sweep");
    }

    let before = index.stats().expect("stats").segments;
    assert!(
        before >= 10,
        "the fixture is supposed to be fragmented: {before}"
    );

    index.maintain(Maintenance::Compact).expect("compact");
    let after = index.stats().expect("stats");
    assert!(
        after.segments < before,
        "compaction folded nothing: {before} segments, each alone in its generation"
    );
    assert_eq!(after.entries, 12, "and it lost nothing doing it");
}

/// A compaction that is not allowed to fold still ends.
///
/// **This one spun a core on the live index for as long as a generation stayed
/// open.** Folding is refused while a walk has marked rows as seen — the marks
/// are keyed on segment numbers and folding renumbers them — and the refusal
/// returned `Ok(())`, which the caller could not tell from having done the
/// work. `maintain(Compact)` is `while let Some(head) = next_head() { fold }`
/// and it ends because a folded group stops qualifying, so a refusal that
/// changed nothing handed back the same group for ever: 99.7% of a core with
/// nothing happening, 224 segments that would not come down.
///
/// The assertion is that it *returns*. Before the fix this test does not fail,
/// it hangs — so it runs on its own thread with a deadline, and the failure is
/// a sentence rather than a timeout nobody can read.
#[test]
fn a_compaction_that_may_not_fold_still_finishes() {
    use std::sync::mpsc;

    let f = Fixture::new(8_000, 800);
    assert!(
        f.index.stats().expect("stats").segments >= 10,
        "the fixture is supposed to be fragmented"
    );

    // A generation with rows marked seen: exactly the state a start-up walk is
    // in for its first several seconds, and the one folding is refused in.
    f.index.begin_generation().expect("generation");
    let mut again = f.entries.iter().cloned().map(Change::Upsert);
    f.index.apply(&mut again).expect("apply");
    f.index.commit().expect("commit");

    let before = f.index.stats().expect("stats").segments;
    let (tx, rx) = mpsc::channel();
    std::thread::scope(|s| {
        s.spawn(|| {
            let r = f.index.maintain(Maintenance::Compact);
            let _ = tx.send(r.is_ok());
        });
        match rx.recv_timeout(std::time::Duration::from_secs(20)) {
            Ok(ok) => assert!(ok, "the compaction failed rather than declining"),
            Err(_) => {
                panic!("maintain(Compact) never returned: a refused fold is being retried for ever")
            }
        }
    });
    assert_eq!(
        f.index.stats().expect("stats").segments,
        before,
        "it declined, so nothing should have moved"
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
    assert_eq!(
        index
            .sweep(SourceId(0), "/w", g, &PrefixSet::default())
            .expect("sweep"),
        4
    );
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
fn two_sources_fold_into_one_segment_once_both_have_settled() {
    // The defect this exists to prevent, found on a real index rather than
    // reasoned about: a rebuild folded within a generation, each source's scan
    // takes its own, and a second source therefore meant two segments that no
    // amount of rebuilding could merge. 2,951,074 entries, 1,441,890 of them
    // unsorted, and `rapor` at 125 ms.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");

    for (root, ino) in [("/home/u", 0u64), ("/mnt/depo", 100)] {
        let g = index.begin_generation().expect("generation");
        for i in 0..4u64 {
            index
                .apply(&mut std::iter::once(Change::Upsert(entry(
                    &format!("{root}/f{i}.rs"),
                    100 + i as i64,
                    ino + i,
                ))))
                .expect("apply");
            index.commit().expect("commit");
        }
        // What ends a scan, and what makes the next fold safe: after this,
        // nothing is waiting to judge these rows.
        index
            .sweep(SourceId(0), root, g, &PrefixSet::default())
            .expect("sweep");
    }
    assert!(index.stats().expect("stats").segments > 2);

    index.maintain(Maintenance::Rebuild).expect("rebuild");
    let s = index.stats().expect("stats");
    assert_eq!(s.segments, 1, "two settled sources are one segment");
    assert_eq!(s.entries, 8);
    assert_eq!(s.unsorted_entries, 0);

    // And both sources are still there and still answerable.
    let paths: Vec<String> = index
        .search(&SearchRequest {
            page: Page::new(0, 20),
            ..Default::default()
        })
        .expect("search")
        .hits
        .into_iter()
        .map(|h| h.path)
        .collect();
    assert_eq!(paths.len(), 8);
    assert_eq!(paths.iter().filter(|p| p.starts_with("/mnt")).count(), 4);

    // A later scan of one source still removes only that source's missing
    // files — the merged segment did not cost the sweep its precision.
    let g = index.begin_generation().expect("generation");
    index
        .apply(&mut std::iter::once(Change::Upsert(entry(
            "/mnt/depo/f0.rs",
            999,
            100,
        ))))
        .expect("apply");
    index.commit().expect("commit");
    assert_eq!(
        index
            .sweep(SourceId(0), "/mnt/depo", g, &PrefixSet::default())
            .expect("sweep"),
        3
    );
    let left = index.stats().expect("stats").entries;
    assert_eq!(left, 5, "four from home and the one depo file that remains");
}

#[test]
fn a_rebuild_drops_the_rows_nobody_can_see() {
    let f = Fixture::new(4_000, 1_000);
    // Files, not directories: a removal takes everything under the path, and
    // the generated tree lists its directories first.
    let doomed: Vec<String> = f
        .entries
        .iter()
        .filter(|e| !e.is_dir)
        .take(500)
        .map(|e| e.path.clone())
        .collect();
    f.index
        .apply(
            &mut doomed
                .into_iter()
                .map(|path| Change::RemoveSubtree { path }),
        )
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
        let victim = fs
            .entries
            .iter()
            .find(|e| !e.is_dir)
            .expect("a file")
            .path
            .clone();
        index
            .apply(&mut std::iter::once(Change::RemoveSubtree { path: victim }))
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
            by: vec![FacetBy::Ext { top: 5 }],
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
            by: vec![FacetBy::Dir {
                path: "/home/u".into(),
                top: 10,
            }],
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

#[test]
fn a_segment_a_sweep_emptied_stops_costing_anything() {
    // The bug this exists for, found on a real index rather than in a test: a
    // rescan stamps a new generation, the sweep kills every row of the old one,
    // and the emptied segment stays in the list. Every query then reads it end
    // to end — 1,204,270 rows to produce nothing — until a rebuild happens to
    // remove it.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    let first: Vec<Entry> = (0..2_000)
        .map(|i| entry(&format!("/w/old{i}.rs"), 1_000 + i, i as u64))
        .collect();
    index
        .apply(&mut first.into_iter().map(Change::Upsert))
        .expect("apply");
    index.commit().expect("commit");
    assert_eq!(index.stats().expect("stats").segments, 1);

    // A rescan that finds a different set entirely.
    let g = index.begin_generation().expect("generation");
    let second: Vec<Entry> = (0..10)
        .map(|i| entry(&format!("/w/new{i}.rs"), 5_000 + i, 10_000 + i as u64))
        .collect();
    index
        .apply(&mut second.into_iter().map(Change::Upsert))
        .expect("apply");
    index
        .sweep(SourceId(0), "/w", g, &PrefixSet::default())
        .expect("sweep");

    let s = index.stats().expect("stats");
    assert_eq!(s.entries, 10);
    assert_eq!(s.segments, 1, "the emptied segment should be gone");

    let res = index
        .search(&SearchRequest {
            page: Page::new(0, 40),
            ..Default::default()
        })
        .expect("search");
    assert_eq!(res.hits.len(), 10);
    assert!(
        res.rows_visited <= 128,
        "read {} rows for ten entries",
        res.rows_visited
    );
}

#[test]
fn deleted_rows_stop_being_walked_before_they_are_erased() {
    // The same idea one level down: a block with no live row is skipped on the
    // strength of sixteen bytes of the bitmap, whether or not the segment as a
    // whole still has something in it.
    let f = Fixture::new(8_000, 8_000);
    let doomed: Vec<String> = f
        .entries
        .iter()
        .filter(|e| !e.is_dir)
        .take(f.entries.len() / 2)
        .map(|e| e.path.clone())
        .collect();
    f.index
        .apply(
            &mut doomed
                .into_iter()
                .map(|path| Change::RemoveSubtree { path }),
        )
        .expect("apply");
    f.index.commit().expect("commit");

    let res = f
        .index
        .search(&SearchRequest {
            query: parse_at("zzzznothing", NOW),
            page: Page {
                offset: 0,
                limit: 40,
                count_cap: 10_000_000,
            },
            ..Default::default()
        })
        .expect("search");
    assert_eq!(res.hits.len(), 0);
    assert!(
        res.rows_visited * 2 < f.entries.len() as u64,
        "visited {} of {}",
        res.rows_visited,
        f.entries.len()
    );
}

#[test]
fn an_index_from_another_version_is_outdated_and_not_damaged() {
    // The difference matters to whoever is watching: nothing is lost, because
    // an index is derived from the filesystem in its entirety. The service
    // acts on it by discarding and rescanning, and this is the machinery that
    // lets it — `IndexCorrupt` would be a lie and would look like one.
    let tmp = tempfile::tempdir().expect("tmpdir");
    {
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        let mut it = (0..50).map(|i| Change::Upsert(entry(&format!("/a/f{i}.rs"), NOW, i)));
        index.apply(&mut it).expect("apply");
        index.commit().expect("commit");
        assert_eq!(index.stats().expect("stats").entries, 50);
    }

    // Rewritten to version zero rather than to "the previous one", so that
    // this test does not have to be edited every time the format moves.
    let manifest = tmp.path().join("native-index.json");
    let written = std::fs::read_to_string(&manifest).expect("manifest");
    let at = written
        .find("\"format\":")
        .expect("the manifest changed shape");
    let end = at + written[at..].find(',').expect("a field after format");
    let older = format!("{}\"format\": 0{}", &written[..at], &written[end..]);
    std::fs::write(&manifest, older).expect("write");

    match NativeIndex::open_or_create(tmp.path()) {
        Err(scour_core::Error::IndexOutdated { found, expected }) => {
            assert_eq!(found, 0);
            assert!(expected > 0, "the current format is not a version");
        }
        other => panic!("expected an outdated index, got {other:?}"),
    }

    NativeIndex::discard(tmp.path()).expect("discard");
    let index = NativeIndex::open_or_create(tmp.path()).expect("reopen");
    assert_eq!(index.stats().expect("stats").entries, 0);
    assert!(
        std::fs::read_dir(tmp.path())
            .expect("read_dir")
            .flatten()
            .all(|e| e.file_name() != "seg-00000001.names"),
        "the old segments are still on disk"
    );
    // Discarding what is not there is the state being asked for, not an error.
    NativeIndex::discard(&tmp.path().join("nothing-here")).expect("discard nothing");
}

#[test]
fn relevance_puts_the_near_copy_first_however_many_segments_there_are() {
    // Relevance is the one order `brute_force` does not model — scoring belongs
    // to the index — so this is where the two halves of it are checked against
    // each other. A segment reads a directory's distance from a table built
    // when it was written; the merge across segments recomputes it from the
    // path, because a `Hit` carries no directory number. Those are two
    // implementations of one number, and nothing else would notice them
    // drifting apart.
    //
    // Every one of these is named `main.rs`, so the name score is identical and
    // the distance is the whole ordering.
    let want = [
        "/home/u/Projeler/app/main.rs",
        "/home/u/Projeler/app/deeper/still/main.rs",
        "/home/u/Projeler/app/target/debug/main.rs",
        "/home/u/.cargo/registry/src/crates.io/lzma-0.1/main.rs",
    ];
    for chunk in [1, 2, 4] {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        // Written oldest-first in a shuffled order, so neither the stored order
        // nor the insertion order can produce the expected answer by accident.
        let written = [want[2], want[0], want[3], want[1]];
        for (i, part) in written.chunks(chunk).enumerate() {
            let base = i * chunk;
            let mut it = part
                .iter()
                .enumerate()
                .map(|(j, p)| {
                    let n = (base + j) as u64;
                    Change::Upsert(entry(p, NOW - n as i64, 900 + n))
                })
                .collect::<Vec<_>>()
                .into_iter();
            index.apply(&mut it).expect("apply");
            index.commit().expect("commit");
        }
        let got: Vec<String> = index
            .search(&SearchRequest {
                query: parse_at("main", NOW),
                sort: SortKey::Relevance,
                descending: true,
                page: Page {
                    offset: 0,
                    limit: 10,
                    count_cap: 10_000,
                },
            })
            .expect("search")
            .hits
            .into_iter()
            .map(|h| h.path)
            .collect();
        assert_eq!(got, want, "with {chunk} entries a segment");
    }
}

#[test]
fn a_file_with_two_names_is_not_two_files_worth_of_disk() {
    // Both names are rows — that is what makes them findable, and it is right.
    // What must not follow is that the disk report counts the blocks twice.
    // `usage.rs` said no work was needed here because an inode was one row;
    // identity became the path and that premise went with it.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    let linked = |path: &str| Entry {
        id: EntryId::path_hash(SourceId(0), path),
        path: path.into(),
        is_dir: false,
        meta: Meta {
            mtime: NOW,
            size: 4096,
            disk: 4096,
            links: 2,
            ..Meta::UNKNOWN
        },
    };
    index
        .apply(
            &mut [
                Change::Upsert(Entry {
                    is_dir: true,
                    ..linked("/w")
                }),
                Change::Upsert(linked("/w/ad-bir.bin")),
                Change::Upsert(linked("/w/ad-iki.bin")),
            ]
            .into_iter(),
        )
        .expect("apply");
    index.commit().expect("commit");

    let usage = index
        .usage(&scour_core::UsageRequest {
            path: "/w".into(),
            top: 5,
        })
        .expect("usage");
    assert_eq!(usage.root.files, 2, "both names are findable");
    assert_eq!(
        usage.root.disk, 4096,
        "one file's blocks were counted once per name"
    );
    assert_eq!(usage.root.bytes, 4096);
}

#[test]
fn a_directory_is_not_a_file_of_type_grup() {
    // Real names, off a volume written from Windows, where a dot inside a
    // folder name is ordinary. `ext:` asks what kind of file a row is, and a
    // folder is not one — asked of `Trabzon 2. Grup` the old answer was that
    // it was a file of type ` grup`. But `*.rs` asks about the *name*, and a
    // folder called `mod.rs` has that name, so the two part company here and
    // the test is what keeps them apart.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    let entries: Vec<Entry> = [
        ("/d/Trabzon 2. Grup", true),
        ("/d/TRABZON.MÜZEKKERE.CEVABI", true),
        ("/d/mod.rs", true),
        ("/d/main.rs", false),
        ("/d/rapor.grup", false),
    ]
    .iter()
    .enumerate()
    .map(|(i, (path, is_dir))| Entry {
        is_dir: *is_dir,
        ..entry(path, NOW, i as u64 + 1)
    })
    .collect();
    let mut it = entries.iter().cloned().map(Change::Upsert);
    index.apply(&mut it).expect("apply");
    index.commit().expect("commit");
    let f = Fixture {
        _tmp: tmp,
        index,
        entries,
    };

    assert_eq!(
        f.search("ext:rs", SortKey::Name, false, 50),
        vec!["/d/main.rs"],
        "ext: is about files, and the directory called mod.rs is not one"
    );
    assert_eq!(
        f.search("ext:grup", SortKey::Name, false, 50),
        vec!["/d/rapor.grup"],
        "`Trabzon 2. Grup` is a folder, not a file of type ` grup`"
    );
    assert_eq!(
        f.search("*.rs", SortKey::Name, false, 50),
        vec!["/d/main.rs", "/d/mod.rs"],
        "a glob is about the name, and a directory can have that name"
    );

    // And the slow answer agrees, which is what makes the above the rule
    // rather than this index's opinion of it.
    for q in ["ext:rs", "ext:grup", "ext:cevabi", "*.rs", "*.grup"] {
        f.check(q, SortKey::Name, false);
    }
}

#[test]
fn the_empty_query_counts_what_is_live_without_walking_for_it() {
    // Nothing to test means every live row matches, and how many that is is a
    // number each segment already keeps. The walk was visiting all of them to
    // arrive at it: 1.233 s on a 2.09 M-row index, for the query a window
    // shows the moment it opens.
    //
    // What this has to get right is *live*, not stored. A removed row is
    // still in the segment, and reading a total off the wrong counter would
    // be a fast wrong answer — the worst kind, and invisible until somebody
    // notices the number is bigger than the list.
    let f = Fixture::new(4_000, 500);
    let live = f.entries.len() as u64;

    let count = |index: &NativeIndex, cap: u32| -> (u64, bool) {
        let r = index
            .search(&SearchRequest {
                query: parse_at("", NOW),
                sort: SortKey::Modified,
                descending: true,
                page: Page {
                    offset: 0,
                    limit: 0,
                    count_cap: cap,
                },
            })
            .expect("count");
        (r.total, r.capped)
    };

    assert_eq!(count(&f.index, u32::MAX).0, live, "every row, and no more");

    // Take four leaves away and ask again. Leaves, because
    // on a directory takes everything under it — which is correct, and would
    // make this test about something else.
    let doomed: Vec<String> = f
        .entries
        .iter()
        .filter(|e| !e.is_dir)
        .filter(|e| {
            !f.entries
                .iter()
                .any(|o| o.path.starts_with(&format!("{}/", e.path)))
        })
        .take(4)
        .map(|e| e.path.clone())
        .collect();
    assert_eq!(doomed.len(), 4, "four leaves to remove");
    for path in &doomed {
        f.index
            .apply(&mut std::iter::once(Change::RemoveSubtree {
                path: path.clone(),
            }))
            .expect("remove");
    }
    f.index.commit().expect("commit");
    assert_eq!(
        count(&f.index, u32::MAX).0,
        live - doomed.len() as u64,
        "a removed row is not live and must not be counted"
    );

    // And the cap still means what it meant.
    let (total, capped) = count(&f.index, 100);
    assert!(capped, "a cap below the total still reports capped");
    assert_eq!(total, 100, "and reports the cap, not the true total");
}

/// A removal that arrives before anything was ever indexed must not vanish.
///
/// `kill_leaves` keys its lookup on a source, and an index that has been
/// handed no rows this session knows none — reopening is exactly that state.
/// The path has to fall through to the walk rather than be dropped between
/// the two. The first version dropped it, and nothing failed: the file simply
/// stayed in the index for ever.
#[test]
fn a_removal_before_the_first_upsert_still_takes_the_row() {
    fn row(path: &str) -> Entry {
        Entry {
            id: EntryId::path_hash(SourceId(0), path),
            path: path.into(),
            is_dir: false,
            meta: Meta::UNKNOWN,
        }
    }
    fn found(index: &NativeIndex, q: &str) -> usize {
        index
            .search(&SearchRequest {
                query: parse_at(q, NOW),
                sort: SortKey::Relevance,
                descending: false,
                page: Page {
                    offset: 0,
                    limit: 50,
                    count_cap: 100,
                },
            })
            .expect("search")
            .hits
            .len()
    }

    let tmp = tempfile::tempdir().expect("tmpdir");
    {
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        let mut it = vec![
            Change::Upsert(row("/a/keepme.txt")),
            Change::Upsert(row("/a/goneme.txt")),
        ]
        .into_iter();
        index.apply(&mut it).expect("apply");
        index.commit().expect("commit");
    }

    let index = NativeIndex::open_or_create(tmp.path()).expect("reopen");
    index
        .apply(&mut std::iter::once(Change::RemoveSubtree {
            path: "/a/goneme.txt".into(),
        }))
        .expect("apply");
    index.commit().expect("commit");

    assert_eq!(
        found(&index, "goneme"),
        0,
        "a removal with no source to key on has to reach the walk, not be dropped"
    );
    assert_eq!(found(&index, "keepme"), 1, "and take nothing else");
}

/// A rescan that changed nothing writes nothing either.
///
/// The other half of the pair below. That one says an untouched rescan must not
/// *remove* anything; this one says it must not *add* anything — and the two
/// failures look nothing alike. Recognising the unchanged rows and then writing
/// what is left anyway is the shape the first version had: the batch that
/// overflowed the buffer was almost all rows the index already held, and the
/// two or three survivors still became a segment. A walk of 870,000 entries
/// left nine of them, one a batch, doubling what every search reads and owing
/// a compaction for the rest.
///
/// The segment count, not the entry count, because the entry count was right
/// the whole time.
#[test]
fn a_rescan_that_changed_nothing_adds_no_segment() {
    let f = Fixture::new(6_000, 500);
    let before = f.index.stats().expect("stats").segments;
    assert!(before > 1, "several segments, or this asserts nothing");

    let g = f.index.begin_generation().expect("generation");
    let mut again = f.entries.iter().cloned().map(Change::Upsert);
    let report = f.index.apply(&mut again).expect("apply");
    f.index.commit().expect("commit");
    f.index
        .sweep(SourceId(0), "", g, &PrefixSet::default())
        .expect("sweep");

    let after = f.index.stats().expect("stats");
    assert_eq!(after.segments, before, "a segment a batch is the bug");
    assert_eq!(
        after.entries,
        f.entries.len() as u64,
        "and every row is still there"
    );
    // **Nothing new was written, and the count that says so arrives late.**
    // Sparing happens when a batch is flushed; this batch fitted in the buffer,
    // so nothing flushed it until the commit — after `apply` had returned its
    // report. The number therefore reaches whoever calls next, which is the
    // documented shape of `ApplyReport::unchanged` and is pinned here because a
    // drain that stopped working would otherwise be invisible.
    assert_eq!(report.unchanged, 0, "this batch had not been flushed yet");
    let mut one = std::iter::once(Change::Upsert(f.entries[0].clone()));
    let later = f.index.apply(&mut one).expect("apply");
    assert!(
        later.unchanged >= before as u64,
        "the commit's sparing never reached a report: {}",
        later.unchanged
    );
}

/// A scan that finds every file exactly as it left it must delete nothing.
///
/// **This is the guard on the most dangerous path in the index.** A sweep
/// decides a row is gone because the walk did not stamp it, and stamping is
/// per segment — so any change that lets an unchanged file skip being written
/// has to keep it stamped some other way, or a rescan that found nothing wrong
/// empties the index and reports success. The failure is silent and total.
///
/// Written before the optimisation it guards, and it passes both before and
/// after by construction: what it asserts is the behaviour, not the mechanism.
#[test]
fn a_rescan_that_finds_nothing_changed_removes_nothing() {
    fn row(path: &str, mtime: i64) -> Entry {
        let mut meta = Meta::UNKNOWN;
        meta.size = 11;
        meta.mtime = mtime;
        Entry {
            id: EntryId::path_hash(SourceId(0), path),
            path: path.into(),
            is_dir: false,
            meta,
        }
    }
    let files = ["/w/a.txt", "/w/b.txt", "/w/deep/c.txt"];

    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");

    let g = index.begin_generation().expect("generation");
    let mut it = files
        .iter()
        .map(|p| Change::Upsert(row(p, 1000)))
        .collect::<Vec<_>>()
        .into_iter();
    index.apply(&mut it).expect("apply");
    index.commit().expect("commit");
    index
        .sweep(SourceId(0), "/w", g, &scour_core::PrefixSet::default())
        .expect("sweep");
    assert_eq!(index.stats().expect("stats").entries, 3, "the first pass");

    // The same walk again: same paths, same metadata, nothing on disk moved.
    let g = index.begin_generation().expect("generation");
    let mut it = files
        .iter()
        .map(|p| Change::Upsert(row(p, 1000)))
        .collect::<Vec<_>>()
        .into_iter();
    index.apply(&mut it).expect("apply");
    index.commit().expect("commit");
    let gone = index
        .sweep(SourceId(0), "/w", g, &scour_core::PrefixSet::default())
        .expect("sweep");

    assert_eq!(gone, 0, "a pass that saw everything must remove nothing");
    assert_eq!(
        index.stats().expect("stats").entries,
        3,
        "and the rows have to still be there"
    );
}

/// The other half, so the guard cannot be satisfied by never sweeping at all.
#[test]
fn a_rescan_that_stops_seeing_a_file_still_removes_it() {
    fn row(path: &str) -> Entry {
        Entry {
            id: EntryId::path_hash(SourceId(0), path),
            path: path.into(),
            is_dir: false,
            meta: Meta::UNKNOWN,
        }
    }
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");

    let g = index.begin_generation().expect("generation");
    let mut it = vec![
        Change::Upsert(row("/w/stays.txt")),
        Change::Upsert(row("/w/goes.txt")),
    ]
    .into_iter();
    index.apply(&mut it).expect("apply");
    index.commit().expect("commit");
    index
        .sweep(SourceId(0), "/w", g, &scour_core::PrefixSet::default())
        .expect("sweep");

    let g = index.begin_generation().expect("generation");
    let mut it = std::iter::once(Change::Upsert(row("/w/stays.txt")));
    index.apply(&mut it).expect("apply");
    index.commit().expect("commit");
    index
        .sweep(SourceId(0), "/w", g, &scour_core::PrefixSet::default())
        .expect("sweep");

    assert_eq!(
        index.stats().expect("stats").entries,
        1,
        "the file the walk stopped seeing has to go"
    );
}

/// A folder's size agrees with the report, and keeps agreeing.
///
/// **The two must never drift**, because the interface prints them beside each
/// other: the column comes from prefix sums over directory numbers and the
/// report comes from `usage.rs`'s rollup, which are two entirely different
/// routes to one number. This holds them together across everything that can
/// move the answer — several segments, a hard-linked file, a removal, and a
/// compaction that renumbers the segments the cache is keyed on.
#[test]
fn a_folder_weighs_what_the_report_says_it_weighs() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");

    let file = |path: &str, ino: u64, disk: i64, links: i64| Entry {
        id: EntryId::inode(SourceId(0), 66_310, ino),
        path: path.into(),
        is_dir: false,
        meta: Meta {
            mtime: NOW,
            size: disk,
            disk,
            links,
            ..Meta::UNKNOWN
        },
    };
    let dir = |path: &str, ino: u64| Entry {
        id: EntryId::inode(SourceId(0), 66_310, ino),
        path: path.into(),
        is_dir: true,
        // A directory's own `st_size` is its entry table, and counting it
        // would report bookkeeping as content. Made large here so that a
        // version which counted it could not pass.
        meta: Meta {
            mtime: NOW,
            size: 99_000,
            disk: 99_000,
            ..Meta::UNKNOWN
        },
    };

    // Two commits, so the answer has to be summed across segments — and the
    // sibling that sorts *between* a folder and its children, because `-` is
    // 0x2D and `/` is 0x2F.
    let first = vec![
        dir("/p", 1),
        dir("/p/a", 2),
        dir("/p-yedek", 3),
        file("/p/one", 10, 1_000, 1),
        file("/p/a/two", 11, 2_000, 1),
        file("/p-yedek/other", 12, 8_000, 1),
    ];
    let second = vec![
        dir("/p/a/deep", 4),
        file("/p/a/deep/three", 13, 4_000, 1),
        // One file, two names: each row carries half, so a tree holding both
        // is charged 6,000 once rather than 12,000.
        file("/p/a/deep/link-a", 14, 6_000, 2),
        file("/p/a/deep/link-b", 15, 6_000, 2),
    ];
    for batch in [first, second] {
        index
            .apply(&mut batch.into_iter().map(Change::Upsert))
            .expect("apply");
        index.commit().expect("commit");
    }

    let paths = ["/p".to_string(), "/p/a".into(), "/p-yedek".into()];
    let agree = |when: &str| {
        let fast: Vec<(u64, u64)> = index
            .subtree_sizes(&paths)
            .expect("sizes")
            .into_iter()
            .map(|s| s.expect("the native index always has an answer"))
            .collect();
        for (path, (disk, files)) in paths.iter().zip(&fast) {
            let report = index
                .usage(&scour_core::UsageRequest {
                    path: path.clone(),
                    top: 0,
                })
                .expect("usage");
            assert_eq!(
                (*disk, *files),
                (report.root.disk, report.root.files),
                "{path} disagrees with the report {when}"
            );
        }
        fast
    };

    let fresh = agree("when fresh");
    // The numbers themselves, so that "they agree" cannot mean "both wrong".
    // /p holds 1,000 + 2,000 + 4,000 + 3,000 + 3,000 — the last two being the
    // two halves of the hard-linked six.
    assert_eq!(fresh[0], (13_000, 5), "/p");
    assert_eq!(fresh[1], (12_000, 4), "/p/a");
    assert_eq!(fresh[2], (8_000, 1), "/p-yedek");

    // A removal moves the alive bits without changing a byte of the segment,
    // which is exactly the case the cache stamp exists for. Without the stamp
    // this still answers 13,000.
    //
    // **The commit is not incidental.** A removal takes effect for *searches*
    // at once — `hidden_prefixes` — but the rows stay alive until they are
    // written away, so until then a subtree still weighs what it weighed. The
    // report does the same, which is what matters here: the two agree at every
    // step, and the window they are both stale in is one commit interval.
    index
        .apply(&mut std::iter::once(Change::RemoveSubtree {
            path: "/p/one".into(),
        }))
        .expect("remove");
    assert_eq!(
        index.subtree_sizes(&paths).expect("sizes")[0],
        Some((13_000, 5)),
        "a hidden-but-not-yet-written removal is not a size change"
    );
    index.commit().expect("commit");
    let after = agree("after a removal");
    assert_eq!(after[0], (12_000, 4), "the removed file is still counted");

    // And compaction, which folds segments away and hands out new numbers —
    // the numbers the cache is keyed on.
    index.maintain(Maintenance::Rebuild).expect("rebuild");
    let folded = agree("after a rebuild");
    assert_eq!(folded[0], (12_000, 4));
}
