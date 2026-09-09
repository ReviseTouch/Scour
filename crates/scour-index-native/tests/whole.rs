//! The index as a whole, checked against the truth.
//!
//! `smoke.rs` checks one segment; this checks several, plus unwritten removals,
//! a generation sweep, a rebuild and a restart, every answer compared against
//! [`scour_mock::brute_force`], which cannot take a shortcut.

use std::os::unix::fs::PermissionsExt;

use scour_core::{
    Ast, Change, Entry, EntryId, FacetBy, FacetRequest, Index, Maintenance, Meta, Page, PrefixSet,
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
    // Eight segments: the merge, the page and the count must give the same
    // list a single pass over the entries would.
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

/// Ordering by path is a stored order, and it has to be the same order.
/// Checked in both directions and at an offset — right at the front and wrong
/// further in is what a page of the first fifty would not show.
#[test]
fn the_stored_path_order_is_the_order_brute_force_gives() {
    let f = Fixture::new(16_000, 2_000);
    for q in [
        "",
        "kind:code",
        "size:>1k",
        "dm:30d",
        "under:/home/u/Projeler",
        "parent:/home/u",
        "!kind:code",
        "is:dir",
    ] {
        for desc in [false, true] {
            f.check(q, SortKey::Path, desc);
            // Past the first page and its boundary. The reference takes a
            // limit and not an offset, so it is asked for the whole prefix.
            let whole = brute_force(&f.entries, &parse_at(q, NOW), SortKey::Path, desc, 400);
            if whole.len() > 300 {
                let want: Vec<String> = whole[300..].iter().map(|h| h.path.clone()).collect();
                assert_eq!(
                    f.paged(q, SortKey::Path, desc, 300, 100),
                    want,
                    "{q:?} by path (desc={desc}) at 300+100 disagrees with brute force"
                );
            }
        }
    }

    // And the point of it: the walk stops. Without the stored order the answer
    // is identical and every row of every segment is visited.
    let res = f
        .index
        .search(&SearchRequest {
            query: parse_at("", NOW),
            sort: SortKey::Path,
            descending: false,
            page: Page {
                offset: 0,
                limit: 50,
                count_cap: 200,
            },
        })
        .expect("search");
    assert!(
        res.rows_visited < f.entries.len() as u64 / 4,
        "a stored path order should read a page, not a corpus: {} rows of {}",
        res.rows_visited,
        f.entries.len()
    );
}

/// Ordering by name is a stored order too. These are the queries that may
/// stream it: none has to read a name to decide whether a row matches.
#[test]
fn the_stored_name_order_is_the_order_brute_force_gives() {
    let f = Fixture::new(16_000, 2_000);
    for q in [
        "",
        "kind:code",
        "size:>1k",
        "dm:30d",
        "under:/home/u/Projeler",
        "parent:/home/u",
        "!kind:code",
        "is:dir",
    ] {
        for desc in [false, true] {
            f.check(q, SortKey::Name, desc);
            let whole = brute_force(&f.entries, &parse_at(q, NOW), SortKey::Name, desc, 400);
            if whole.len() > 300 {
                let want: Vec<String> = whole[300..].iter().map(|h| h.path.clone()).collect();
                assert_eq!(
                    f.paged(q, SortKey::Name, desc, 300, 100),
                    want,
                    "{q:?} by name (desc={desc}) at 300+100 disagrees with brute force"
                );
            }
        }
    }

    let res = f
        .index
        .search(&SearchRequest {
            query: parse_at("", NOW),
            sort: SortKey::Name,
            descending: false,
            page: Page {
                offset: 0,
                limit: 50,
                count_cap: 200,
            },
        })
        .expect("search");
    assert!(
        res.rows_visited < f.entries.len() as u64 / 4,
        "a stored name order should read a page, not a corpus: {} rows of {}",
        res.rows_visited,
        f.entries.len()
    );
}

/// Extension order is persisted for the broad list the GUI shows. Extensions
/// have few values, so the order must hold both the primary key and the public
/// newest-first/path-first tie order while still stopping after a page.
#[test]
fn the_stored_extension_order_is_the_order_brute_force_gives() {
    let f = Fixture::new(16_000, 2_000);
    for q in [
        "",
        "kind:code",
        "size:>1k",
        "dm:30d",
        "under:/home/u/Projeler",
        "parent:/home/u",
        "!kind:code",
        "is:dir",
    ] {
        for desc in [false, true] {
            f.check(q, SortKey::Ext, desc);
            let whole = brute_force(&f.entries, &parse_at(q, NOW), SortKey::Ext, desc, 400);
            if whole.len() > 300 {
                let want: Vec<String> = whole[300..].iter().map(|h| h.path.clone()).collect();
                assert_eq!(
                    f.paged(q, SortKey::Ext, desc, 300, 100),
                    want,
                    "{q:?} by extension (desc={desc}) at 300+100 disagrees with brute force"
                );
            }
        }
    }

    let res = f
        .index
        .search(&SearchRequest {
            query: parse_at("", NOW),
            sort: SortKey::Ext,
            descending: false,
            page: Page {
                offset: 0,
                limit: 50,
                count_cap: 200,
            },
        })
        .expect("search");
    assert!(
        res.rows_visited < f.entries.len() as u64 / 4,
        "a stored extension order should read a page, not a corpus: {} rows of {}",
        res.rows_visited,
        f.entries.len()
    );
}

/// Reversing a name order must not reverse the rows that share one name.
/// `Cargo.toml`, `index.js` and `README` tie throughout a real tree.
#[test]
fn descending_stored_name_order_keeps_the_public_tie_order() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    let variants = ["REPORT.TXT", "Report.Txt", "report.txt"];
    let entries: Vec<Entry> = (0..600u64)
        .map(|i| {
            entry(
                &format!("/d{i:04}/{}", variants[i as usize % variants.len()]),
                NOW - (i % 19) as i64,
                10_000 + i,
            )
        })
        .collect();
    for part in entries.chunks(100) {
        index
            .apply(&mut part.iter().cloned().map(Change::Upsert))
            .expect("apply");
        index.commit().expect("commit");
    }

    for desc in [false, true] {
        for &(offset, limit) in &[(0usize, 40usize), (170, 80), (500, 50)] {
            let got: Vec<String> = index
                .search(&SearchRequest {
                    query: parse_at("", NOW),
                    sort: SortKey::Name,
                    descending: desc,
                    page: Page {
                        offset: offset as u32,
                        limit: limit as u32,
                        count_cap: 10_000,
                    },
                })
                .expect("search")
                .hits
                .into_iter()
                .map(|h| h.path)
                .collect();
            let whole = brute_force(
                &entries,
                &parse_at("", NOW),
                SortKey::Name,
                desc,
                offset + limit,
            );
            let want: Vec<String> = whole[offset..].iter().map(|h| h.path.clone()).collect();
            assert_eq!(
                got, want,
                "name ties changed at {offset}+{limit} (desc={desc})"
            );
        }
    }
}

#[test]
fn descending_stored_extension_order_keeps_the_public_tie_order() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    let variants = ["RS", "Rs", "rs"];
    let entries: Vec<Entry> = (0..600u64)
        .map(|i| {
            entry(
                &format!(
                    "/d{:04}/file{:04}.{}",
                    599 - i,
                    i,
                    variants[i as usize % variants.len()]
                ),
                NOW - (i % 19) as i64,
                20_000 + i,
            )
        })
        .collect();
    for part in entries.chunks(100) {
        index
            .apply(&mut part.iter().cloned().map(Change::Upsert))
            .expect("apply");
        index.commit().expect("commit");
    }

    for desc in [false, true] {
        for &(offset, limit) in &[(0usize, 40usize), (170, 80), (500, 50)] {
            let got: Vec<String> = index
                .search(&SearchRequest {
                    query: parse_at("", NOW),
                    sort: SortKey::Ext,
                    descending: desc,
                    page: Page {
                        offset: offset as u32,
                        limit: limit as u32,
                        count_cap: 10_000,
                    },
                })
                .expect("search")
                .hits
                .into_iter()
                .map(|h| h.path)
                .collect();
            let whole = brute_force(
                &entries,
                &parse_at("", NOW),
                SortKey::Ext,
                desc,
                offset + limit,
            );
            let want: Vec<String> = whole[offset..].iter().map(|h| h.path.clone()).collect();
            assert_eq!(
                got, want,
                "extension ties changed at {offset}+{limit} (desc={desc})"
            );
        }
    }
}

#[test]
fn folded_extension_rules_survive_a_multi_segment_merge() {
    use scour_core::text::{DefaultFolder, Folder};

    // Both are twelve bytes before folding and eighteen after, agreeing on
    // their first sixteen folded bytes, so a merge treating the `Head` as
    // exact — or resolving it with the whole name — gets the boundary wrong.
    let common = "Ⱥ".repeat(5);
    let grown_a = format!("{common}Ⱥ");
    let grown_b = format!("{common}Ⱦ");
    assert_eq!(grown_a.len(), 12);
    assert_eq!(grown_b.len(), 12);
    let folded_a = DefaultFolder.fold(&grown_a);
    let folded_b = DefaultFolder.fold(&grown_b);
    assert_eq!(&folded_a.as_bytes()[..16], &folded_b.as_bytes()[..16]);
    assert_ne!(folded_a, folded_b);

    // Seven dotless i characters contract from fourteen raw bytes to seven and
    // stay ineligible; an ASCII suffix folding the same way is eligible.
    let contracted = "ı".repeat(7);
    let eligible = "iiiiiii";
    let mut entries = Vec::new();
    for i in 0..250u64 {
        let ext = match i % 5 {
            0 | 3 => grown_a.as_str(),
            1 => grown_b.as_str(),
            2 => contracted.as_str(),
            _ => eligible,
        };
        entries.push(entry(
            &format!("/fold/d{:03}/file{i:03}.{ext}", 249 - i),
            NOW - ((i * 37) % 29) as i64,
            50_000 + i,
        ));
    }

    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    for part in entries.chunks(17) {
        index
            .apply(&mut part.iter().cloned().map(Change::Upsert))
            .expect("apply");
        index.commit().expect("commit");
    }

    for desc in [false, true] {
        for &(offset, limit) in &[(0usize, 25usize), (35, 30), (80, 35), (130, 40), (210, 25)] {
            let got: Vec<String> = index
                .search(&SearchRequest {
                    query: parse_at("", NOW),
                    sort: SortKey::Ext,
                    descending: desc,
                    page: Page {
                        offset: offset as u32,
                        limit: limit as u32,
                        count_cap: 10_000,
                    },
                })
                .expect("search")
                .hits
                .into_iter()
                .map(|hit| hit.path)
                .collect();
            let whole = brute_force(
                &entries,
                &parse_at("", NOW),
                SortKey::Ext,
                desc,
                offset + limit,
            );
            let want: Vec<String> = whole[offset..].iter().map(|hit| hit.path.clone()).collect();
            assert_eq!(
                got, want,
                "folded extension merge changed at {offset}+{limit} (desc={desc})"
            );
        }
    }

    let query = parse_at("ext:iiiiiii", NOW);
    let got: Vec<String> = index
        .search(&SearchRequest {
            query: query.clone(),
            sort: SortKey::Ext,
            descending: false,
            page: Page::new(0, 1_000),
        })
        .expect("extension filter")
        .hits
        .into_iter()
        .map(|hit| hit.path)
        .collect();
    let want: Vec<String> = brute_force(&entries, &query, SortKey::Ext, false, 1_000)
        .into_iter()
        .map(|hit| hit.path)
        .collect();
    assert_eq!(got, want);
    assert!(
        got.iter().all(|path| path.ends_with(".iiiiiii")),
        "a contracted over-limit extension passed the raw-length rule"
    );
}

/// Oldest-first is the same answer as before, now that it is a different walk.
/// A backwards walk yields dates in order and paths *reversed* inside a date,
/// so a page whose edge falls in a group of files sharing one second would be
/// handed the last paths to choose from rather than the first.
#[test]
fn oldest_first_is_the_answer_brute_force_gives() {
    let f = Fixture::new(16_000, 2_000);
    for q in [
        "",
        "rapor",
        "ext:rs",
        "kind:code",
        "size:>1mb",
        "under:/home/u/Projeler",
        "dm:30d",
        "zzzznothing",
    ] {
        f.check(q, SortKey::Modified, false);
    }

    // And the ties. One second, more files than a page, across four segments —
    // so the group is split by segment as well as by page.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    let mut all: Vec<Entry> = Vec::new();
    for i in 0..400u64 {
        // Names that sort the *other* way from the order they are written in,
        // so a walk that keeps whichever it met first cannot pass by luck.
        all.push(entry(
            &format!("/t/{:04}.txt", 399 - i),
            NOW - 5_000,
            1_000 + i,
        ));
    }
    // A little on either side of the tie, so the page edge lands inside it.
    all.push(entry("/t/before.txt", NOW - 9_000, 1));
    all.push(entry("/t/after.txt", NOW - 1_000, 2));
    for part in all.chunks(100) {
        let mut it = part.iter().cloned().map(Change::Upsert);
        index.apply(&mut it).expect("apply");
        index.commit().expect("commit");
    }
    let tied = Fixture {
        _tmp: tmp,
        index,
        entries: all,
    };
    for limit in [1, 5, 40, 200, 402] {
        assert_eq!(
            tied.search("", SortKey::Modified, false, limit),
            tied.expected("", SortKey::Modified, false, limit),
            "oldest-first, first {limit}, disagrees where the dates tie"
        );
    }
    // And every page of it, which is where the edge of a page meets the edge
    // of the tie group.
    for offset in [0, 1, 39, 40, 199, 200, 399] {
        assert_eq!(
            tied.paged("", SortKey::Modified, false, offset, 40),
            tied.expected("", SortKey::Modified, false, offset + 40)[offset..].to_vec(),
            "oldest-first page at {offset} disagrees where the dates tie"
        );
    }

    /* And the walk's own answer, not only the merge's: `run_with` has two ways
     * out, and a single-segment caller takes `hits` already ordered. Its rows
     * arrive in date order with paths reversed inside a date.
     */
    let one = Fixture::new(600, 600);
    // Folded into one, because this half is about the path a single segment
    // takes on its own rather than about the merge.
    one.index.maintain(Maintenance::Rebuild).expect("rebuild");
    assert_eq!(one.index.stats().expect("stats").segments, 1);
    let mut direct: Vec<String> = Vec::new();
    one.index
        .for_each_segment(&mut |_, seg| {
            let plan = scour_index_native::Plan::compile(&parse_at("", NOW), seg).expect("plan");
            let found = scour_index_native::run_with(
                seg,
                &plan,
                scour_index_native::Wanted {
                    sort: SortKey::Modified,
                    descending: false,
                    offset: 0,
                    limit: 60,
                    count_cap: 10_000_000,
                    rank_only: false,
                },
                None,
                &[],
            );
            direct = found.hits.into_iter().map(|h| h.path).collect();
        })
        .expect("segments");
    assert_eq!(
        direct,
        one.expected("", SortKey::Modified, false, 60),
        "the walk's own oldest-first page disagrees with brute force"
    );
}

/// A page that ends inside a tie on sixteen bytes is still the page: the name
/// key abbreviates, so two rows tying on it are not equal. Two hundred names
/// agree here, written in reverse of their sort order and split across segments
/// so the merge comparator is exercised as well as `narrow`.
#[test]
fn a_page_that_ends_inside_a_tie_on_sixteen_bytes_is_still_the_page() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    let mut all: Vec<Entry> = Vec::new();
    for i in 0..20u64 {
        all.push(entry(&format!("/t/aaa_{i:03}.rs"), NOW - 5_000, 10_000 + i));
        all.push(entry(&format!("/t/zzz_{i:03}.rs"), NOW - 5_000, 20_000 + i));
    }
    // `sozlesme_arsivi_` is sixteen bytes exactly, so the key sees that and
    // nothing else; dates repeat every third file, exercising the tie-break.
    for i in 0..200u64 {
        let n = 199 - i;
        all.push(entry(
            &format!("/t/sozlesme_arsivi_{n:03}.rs"),
            NOW - 5_000 - (n as i64 % 3),
            30_000 + i,
        ));
    }
    // The same names spelled differently, in another directory: the key is over
    // the *folded* name, so these join the group.
    for i in 0..50u64 {
        let n = 49 - i;
        all.push(entry(
            &format!("/u/SOZLESME_ARSIVI_{n:03}.rs"),
            NOW - 5_000 - (n as i64 % 3),
            40_000 + i,
        ));
    }
    for part in all.chunks(40) {
        let mut it = part.iter().cloned().map(Change::Upsert);
        index.apply(&mut it).expect("apply");
        index.commit().expect("commit");
    }
    let f = Fixture {
        _tmp: tmp,
        index,
        entries: all,
    };
    assert!(
        f.index.stats().expect("stats").segments >= 7,
        "the fixture is supposed to be fragmented"
    );

    for sort in [SortKey::Name, SortKey::Path] {
        for desc in [true, false] {
            // Every edge of the group: just before it, its first row, inside
            // it, its last row, and past it.
            for limit in [1, 19, 20, 21, 25, 120, 269, 270, 290] {
                assert_eq!(
                    f.search("", sort, desc, limit),
                    f.expected("", sort, desc, limit),
                    "first {limit} by {sort:?} (desc={desc}) disagrees where sixteen bytes tie"
                );
            }
            // And the pages of it, where the edge of a page meets the edge of
            // the group from the other side.
            for offset in [0, 1, 19, 20, 39, 120, 250, 289] {
                assert_eq!(
                    f.paged("", sort, desc, offset, 20),
                    f.expected("", sort, desc, offset + 20)[offset..].to_vec(),
                    "page at {offset} by {sort:?} (desc={desc}) disagrees where sixteen bytes tie"
                );
            }
        }
    }
}

/// A numeric order stops opening blocks, and stops at the right place. Three
/// halves to it: the tie group at the edge, where a block that can only *equal*
/// the worst row held may still displace it; the count, which a cap may bound
/// while the page may not be; and the offset, out to twenty thousand.
#[test]
fn a_numeric_order_stops_where_the_page_really_ends() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    let mut all: Vec<Entry> = Vec::new();
    for i in 0..4_000u64 {
        let n = i as i64;
        all.push(Entry {
            id: EntryId::inode(SourceId(0), 66_310, 1_000 + i),
            path: format!("/t/{:05}.bin", 3_999 - i),
            is_dir: false,
            meta: Meta {
                // Four sizes, seven creation dates, three block counts and one
                // kind — every one a tie group wider than a page.
                size: (n % 4) * 4_096,
                ctime: NOW - (n % 7) * 1_000,
                disk: (n % 3) * 4_096,
                mtime: NOW - (n % 11) * 1_000,
                ..Meta::UNKNOWN
            },
        });
    }
    for part in all.chunks(400) {
        let mut it = part.iter().cloned().map(Change::Upsert);
        index.apply(&mut it).expect("apply");
        index.commit().expect("commit");
    }
    let f = Fixture {
        _tmp: tmp,
        index,
        entries: all,
    };
    assert!(f.index.stats().expect("stats").segments >= 8);

    for sort in [
        SortKey::Size,
        SortKey::Created,
        SortKey::Disk,
        SortKey::Kind,
        SortKey::Modified,
    ] {
        for desc in [true, false] {
            for &(offset, limit) in &[(0, 40), (39, 3), (200, 40), (1_999, 40), (3_960, 40)] {
                assert_eq!(
                    f.paged("", sort, desc, offset, limit),
                    f.expected("", sort, desc, offset + limit)[offset..].to_vec(),
                    "{sort:?} (desc={desc}) at {offset}+{limit} disagrees with brute force"
                );
            }
        }
    }

    // The cap bounds the total and never the page. `ext:bin` rather than the
    // empty query: an empty one takes its total from the segment's live count
    // and never hands the cap to the walk at all.
    let want = f.expected("ext:bin", SortKey::Size, true, 40);
    for cap in [1u32, 40, 100, 10_000_000] {
        let res = f
            .index
            .search(&SearchRequest {
                query: parse_at("ext:bin", NOW),
                sort: SortKey::Size,
                descending: true,
                page: Page {
                    offset: 0,
                    limit: 40,
                    count_cap: cap,
                },
            })
            .expect("search");
        assert_eq!(
            res.hits.iter().map(|h| h.path.clone()).collect::<Vec<_>>(),
            want,
            "a count cap of {cap} changed which forty won"
        );
        assert_eq!(res.total, u64::from(cap).min(4_000), "cap {cap}");
        assert_eq!(res.capped, cap <= 4_000, "cap {cap}");
    }
}

/// A folder still sorts by what is under it once blocks are being skipped: a
/// directory sorts by its rollup, not its `Size` column, so that column's range
/// is not a bound on what the block can reach. The largest folder here sits in
/// a block of tiny files.
#[test]
fn the_largest_folder_survives_a_walk_that_skips_blocks() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    let mut all: Vec<Entry> = vec![Entry {
        id: EntryId::inode(SourceId(0), 66_310, 1),
        path: "/big".into(),
        is_dir: true,
        meta: Meta {
            mtime: NOW,
            size: 4_096,
            disk: 4_096,
            ..Meta::UNKNOWN
        },
    }];
    // Newest, so they share the first block with the folder's own row and
    // leave its stored range saying nothing but "four kilobytes".
    for i in 0..40u64 {
        all.push(Entry {
            id: EntryId::inode(SourceId(0), 66_310, 100 + i),
            path: format!("/small/{i:04}.txt"),
            is_dir: false,
            meta: Meta {
                mtime: NOW,
                size: 10,
                disk: 4_096,
                ..Meta::UNKNOWN
            },
        });
    }
    // And a few thousand rows of real size underneath it, older, so they fill
    // every block after the first.
    for i in 0..5_000u64 {
        all.push(Entry {
            id: EntryId::inode(SourceId(0), 66_310, 10_000 + i),
            path: format!("/big/{i:05}.bin"),
            is_dir: false,
            meta: Meta {
                mtime: NOW - 1_000,
                size: 1_000_000 + i as i64,
                disk: 1_000_000 + i as i64,
                ..Meta::UNKNOWN
            },
        });
    }
    index
        .apply(&mut all.into_iter().map(Change::Upsert))
        .expect("apply");
    index.commit().expect("commit");
    index.subtree_sizes(&[]).expect("warm the folder sizes");

    let page: Vec<String> = index
        .search(&SearchRequest {
            query: parse_at("", NOW),
            sort: SortKey::Size,
            descending: true,
            page: Page::new(0, 40),
        })
        .expect("search")
        .hits
        .into_iter()
        .map(|h| h.path)
        .collect();
    assert_eq!(
        page.first().map(String::as_str),
        Some("/big"),
        "the folder holds five gigabytes and has to lead the page: {:?}",
        &page[..5.min(page.len())]
    );

    // Ascending is the mirror and gets the mirror's bound: the folder is now
    // the *last* thing that could reach the page, and the tiny files lead it.
    let up: Vec<String> = index
        .search(&SearchRequest {
            query: parse_at("", NOW),
            sort: SortKey::Size,
            descending: false,
            page: Page::new(0, 40),
        })
        .expect("search")
        .hits
        .into_iter()
        .map(|h| h.path)
        .collect();
    assert!(
        !up.contains(&"/big".to_string()),
        "five gigabytes is not among the forty smallest: {:?}",
        &up[..5.min(up.len())]
    );
    assert!(up.iter().all(|p| p.starts_with("/small/")));
}

#[test]
fn paging_across_segments_reconstructs_the_list() {
    // The offset belongs to the merged list, not to any one segment: applying
    // it per segment drops a row from each and returns a short page.
    let f = Fixture::new(6_000, 500);
    let mut got = f.paged("ext:rs", SortKey::Name, false, 0, 20);
    got.extend(f.paged("ext:rs", SortKey::Name, false, 20, 20));
    got.extend(f.paged("ext:rs", SortKey::Name, false, 40, 20));
    assert_eq!(got, f.expected("ext:rs", SortKey::Name, false, 60));
}

/// A page deep in the list is the same rows the list has there, in every order,
/// both directions, at offsets inside a segment and across a boundary. The
/// reference is the index's own full answer: paging is self-consistency, and
/// where a sort ties — `ext:rs` by relevance — two orders are both right.
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
    // The empty query is what a window opens on and pages furthest; the other
    // two page a filtered list, where the offset counts matches, not rows.
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

/// Asking for one row at a time gives the same list as asking for all of them:
/// an offset applied per segment loses a row per segment rather than shifting
/// the window, so every page differs subtly and each looks reasonable.
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
    // A save writes a temporary file and renames it over the target, so the
    // path survives and the object identity does not. Each round hands over a
    // different identity for one path; the index must answer with one row.
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
    // The staged rows are lifted out of the buffer before anything is written
    // and are the only copy, so a failed write used to drop them while
    // reporting success. An unwritable directory is the deterministic ENOSPC.
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

/// A failed bitmap replacement must leave enough state for the next commit: the
/// first attempt already killed the row in memory, so the retry reports zero and
/// without a separate dirty-bitmap stamp writes nothing.
#[test]
fn a_failed_alive_write_retries_the_pending_removal() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let victim = "/home/u/retry-removal.txt";
    {
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        let mut rows = (0..200u64).map(|i| {
            let path = if i == 77 {
                victim.to_owned()
            } else {
                format!("/home/u/keep-{i:03}.txt")
            };
            Change::Upsert(entry(&path, NOW + i as i64, i))
        });
        index.apply(&mut rows).expect("apply");
        index.commit().expect("commit");

        index
            .apply(&mut std::iter::once(Change::RemoveSubtree {
                path: victim.into(),
            }))
            .expect("remove");

        let alive = std::fs::read_dir(tmp.path())
            .expect("read_dir")
            .flatten()
            .map(|entry| entry.path())
            .find(|path| path.extension().is_some_and(|ext| ext == "alive"))
            .expect("alive file");
        let saved = tmp.path().join("saved-alive");
        std::fs::rename(&alive, &saved).expect("move alive aside");
        std::fs::create_dir(&alive).expect("block alive replacement");

        assert!(
            index.commit().is_err(),
            "a directory accepted a bitmap rename"
        );
        assert_eq!(index.stats().expect("stats").pending_removals, 1);
        assert!(
            !index
                .search(&SearchRequest {
                    query: parse_at("retry-removal", NOW),
                    page: Page::new(0, 10),
                    ..Default::default()
                })
                .expect("search after failed commit")
                .hits
                .iter()
                .any(|hit| hit.path == victim),
            "the failed durable write undid the in-memory removal"
        );

        std::fs::remove_dir(&alive).expect("remove blocker");
        std::fs::rename(&saved, &alive).expect("restore old bitmap");
        index.commit().expect("retry");
        assert_eq!(index.stats().expect("stats").pending_removals, 0);
    }

    let index = NativeIndex::open_or_create(tmp.path()).expect("reopen");
    let found = index
        .search(&SearchRequest {
            query: parse_at("retry-removal", NOW),
            page: Page::new(0, 10),
            ..Default::default()
        })
        .expect("search after reopen");
    assert!(found.hits.is_empty(), "the restart resurrected {victim}");
    assert_eq!(index.stats().expect("stats").entries, 199);
}

/// `forget` bypasses the ordinary removal overlay, but its bitmap has the same
/// durability rule: a failed replacement must remain work for the next call.
#[test]
fn forget_retries_a_failed_alive_write_and_survives_reopen() {
    fn all_paths(index: &NativeIndex) -> Vec<String> {
        index
            .search(&SearchRequest {
                query: parse_at("", NOW),
                page: Page::new(0, 10),
                ..Default::default()
            })
            .expect("search")
            .hits
            .into_iter()
            .map(|hit| hit.path)
            .collect()
    }

    let tmp = tempfile::tempdir().expect("tmpdir");
    {
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        let kept = Entry {
            id: EntryId::path_hash(SourceId(1), "/w/kept.txt"),
            path: "/w/kept.txt".into(),
            is_dir: false,
            meta: Meta {
                mtime: NOW + 1,
                size: 2,
                ..Meta::UNKNOWN
            },
        };
        index
            .apply(
                &mut [
                    Change::Upsert(entry("/w/forgotten.txt", NOW, 1)),
                    Change::Upsert(kept),
                ]
                .into_iter(),
            )
            .expect("apply");
        index.commit().expect("commit");

        let alive = tmp.path().join("seg-00000001.alive");
        let saved = tmp.path().join("saved-forget-alive");
        std::fs::rename(&alive, &saved).expect("move bitmap aside");
        std::fs::create_dir(&alive).expect("block bitmap replacement");

        assert!(index.forget(SourceId(0)).is_err());
        assert_eq!(all_paths(&index), ["/w/kept.txt"]);

        std::fs::remove_dir(&alive).expect("remove blocker");
        std::fs::rename(&saved, &alive).expect("restore old bitmap");
        assert_eq!(index.forget(SourceId(0)).expect("retry forget"), 0);
    }

    let reopened = NativeIndex::open_or_create(tmp.path()).expect("reopen");
    assert_eq!(all_paths(&reopened), ["/w/kept.txt"]);
}

/// A failed sweep must restore its unchanged-row stamps as well as its dirty
/// bitmap, or retrying deletes the row the walk explicitly saw.
#[test]
fn sweep_restores_seen_marks_and_retries_a_failed_alive_write() {
    fn all_paths(index: &NativeIndex) -> Vec<String> {
        index
            .search(&SearchRequest {
                query: parse_at("", NOW),
                page: Page::new(0, 10),
                ..Default::default()
            })
            .expect("search")
            .hits
            .into_iter()
            .map(|hit| hit.path)
            .collect()
    }

    let tmp = tempfile::tempdir().expect("tmpdir");
    {
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        let keeper = entry("/w/keeper.txt", NOW, 1);
        index
            .apply(
                &mut [
                    Change::Upsert(keeper.clone()),
                    Change::Upsert(entry("/w/missing.txt", NOW + 1, 2)),
                ]
                .into_iter(),
            )
            .expect("apply");
        index.commit().expect("commit");

        let generation = index.begin_generation().expect("generation");
        index
            .apply(&mut std::iter::once(Change::Upsert(keeper)))
            .expect("stamp keeper");
        index.commit().expect("commit stamp");

        let alive = tmp.path().join("seg-00000001.alive");
        let saved = tmp.path().join("saved-sweep-alive");
        std::fs::rename(&alive, &saved).expect("move bitmap aside");
        std::fs::create_dir(&alive).expect("block bitmap replacement");
        assert!(
            index
                .sweep(
                    SourceId(0),
                    &["/w".to_string()],
                    generation,
                    &PrefixSet::default()
                )
                .is_err()
        );

        std::fs::remove_dir(&alive).expect("remove blocker");
        std::fs::rename(&saved, &alive).expect("restore old bitmap");
        index
            .sweep(
                SourceId(0),
                &["/w".to_string()],
                generation,
                &PrefixSet::default(),
            )
            .expect("retry sweep");
    }

    let reopened = NativeIndex::open_or_create(tmp.path()).expect("reopen");
    assert_eq!(all_paths(&reopened), ["/w/keeper.txt"]);
}

#[test]
fn one_source_cannot_sweep_away_another_source_rows() {
    // Two sources whose roots overlap. A scan of one says "I walked here and
    // did not find these rows" — a statement about its own rows only.
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
        .sweep(
            SourceId(0),
            &["/ortak".to_string()],
            g,
            &PrefixSet::default(),
        )
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
    // The one thing that may not wait for a commit: a removal takes effect in
    // the overlay first and in the files afterwards.
    let f = Fixture::new(20_000, 20_000);
    let victim = f
        .entries
        .iter()
        .find(|entry| !entry.is_dir)
        .expect("a file")
        .path
        .clone();

    f.index
        .apply(&mut std::iter::once(Change::RemoveSubtree {
            path: victim.clone(),
        }))
        .expect("apply");
    let remaining: Vec<Entry> = f
        .entries
        .iter()
        .filter(|entry| entry.path != victim)
        .cloned()
        .collect();
    for sort in [SortKey::Name, SortKey::Ext] {
        let pending = f
            .index
            .search(&SearchRequest {
                query: parse_at("", NOW),
                sort,
                descending: false,
                page: Page {
                    offset: 0,
                    limit: 50,
                    count_cap: 200,
                },
            })
            .expect("search while removal is pending");
        assert!(
            !pending.hits.iter().any(|hit| hit.path == victim),
            "{victim} is still visible"
        );
        assert!(
            pending.rows_visited < f.entries.len() as u64 / 4,
            "one pending removal disabled the stored {sort:?} order: {} rows of {}",
            pending.rows_visited,
            f.entries.len()
        );
        let want: Vec<String> = brute_force(&remaining, &parse_at("", NOW), sort, false, 50)
            .into_iter()
            .map(|hit| hit.path)
            .collect();
        assert_eq!(
            pending
                .hits
                .iter()
                .map(|hit| hit.path.clone())
                .collect::<Vec<_>>(),
            want,
            "the stored {sort:?} order and pending-removal veto disagree with brute force"
        );
    }
    assert_eq!(f.index.stats().expect("stats").pending_removals, 1);

    f.index.commit().expect("commit");
    let after = f.search("", SortKey::Name, false, 50);
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
    // The case a rescan cannot report: a file deleted while nothing watched.
    // The fourth is identifiable only by carrying an older stamp.
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
        .sweep(SourceId(0), &["/w".to_string()], g, &PrefixSet::default())
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
    // The sweep must give the same two answers as rebuilding a path per row
    // did: the swept directory's own row goes, though it lives in its parent
    // and carries the parent's number; a sibling merely sharing a prefix stays.
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
        .sweep(
            SourceId(0),
            &["/w/proj".to_string()],
            g,
            &PrefixSet::default(),
        )
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
    // A few thousand removed paths land in one batch. Asking every row about
    // every path was 2.24 s of held write lock at four thousand paths. What
    // replaces it must not change the answer: a subtree reported as one prefix
    // and as every path inside it are two descriptions of one delete.
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
    // A segment swept empty is both *touched* — its bitmap changed — and gone,
    // and a commit writes bitmaps after releasing the lock, so the `.alive`
    // file came back for a segment nothing would open: 182 orphans against 55.
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    let first: Vec<Entry> = (0..200)
        .map(|i| entry(&format!("/w/old{i}.rs"), 1_000 + i, i as u64))
        .collect();
    index
        .apply(&mut first.into_iter().map(Change::Upsert))
        .expect("apply");
    index.commit().expect("commit");

    // Remove every row, so the commit both touches that segment and empties it.
    // `commit` erases files under the lock and writes bitmaps after releasing.
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

    // Checked against the files, not the manifest: `Live::open` reads `.names`
    // first, so any other part without it is a number nothing can open.
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
    // A segment is nine files written one at a time, so a kill in the middle
    // leaves a partial set. Safe to remove: the manifest is written before
    // anything is unlinked, so a file it does not name is unreachable.
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
    // A fold releases the lock while it builds, so a commit lands during it and
    // appends a segment to the generation just folded — which had the rebuild
    // loop folding the whole index again, over ten minutes at 2.1 M entries.
    // The work is decided once now, so this finishes under a hard writer.
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

/// Folding a segment a walk has marked must not lose the files it marked: marks
/// are keyed on segment numbers and a fold writes a new number, so the sweep
/// deletes files that are on disk. Removing the guard fails only this test.
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

    // Housekeeping arriving in the middle: the engine compacts on the commit
    // boundary and a real walk holds a generation open for seconds.
    f.index
        .maintain(Maintenance::Compact)
        .expect("compaction should decline, not fail");

    // And now the sweep, judging rows by marks the fold may have invalidated.
    f.index
        .sweep(SourceId(0), &["".to_string()], g, &PrefixSet::default())
        .expect("sweep");

    assert_eq!(
        f.index.stats().expect("stats").entries,
        held,
        "the sweep deleted rows the walk had seen — the marks were folded away"
    );
}

/// Two paths that hash to the same key are still two files: 32 bits over 2.2 M
/// entries expect about 576 colliding pairs, harmless only because
/// `Segment::is_at` confirms a candidate against the row's directory, name and
/// source. A fixture never collides by accident, so this searches for one.
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

    // Saving over one of them must not take the other: the identity table
    // answers with both, and only the name and directory comparison says which.
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

/// Two colliding paths that share a name are told apart by their directory —
/// the case a differing name would otherwise settle, which left the directory
/// comparison untested. Same basename, different folder, same 32-bit key.
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

/// The same path under two sources stays two rows when one is saved over. The
/// source comparison in `Segment::is_at` cannot change an answer (see the note
/// there); the behaviour is pinned whatever enforces it.
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

/// A pass that is never swept still lets housekeeping run afterwards. A walk
/// that could not look must not sweep, but the sweep was the only thing ending a
/// generation, so the pass stayed open and a noted segment cannot be folded.
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

/// Compaction still folds when every segment has a generation of its own — what
/// a watcher produces by putting a walk between almost every pair of commits.
/// Grouping by generation then never finds three: 241 segments across 205
/// generations, largest group 2. One-way; nothing brings them back together.
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
            .sweep(
                SourceId(0),
                &["/baska-yer".to_string()],
                g,
                &PrefixSet::default(),
            )
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

/// A compaction that is not allowed to fold still ends. The refusal returned
/// `Ok(())`, indistinguishable from having done the work, so `maintain(Compact)`
/// handed back the same group for ever — 99.7% of a core, 224 segments. Before
/// the fix this hangs rather than fails, so it runs on a thread with a deadline.
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
    // A search pays for the number of segments, so a compaction only has to get
    // that down; rewriting the body would cost a pass over the index.
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
    // The merged segment carries one stamp, so folding across a boundary would
    // give old rows a new one and the next sweep would walk past them.
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
            .sweep(SourceId(0), &["/w".to_string()], g, &PrefixSet::default())
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

    // The generation the sweep emptied folds to nothing rather than to an empty
    // segment: no rows means no dead rows, so it would never qualify again.
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
    // A rebuild folds within a generation and each source's scan takes its own,
    // so a second source meant two segments no rebuild could merge: 2,951,074
    // entries, 1,441,890 unsorted, and `rapor` at 125 ms.
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
            .sweep(SourceId(0), &[root.to_string()], g, &PrefixSet::default())
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
            .sweep(
                SourceId(0),
                &["/mnt/depo".to_string()],
                g,
                &PrefixSet::default()
            )
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
    // is in date order, so each stops at its own page and the merge picks.
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

    // And an order the row layout says nothing about: blocks are opened by the
    // largest size each holds, so once forty rows beat everything the next
    // block could contain there is nothing left to open. A cap may bound the
    // total, never the result.
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
    assert!(res.fast_path, "the zone map should have ended this walk");
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

    // The same pending-removal overlay as search, compiled into directory
    // ranges rather than a spelled path built for every matching row.
    let kinds = |index: &NativeIndex| {
        index
            .facets(&FacetRequest {
                query: parse_at("", NOW),
                by: vec![FacetBy::Kind],
            })
            .expect("kind facets")
            .facets
            .into_iter()
            .map(|facet| facet.count)
            .sum::<u64>()
    };
    let before = kinds(&f.index);
    let victim = f
        .entries
        .iter()
        .find(|entry| !entry.is_dir)
        .expect("a file")
        .path
        .clone();
    f.index
        .apply(&mut std::iter::once(Change::RemoveSubtree { path: victim }))
        .expect("remove before facets");
    assert_eq!(
        kinds(&f.index),
        before - 1,
        "a pending removal stayed in the sidebar"
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
    // A rescan stamps a new generation, the sweep kills every row of the old
    // one, and the emptied segment stays in the list — 1,204,270 rows read end
    // to end to produce nothing, until a rebuild happens to remove it.
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
        .sweep(SourceId(0), &["/w".to_string()], g, &PrefixSet::default())
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
    // The same one level down: a block with no live row is skipped on sixteen
    // bytes of the bitmap, whether or not its segment still holds anything.
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
    // Nothing is lost: an index is derived from the filesystem in its entirety,
    // and the service acts on this by discarding and rescanning. `IndexCorrupt`
    // would be a lie and would look like one.
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

/// An index written before the path order existed is read, not thrown away.
/// Simulated by deleting what an older build never wrote. Both halves: every
/// answer matches brute force, and the walk visits every row again — which is
/// what says the fallback is really being taken.
#[test]
fn an_index_written_without_a_path_order_answers_the_same_way() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let fs = generate(&MockOptions {
        files: 6_000,
        now: NOW,
        ..Default::default()
    });
    let page = |index: &NativeIndex, desc: bool| -> (Vec<String>, u64) {
        let res = index
            .search(&SearchRequest {
                query: parse_at("", NOW),
                sort: SortKey::Path,
                descending: desc,
                page: Page {
                    offset: 40,
                    limit: 60,
                    count_cap: 10_000_000,
                },
            })
            .expect("search");
        (
            res.hits.into_iter().map(|h| h.path).collect(),
            res.rows_visited,
        )
    };

    // The same page in both directions, with the order present: what it holds
    // and what it cost to find out.
    let with_order = {
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        for part in fs.entries.chunks(2_000) {
            index
                .apply(&mut part.iter().cloned().map(Change::Upsert))
                .expect("apply");
            index.commit().expect("commit");
        }
        [page(&index, false), page(&index, true)]
    };

    let mut removed = 0;
    for entry in std::fs::read_dir(tmp.path()).expect("read_dir").flatten() {
        if entry.file_name().to_string_lossy().ends_with(".porder") {
            std::fs::remove_file(entry.path()).expect("remove");
            removed += 1;
        }
    }
    assert!(removed > 0, "the fixture wrote no path order to remove");

    let index = NativeIndex::open_or_create(tmp.path()).expect("reopen without the path order");
    for (i, desc) in [false, true].into_iter().enumerate() {
        let (want, visited) = &with_order[i];
        let (got, walked) = page(&index, desc);
        assert_eq!(&got, want, "path (desc={desc}) changed without the order");
        let reference: Vec<String> =
            brute_force(&fs.entries, &parse_at("", NOW), SortKey::Path, desc, 100)[40..]
                .iter()
                .map(|h| h.path.clone())
                .collect();
        assert_eq!(
            got, reference,
            "path (desc={desc}) disagrees with brute force"
        );
        assert!(
            walked > *visited,
            "without the order the walk has nothing to stop it: {walked} against {visited}"
        );
    }
}

/// Name-order files are an optional acceleration, independently per segment, so
/// an upgrade has three states: all current, mixed, and entirely legacy. All
/// must answer identically; only the number of rows visited may change.
#[test]
fn segments_with_and_without_a_name_order_answer_one_name_list() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let fs = generate(&MockOptions {
        files: 12_000,
        now: NOW,
        ..Default::default()
    });
    let page = |index: &NativeIndex, desc: bool, offset: usize, limit: usize| {
        let res = index
            .search(&SearchRequest {
                query: parse_at("", NOW),
                sort: SortKey::Name,
                descending: desc,
                page: Page {
                    offset: offset as u32,
                    limit: limit as u32,
                    count_cap: 10_000_000,
                },
            })
            .expect("search");
        (
            res.hits.into_iter().map(|h| h.path).collect::<Vec<_>>(),
            res.rows_visited,
        )
    };

    {
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        for part in fs.entries.chunks(1_500) {
            index
                .apply(&mut part.iter().cloned().map(Change::Upsert))
                .expect("apply");
            index.commit().expect("commit");
        }
    }

    let mut orders: Vec<std::path::PathBuf> = std::fs::read_dir(tmp.path())
        .expect("read_dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "norder"))
        .collect();
    orders.sort();
    assert!(orders.len() >= 4, "the fixture wrote too few name orders");

    let with_order = {
        let index = NativeIndex::open_or_create(tmp.path()).expect("open current index");
        [page(&index, false, 300, 80), page(&index, true, 300, 80)]
    };

    // Every other segment is legacy: both paths must contribute to one merge.
    for p in orders.iter().step_by(2) {
        std::fs::remove_file(p).expect("remove alternating name order");
    }
    {
        let index = NativeIndex::open_or_create(tmp.path()).expect("open mixed index");
        for (i, desc) in [false, true].into_iter().enumerate() {
            let (got, _) = page(&index, desc, 300, 80);
            assert_eq!(
                got, with_order[i].0,
                "mixed name order changed (desc={desc})"
            );
        }
    }

    // Remove the rest: an index from before the feature still opens and falls
    // back to the full keyed walk.
    for p in orders.iter().skip(1).step_by(2) {
        std::fs::remove_file(p).expect("remove remaining name order");
    }
    let index = NativeIndex::open_or_create(tmp.path()).expect("open legacy index");
    for (i, desc) in [false, true].into_iter().enumerate() {
        let (got, walked) = page(&index, desc, 300, 80);
        assert_eq!(
            got, with_order[i].0,
            "legacy name order changed (desc={desc})"
        );
        let reference: Vec<String> =
            brute_force(&fs.entries, &parse_at("", NOW), SortKey::Name, desc, 380)[300..]
                .iter()
                .map(|h| h.path.clone())
                .collect();
        assert_eq!(
            got, reference,
            "legacy name order disagrees with brute force"
        );
        assert!(
            walked > with_order[i].1,
            "without name orders the walk should do more work: {walked} against {}",
            with_order[i].1
        );
    }
}

#[test]
fn segments_with_and_without_an_extension_order_answer_one_extension_list() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let fs = generate(&MockOptions {
        files: 12_000,
        now: NOW,
        ..Default::default()
    });
    let page = |index: &NativeIndex, desc: bool, offset: usize, limit: usize| {
        let res = index
            .search(&SearchRequest {
                query: parse_at("", NOW),
                sort: SortKey::Ext,
                descending: desc,
                page: Page {
                    offset: offset as u32,
                    limit: limit as u32,
                    count_cap: 10_000_000,
                },
            })
            .expect("search");
        (
            res.hits.into_iter().map(|h| h.path).collect::<Vec<_>>(),
            res.rows_visited,
        )
    };

    {
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        for part in fs.entries.chunks(1_500) {
            index
                .apply(&mut part.iter().cloned().map(Change::Upsert))
                .expect("apply");
            index.commit().expect("commit");
        }
    }

    let mut orders: Vec<std::path::PathBuf> = std::fs::read_dir(tmp.path())
        .expect("read_dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "eorder"))
        .collect();
    orders.sort();
    assert!(
        orders.len() >= 4,
        "the fixture wrote too few extension orders"
    );

    let with_order = {
        let index = NativeIndex::open_or_create(tmp.path()).expect("open current index");
        [page(&index, false, 300, 80), page(&index, true, 300, 80)]
    };

    for p in orders.iter().step_by(2) {
        std::fs::remove_file(p).expect("remove alternating extension order");
    }
    {
        let index = NativeIndex::open_or_create(tmp.path()).expect("open mixed index");
        for (i, desc) in [false, true].into_iter().enumerate() {
            let (got, _) = page(&index, desc, 300, 80);
            assert_eq!(
                got, with_order[i].0,
                "mixed extension order changed (desc={desc})"
            );
        }
    }

    for p in orders.iter().skip(1).step_by(2) {
        std::fs::remove_file(p).expect("remove remaining extension order");
    }
    let index = NativeIndex::open_or_create(tmp.path()).expect("open legacy index");
    for (i, desc) in [false, true].into_iter().enumerate() {
        let (got, walked) = page(&index, desc, 300, 80);
        assert_eq!(
            got, with_order[i].0,
            "legacy extension order changed (desc={desc})"
        );
        let reference: Vec<String> =
            brute_force(&fs.entries, &parse_at("", NOW), SortKey::Ext, desc, 380)[300..]
                .iter()
                .map(|h| h.path.clone())
                .collect();
        assert_eq!(
            got, reference,
            "legacy extension order disagrees with brute force"
        );
        assert!(
            walked > with_order[i].1,
            "without extension orders the walk should do more work: {walked} against {}",
            with_order[i].1
        );
    }
}

#[test]
fn a_name_order_that_does_not_describe_the_segment_is_refused() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    {
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        let mut it = (0..200).map(|i| Change::Upsert(entry(&format!("/a/f{i}.rs"), NOW, i)));
        index.apply(&mut it).expect("apply");
        index.commit().expect("commit");
    }

    let order = std::fs::read_dir(tmp.path())
        .expect("read_dir")
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "norder"))
        .expect("a name order was written");
    let whole = std::fs::read(&order).expect("read");
    std::fs::write(&order, &whole[..whole.len() - 1]).expect("truncate");

    match NativeIndex::open_or_create(tmp.path()) {
        Err(scour_core::Error::IndexCorrupt { detail }) => {
            assert!(detail.contains("norder"), "unhelpful detail: {detail}");
        }
        other => panic!("expected damage, got {other:?}"),
    }

    std::fs::remove_file(&order).expect("remove");
    let index = NativeIndex::open_or_create(tmp.path()).expect("open as legacy");
    assert_eq!(index.stats().expect("stats").entries, 200);
}

#[test]
fn an_extension_order_that_does_not_describe_the_segment_is_refused() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    {
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        let mut it = (0..200).map(|i| Change::Upsert(entry(&format!("/a/f{i}.rs"), NOW, i)));
        index.apply(&mut it).expect("apply");
        index.commit().expect("commit");
    }

    let order = std::fs::read_dir(tmp.path())
        .expect("read_dir")
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "eorder"))
        .expect("an extension order was written");
    let whole = std::fs::read(&order).expect("read");
    std::fs::write(&order, &whole[..whole.len() - 1]).expect("truncate");

    match NativeIndex::open_or_create(tmp.path()) {
        Err(scour_core::Error::IndexCorrupt { detail }) => {
            assert!(detail.contains("eorder"), "unhelpful detail: {detail}");
        }
        other => panic!("expected damage, got {other:?}"),
    }

    std::fs::remove_file(&order).expect("remove");
    let index = NativeIndex::open_or_create(tmp.path()).expect("open as legacy");
    assert_eq!(index.stats().expect("stats").entries, 200);
}

/// Half the segments having a path order is the ordinary state until a
/// compaction finishes: the merge is handed candidates chosen two ways and must
/// be unable to tell. True by construction — positions never leave their
/// segment — and measured against brute force anyway.
#[test]
fn segments_with_and_without_a_path_order_merge_into_one_list() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let fs = generate(&MockOptions {
        files: 12_000,
        now: NOW,
        ..Default::default()
    });
    {
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        for part in fs.entries.chunks(1_500) {
            index
                .apply(&mut part.iter().cloned().map(Change::Upsert))
                .expect("apply");
            index.commit().expect("commit");
        }
    }

    // Every other one, so both kinds are in the merge and neither is first.
    let mut orders: Vec<std::path::PathBuf> = std::fs::read_dir(tmp.path())
        .expect("read_dir")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "porder"))
        .collect();
    orders.sort();
    assert!(
        orders.len() >= 4,
        "the fixture is supposed to be fragmented"
    );
    for p in orders.iter().step_by(2) {
        std::fs::remove_file(p).expect("remove");
    }

    let index = NativeIndex::open_or_create(tmp.path()).expect("reopen");
    for desc in [false, true] {
        for &(offset, limit) in &[(0usize, 60usize), (300, 40), (2_000, 25)] {
            let got: Vec<String> = index
                .search(&SearchRequest {
                    query: parse_at("", NOW),
                    sort: SortKey::Path,
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
                .collect();
            let whole = brute_force(
                &fs.entries,
                &parse_at("", NOW),
                SortKey::Path,
                desc,
                offset + limit,
            );
            let want: Vec<String> = whole[offset..].iter().map(|h| h.path.clone()).collect();
            assert_eq!(
                got, want,
                "a mixed index disagrees with brute force by path \
                 (desc={desc}) at {offset}+{limit}"
            );
        }
    }
}

/// A path order that is there and wrong is damage, not an older index. Reading
/// it anyway gives a page that is ordered, plausible and short of whatever the
/// file stopped before. Refused as damage rather than as a version.
#[test]
fn a_path_order_that_does_not_describe_the_segment_is_refused() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    {
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        let mut it = (0..200).map(|i| Change::Upsert(entry(&format!("/a/f{i}.rs"), NOW, i)));
        index.apply(&mut it).expect("apply");
        index.commit().expect("commit");
        assert_eq!(index.stats().expect("stats").entries, 200);
    }

    let order = std::fs::read_dir(tmp.path())
        .expect("read_dir")
        .flatten()
        .map(|e| e.path())
        .find(|p| p.extension().is_some_and(|e| e == "porder"))
        .expect("a path order was written");
    let whole = std::fs::read(&order).expect("read");
    std::fs::write(&order, &whole[..whole.len() - 4]).expect("truncate");

    match NativeIndex::open_or_create(tmp.path()) {
        Err(scour_core::Error::IndexCorrupt { detail }) => {
            assert!(detail.contains("porder"), "unhelpful detail: {detail}");
        }
        other => panic!("expected damage, got {other:?}"),
    }

    // And the file being gone is the other thing entirely: the index opens.
    std::fs::remove_file(&order).expect("remove");
    let index = NativeIndex::open_or_create(tmp.path()).expect("reopen");
    assert_eq!(index.stats().expect("stats").entries, 200);
}

#[test]
fn relevance_puts_the_near_copy_first_however_many_segments_there_are() {
    // Relevance is the one order `brute_force` does not model. A segment reads
    // a directory's distance from its table; the merge recomputes it from the
    // path, a `Hit` carrying no directory number. Every name here is `main.rs`,
    // so the distance is the whole ordering.
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
    // Both names are rows, which is what makes them findable. What must not
    // follow is the disk report counting the blocks twice.
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
            query: Ast::default(),
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
    // Real names off a volume written from Windows, where a dot inside a folder
    // name is ordinary. `ext:` asks what kind of file a row is and a folder is
    // not one, while `*.rs` asks about the name — so a folder called `mod.rs`
    // matches the second and not the first.
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
    // Nothing to test means every live row matches, and each segment already
    // keeps that number; the walk was visiting all of them for it, 1.233 s on
    // 2.09 M rows. *Live*, not stored: a removed row is still in the segment.
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

    // Take four leaves away and ask again. Leaves, because a removal on a
    // directory takes everything under it.
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
/// `kill_leaves` keys on a source, and a reopened index knows none, so the path
/// has to fall through to the walk rather than be dropped between the two.
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

/// A rescan that changed nothing writes nothing either — the other half of the
/// pair below, which says it must not *remove* anything. Recognising the
/// unchanged rows and writing what is left anyway left nine segments from a
/// walk of 870,000 entries. The segment count, not the entry count.
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
        .sweep(SourceId(0), &["".to_string()], g, &PrefixSet::default())
        .expect("sweep");

    let after = f.index.stats().expect("stats");
    assert_eq!(after.segments, before, "a segment a batch is the bug");
    assert_eq!(
        after.entries,
        f.entries.len() as u64,
        "and every row is still there"
    );
    // Sparing happens when a batch is flushed; this batch fitted in the buffer,
    // so nothing flushed it until the commit, after `apply` had returned. The
    // number reaches whoever calls next — the shape of `ApplyReport::unchanged`.
    assert_eq!(report.unchanged, 0, "this batch had not been flushed yet");
    let mut one = std::iter::once(Change::Upsert(f.entries[0].clone()));
    let later = f.index.apply(&mut one).expect("apply");
    assert!(
        later.unchanged >= before as u64,
        "the commit's sparing never reached a report: {}",
        later.unchanged
    );
}

/// A scan that finds every file exactly as it left it must delete nothing: a
/// sweep deletes a row the walk did not stamp, and stamping is per segment, so
/// letting an unchanged file skip being written must keep it stamped some other
/// way. What this asserts is the behaviour, not the mechanism.
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
        .sweep(
            SourceId(0),
            &["/w".to_string()],
            g,
            &scour_core::PrefixSet::default(),
        )
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
        .sweep(
            SourceId(0),
            &["/w".to_string()],
            g,
            &scour_core::PrefixSet::default(),
        )
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
        .sweep(
            SourceId(0),
            &["/w".to_string()],
            g,
            &scour_core::PrefixSet::default(),
        )
        .expect("sweep");

    let g = index.begin_generation().expect("generation");
    let mut it = std::iter::once(Change::Upsert(row("/w/stays.txt")));
    index.apply(&mut it).expect("apply");
    index.commit().expect("commit");
    index
        .sweep(
            SourceId(0),
            &["/w".to_string()],
            g,
            &scour_core::PrefixSet::default(),
        )
        .expect("sweep");

    assert_eq!(
        index.stats().expect("stats").entries,
        1,
        "the file the walk stopped seeing has to go"
    );
}

/// A folder's size agrees with the report, and keeps agreeing: the column comes
/// from prefix sums over directory numbers and the report from `usage.rs`'s
/// rollup, two routes to one number printed side by side. Held together across
/// segments, a hard link, a removal, and a compaction that renumbers.
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
        // A directory's own `st_size` is its entry table; made large here so a
        // version counting it as content could not pass.
        meta: Meta {
            mtime: NOW,
            size: 99_000,
            disk: 99_000,
            ..Meta::UNKNOWN
        },
    };

    // Two commits, so the answer is summed across segments — and the sibling
    // that sorts between a folder and its children, `-` being 0x2D.
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
                    query: Ast::default(),
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
    // The numbers themselves, so that "they agree" cannot mean "both wrong":
    // /p holds 1,000 + 2,000 + 4,000 + 3,000 + 3,000, the last two halves of
    // the hard-linked six.
    assert_eq!(fresh[0], (13_000, 5), "/p");
    assert_eq!(fresh[1], (12_000, 4), "/p/a");
    assert_eq!(fresh[2], (8_000, 1), "/p-yedek");

    // A removal moves the alive bits without changing a byte of the segment,
    // which is what the cache stamp exists for; without it this answers 13,000.
    // The commit matters: rows stay alive until written away, so both the
    // column and the report are stale for one commit interval, together.
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

/// The report can be asked about part of a folder. A query narrows which rows
/// are weighed and nothing else: a folder with no match is still in the tree,
/// or a match's bytes roll into a grandparent that no row admits to.
#[test]
fn the_report_weighs_what_a_query_names_and_still_adds_up() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");

    let file = |path: &str, ino: u64, disk: i64| Entry {
        id: EntryId::inode(SourceId(0), 66_310, ino),
        path: path.into(),
        is_dir: false,
        meta: Meta {
            mtime: NOW,
            size: disk,
            disk,
            links: 1,
            ..Meta::UNKNOWN
        },
    };
    // Made large, so a version counting a directory's own row could not pass —
    // and one folder is named for the query below, which is how it is reached.
    let dir = |path: &str, ino: u64| Entry {
        id: EntryId::inode(SourceId(0), 66_310, ino),
        path: path.into(),
        is_dir: true,
        meta: Meta {
            mtime: NOW,
            size: 99_000,
            disk: 99_000,
            ..Meta::UNKNOWN
        },
    };

    // Two commits, so the rollup has to merge the same folder across segments.
    let first = vec![
        dir("/f", 1),
        dir("/f/log", 2),
        dir("/f/resim", 3),
        dir("/f/bos", 4),
        file("/f/log/a.log", 10, 1_000),
        file("/f/log/b.txt", 11, 2_000),
        file("/f/resim/c.log", 12, 4_000),
        file("/f/resim/d.txt", 13, 8_000),
    ];
    let second = vec![
        dir("/f/resim/derin", 5),
        file("/f/resim/derin/e.log", 14, 16_000),
        file("/f/bos/f.txt", 15, 32_000),
    ];
    for batch in [first, second] {
        index
            .apply(&mut batch.into_iter().map(Change::Upsert))
            .expect("apply");
        index.commit().expect("commit");
    }

    let weigh = |query: &str| {
        index
            .usage(&scour_core::UsageRequest {
                path: "/f".into(),
                top: 5,
                query: scour_query::parse(query),
            })
            .expect("usage")
    };

    // The `du` question, unchanged: every file under /f.
    let all = weigh("");
    assert_eq!((all.root.disk, all.root.files), (63_000, 6), "no filter");

    // The same folder, asked only about its logs: `log` matches three file names
    // and the directory called `log`, whose 99,000 is not content.
    for query in ["ext:log", "log"] {
        let some = weigh(query);
        assert_eq!(
            (some.root.disk, some.root.files),
            (21_000, 3),
            "{query} weighs the matching files and nothing else"
        );

        // Every child still there, including the one with no match in it —
        // and they come to the root exactly.
        let kids: Vec<(&str, u64)> = some
            .children
            .iter()
            .map(|c| (c.path.as_str(), c.disk))
            .collect();
        assert_eq!(
            kids,
            vec![("/f/resim", 20_000), ("/f/log", 1_000), ("/f/bos", 0)],
            "{query}: the tree is the same tree"
        );
        assert_eq!(
            some.children.iter().map(|c| c.disk).sum::<u64>(),
            some.root.disk,
            "{query}: the children come to the root"
        );
    }

    // A query nothing answers weighs nothing, rather than falling back to all
    // of it — which is what a filter quietly not being applied would look like.
    assert_eq!(weigh("ext:yok").root.disk, 0, "no match, no bytes");
}

/// Sorting by size puts a folder where its number says it is. A directory's
/// `Size` column is its entry table, about four kilobytes, so ordering by it put
/// every folder behind every file larger than a block — on a page of two hundred
/// out of two million, gone entirely.
#[test]
fn a_folder_sorts_by_the_number_it_shows() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");

    let mut rows = vec![
        // A folder whose own row is small and whose contents are not.
        Entry {
            id: EntryId::inode(SourceId(0), 66_310, 1),
            path: "/big".into(),
            is_dir: true,
            meta: Meta {
                mtime: NOW,
                size: 4_096,
                disk: 4_096,
                ..Meta::UNKNOWN
            },
        },
        // And a file that beats the folder's own row but not its contents.
        Entry {
            id: EntryId::inode(SourceId(0), 66_310, 2),
            path: "/middling.bin".into(),
            is_dir: false,
            meta: Meta {
                mtime: NOW,
                size: 500_000,
                disk: 500_000,
                ..Meta::UNKNOWN
            },
        },
    ];
    for i in 0..4u64 {
        rows.push(Entry {
            id: EntryId::inode(SourceId(0), 66_310, 10 + i),
            path: format!("/big/part{i}"),
            is_dir: false,
            meta: Meta {
                mtime: NOW,
                size: 1_000_000,
                disk: 1_000_000,
                ..Meta::UNKNOWN
            },
        });
    }
    index
        .apply(&mut rows.into_iter().map(Change::Upsert))
        .expect("apply");
    index.commit().expect("commit");

    let by_size = |index: &NativeIndex| -> Vec<String> {
        index
            .search(&SearchRequest {
                query: parse_at("", NOW),
                sort: SortKey::Size,
                descending: true,
                page: Page::new(0, 10),
            })
            .expect("search")
            .hits
            .into_iter()
            .map(|h| h.path)
            .collect()
    };

    // Cold: nothing has asked for a folder size, so the table is not built and
    // the folder sorts by its own column. Building it here costs 90 ms.
    let cold = by_size(&index);
    assert!(
        cold.iter().position(|p| p == "/big").unwrap()
            > cold.iter().position(|p| p == "/middling.bin").unwrap(),
        "a cold cache is the old order, not a wrong one: {cold:?}"
    );

    // Warm: `/big` holds 4 MB, so it goes above the 500 KB file and below the
    // 1 MB parts.
    index.subtree_sizes(&[]).expect("warm");
    let warm = by_size(&index);
    assert_eq!(warm[0], "/big", "the folder holds more than anything in it");
    assert!(
        warm.iter().position(|p| p == "/big").unwrap()
            < warm.iter().position(|p| p == "/middling.bin").unwrap(),
        "the folder has to sort by what it shows: {warm:?}"
    );

    // Ascending too, since a comparator is easy to get right in one direction.
    let up = index
        .search(&SearchRequest {
            query: parse_at("", NOW),
            sort: SortKey::Size,
            descending: false,
            page: Page::new(0, 10),
        })
        .expect("search");
    assert_eq!(
        up.hits.last().expect("rows").path,
        "/big",
        "the biggest is last when the order is reversed"
    );
}

#[test]
fn reported_disk_bytes_follow_publication_erasure_and_rebuild() {
    fn physical_bytes(dir: &std::path::Path) -> u64 {
        std::fs::read_dir(dir)
            .expect("index directory")
            .flatten()
            .filter_map(|entry| entry.metadata().ok())
            .map(|metadata| metadata.len())
            .sum()
    }

    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    assert_eq!(
        index.stats().expect("empty stats").bytes_on_disk,
        physical_bytes(tmp.path())
    );

    for (path, ino) in [("/w/a.rs", 1), ("/w/b.rs", 2), ("/w/c.rs", 3)] {
        index
            .apply(&mut [Change::Upsert(entry(path, NOW, ino))].into_iter())
            .expect("apply");
        index.commit().expect("commit");
    }
    assert_eq!(
        index.stats().expect("published stats").bytes_on_disk,
        physical_bytes(tmp.path()),
        "published segment files and the manifest all count"
    );

    index
        .apply(
            &mut [Change::RemoveSubtree {
                path: "/w/a.rs".into(),
            }]
            .into_iter(),
        )
        .expect("remove");
    index.commit().expect("commit removal");
    assert_eq!(
        index.stats().expect("erased stats").bytes_on_disk,
        physical_bytes(tmp.path()),
        "an erased segment must leave the cached physical total too"
    );

    let rebuilt = index
        .maintain(Maintenance::Rebuild)
        .expect("rebuild remaining segments");
    let physical = physical_bytes(tmp.path());
    assert_eq!(rebuilt.bytes_after, physical);
    assert_eq!(
        index.stats().expect("rebuilt stats").bytes_on_disk,
        physical
    );
    assert_eq!(
        index.stats().expect("cached stats").bytes_on_disk,
        physical,
        "a second quiet read must preserve the exact result"
    );
}

/// The scan reaches every row a search would, across every segment — the set,
/// not the order: `Index::scan` streams and does not sort, so both sides are
/// sorted here. Guards a segment walked but not merged, a hidden removal still
/// emitted, a block the zone map skipped that held a match.
#[test]
fn a_scan_reaches_every_row_a_search_would() {
    let f = Fixture::new(16_000, 2_000);
    assert!(
        f.index.stats().expect("stats").segments >= 8,
        "the fixture is supposed to be fragmented"
    );
    for q in [
        "",
        "rapor",
        "ext:rs",
        "*.pdf",
        "size:>1kb",
        "dm:>2020-01-01",
    ] {
        let mut got = Vec::new();
        let counted = f
            .index
            .scan(
                &scour_core::ScanRequest {
                    query: parse_at(q, NOW),
                },
                &mut |hit| {
                    got.push(hit.path.clone());
                    true
                },
            )
            .expect("scan");
        assert_eq!(counted as usize, got.len(), "{q:?}: the count is the rows");

        let mut want: Vec<String> = brute_force(
            &f.entries,
            &parse_at(q, NOW),
            SortKey::Modified,
            true,
            usize::MAX,
        )
        .into_iter()
        .map(|h| h.path)
        .collect();
        got.sort();
        want.sort();
        assert_eq!(got, want, "query {q:?} scanned a different set than exists");
    }
}

/// A scan and a count answer the same number — the one thing an export's reader
/// can check without this repository, so the one that must not drift.
#[test]
fn a_scan_counts_what_a_search_counts() {
    let f = Fixture::new(8_000, 1_000);
    for q in ["", "rapor", "ext:rs", "kind:image", "zzzz-nothing-matches"] {
        let scanned = f
            .index
            .scan(
                &scour_core::ScanRequest {
                    query: parse_at(q, NOW),
                },
                &mut |_| true,
            )
            .expect("scan");
        let counted = f
            .index
            .search(&SearchRequest {
                query: parse_at(q, NOW),
                sort: SortKey::Modified,
                descending: true,
                page: Page {
                    offset: 0,
                    limit: 0,
                    count_cap: 10_000_000,
                },
            })
            .expect("search")
            .total;
        assert_eq!(scanned, counted, "{q:?}: scan and count disagree");
    }
}

/// A reader that stops is obeyed at once, and the count says where. What must
/// not happen is the walk running to the end for somebody who has gone.
#[test]
fn a_scan_that_is_stopped_stops() {
    let f = Fixture::new(8_000, 1_000);
    let mut seen = 0u64;
    let counted = f
        .index
        .scan(
            &scour_core::ScanRequest {
                query: Ast::default(),
            },
            &mut |_| {
                seen += 1;
                seen < 10
            },
        )
        .expect("scan");
    assert_eq!(seen, 10);
    assert_eq!(counted, 10, "the count is what the caller was given");

    // And the index is untouched: no lock kept, no state left, the next question
    // answered in full. Against `f.entries` rather than the 8,000 asked for —
    // the generator makes directories too, so a number typed here would be a
    // fact about the generator.
    assert_eq!(
        f.index
            .scan(
                &scour_core::ScanRequest {
                    query: Ast::default()
                },
                &mut |_| true
            )
            .expect("scan again"),
        f.entries.len() as u64
    );
}

/// A removal that has not been written yet must not be exported. It is hidden
/// from searches when applied and erased at the next commit; between those the
/// row still matches, and an export is acted on when the difference is gone.
#[test]
fn a_scan_does_not_export_what_a_pending_removal_took() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    let entries = [
        entry("/a/keep.txt", NOW, 1),
        entry("/a/gone.txt", NOW, 2),
        entry("/a/also-gone.txt", NOW, 3),
    ];
    index
        .apply(&mut entries.iter().cloned().map(Change::Upsert))
        .expect("apply");
    index.commit().expect("commit");

    // Applied and deliberately not committed.
    index
        .apply(
            &mut ["/a/gone.txt", "/a/also-gone.txt"]
                .into_iter()
                .map(|p| Change::RemoveSubtree { path: p.into() }),
        )
        .expect("remove");

    let mut got = Vec::new();
    index
        .scan(
            &scour_core::ScanRequest {
                query: Ast::default(),
            },
            &mut |h| {
                got.push(h.path.clone());
                true
            },
        )
        .expect("scan");
    assert_eq!(got, ["/a/keep.txt".to_owned()]);
}

/// A page **reached** rather than walked to, checked against the truth. `search`
/// bisects for the date the page begins at and counts the rows above it out of a
/// rank over the live bitmap; every way that can be wrong returns a fast,
/// plausible page, so every offset is compared.
#[test]
fn a_reached_page_is_the_page_the_walk_would_have_found() {
    let f = Fixture::new(16_000, 2_000);
    assert!(
        f.index.stats().expect("stats").segments >= 8,
        "the fixture is supposed to be fragmented"
    );
    for &(offset, limit) in &[
        (1_999usize, 60usize),
        (2_000, 100),
        (2_001, 40),
        (5_000, 200),
        (9_999, 37),
        (15_800, 200),
        (16_000, 50),
    ] {
        // `brute_force` takes a limit and not an offset, so the reference is
        // the whole prefix, sliced.
        let whole = brute_force(
            &f.entries,
            &parse_at("", NOW),
            SortKey::Modified,
            true,
            offset + limit,
        );
        let want: Vec<String> = whole[offset.min(whole.len())..]
            .iter()
            .map(|h| h.path.clone())
            .collect();
        let got = f
            .index
            .search(&SearchRequest {
                query: parse_at("", NOW),
                sort: SortKey::Modified,
                descending: true,
                page: Page {
                    offset: offset as u32,
                    limit: limit as u32,
                    count_cap: 10_000_000,
                },
            })
            .expect("search");
        let paths: Vec<String> = got.hits.iter().map(|h| h.path.clone()).collect();
        assert_eq!(
            paths, want,
            "the page at {offset}+{limit} disagrees with brute force"
        );
        // And that it was reached, not walked to: without this a reach that
        // silently declined leaves the test passing and the cost unchanged.
        if offset >= 2_000 {
            // Not merely fewer than the offset: a reach landing on the wrong
            // date is still correct, since the merge walks forward, and would
            // pass a looser bound at the walk's price. No group here shares a
            // second, so a page is the page and little else.
            assert!(
                got.rows_visited < (limit * 4) as u64,
                "the page at {offset} visited {} rows for {limit} — it was walked to",
                got.rows_visited
            );
        }
    }
}

/// A reach over segments whose dates do not overlap. The first bisection
/// bracketed on the newest date the segments had *in common*, so an index of one
/// segment from this morning and one from last year searched a window without
/// the answer — and still returned the right page, after ninety thousand rows.
#[test]
fn a_reach_over_segments_that_share_no_dates_still_lands_on_the_page() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    let mut all: Vec<Entry> = Vec::new();
    // Four segments, each a year apart and each written after the last, so no
    // two of them hold a date between them.
    for era in 0..4i64 {
        let part: Vec<Entry> = (0..3_000)
            .map(|i| {
                entry(
                    &format!("/corpus/{era}/file{i:06}.txt"),
                    NOW - era * 31_536_000 - i,
                    (era * 100_000 + i) as u64 + 1,
                )
            })
            .collect();
        index
            .apply(&mut part.iter().cloned().map(Change::Upsert))
            .expect("apply");
        index.commit().expect("commit");
        all.extend(part);
    }
    for &(offset, limit) in &[(2_500usize, 50usize), (6_000, 100), (9_500, 200)] {
        let whole = brute_force(
            &all,
            &parse_at("", NOW),
            SortKey::Modified,
            true,
            offset + limit,
        );
        let want: Vec<String> = whole[offset.min(whole.len())..]
            .iter()
            .map(|h| h.path.clone())
            .collect();
        let answer = index
            .search(&SearchRequest {
                query: parse_at("", NOW),
                sort: SortKey::Modified,
                descending: true,
                page: Page {
                    offset: offset as u32,
                    limit: limit as u32,
                    count_cap: 10_000_000,
                },
            })
            .expect("search");
        let got: Vec<String> = answer.hits.into_iter().map(|h| h.path).collect();
        assert_eq!(
            got, want,
            "the page at {offset}+{limit} disagrees with brute force"
        );
        assert!(
            answer.rows_visited < (limit * 4) as u64,
            "the page at {offset} visited {} rows — the bracket missed it",
            answer.rows_visited
        );
    }
}

/// The same, over an index where a thousand files share every timestamp and one
/// in seven has been deleted. A date shared by a thousand rows has no rank
/// inside it, so the merge steps through the part preceding the page; a deleted
/// row must not be counted but still spends a place in the row numbering.
#[test]
fn a_reached_page_survives_shared_dates_and_deleted_rows() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    // Twelve thousand files, a thousand to a date, spread over twelve folders
    // so that path order and row order are not the same thing.
    let all: Vec<Entry> = (0..12_000)
        .map(|i| {
            entry(
                &format!("/corpus/{:02}/file{i:06}.txt", i % 12),
                NOW - (i / 1_000) as i64 * 86_400,
                i as u64 + 1,
            )
        })
        .collect();
    for part in all.chunks(2_000) {
        index
            .apply(&mut part.iter().cloned().map(Change::Upsert))
            .expect("apply");
        index.commit().expect("commit");
    }
    let (gone, alive): (Vec<Entry>, Vec<Entry>) = all
        .iter()
        .cloned()
        .partition(|e| e.meta.mtime % 7 == 0 && e.path.ends_with("3.txt"));
    assert!(!gone.is_empty(), "the fixture is supposed to lose rows");
    index
        .apply(&mut gone.iter().map(|e| Change::RemoveSubtree {
            path: e.path.clone(),
        }))
        .expect("remove");
    index.commit().expect("commit");

    for &(offset, limit) in &[
        (2_000usize, 100usize),
        (2_500, 60),
        (4_999, 200),
        (8_000, 120),
        (11_000, 200),
    ] {
        let whole = brute_force(
            &alive,
            &parse_at("", NOW),
            SortKey::Modified,
            true,
            offset + limit,
        );
        let want: Vec<String> = whole[offset.min(whole.len())..]
            .iter()
            .map(|h| h.path.clone())
            .collect();
        let answer = index
            .search(&SearchRequest {
                query: parse_at("", NOW),
                sort: SortKey::Modified,
                descending: true,
                page: Page {
                    offset: offset as u32,
                    limit: limit as u32,
                    count_cap: 10_000_000,
                },
            })
            .expect("search");
        let got: (Vec<String>, u64) = (
            answer.hits.into_iter().map(|h| h.path).collect(),
            answer.rows_visited,
        );
        assert_eq!(
            got.0, want,
            "the page at {offset}+{limit} disagrees with brute force"
        );
        // A thousand rows share every date, so a page inside a group steps
        // through the part before it — never the thousands above the group.
        assert!(
            got.1 < 1_200 + limit as u64,
            "the page at {offset} visited {} rows — it was walked to",
            got.1
        );
    }
}

/// A walk of several roots keeps what it saw in every one of them. Unchanged-row
/// marks were consumed by the first root's sweep, so every later root was
/// reconciled against nothing and deleted — `/opt` alternated between 5,477 rows
/// and none. A sweep takes every root of the pass at once.
#[test]
fn a_sweep_of_several_roots_keeps_what_the_walk_saw_in_each() {
    fn all_paths(index: &NativeIndex) -> Vec<String> {
        let mut out: Vec<String> = index
            .search(&SearchRequest {
                query: parse_at("", NOW),
                page: Page::new(0, 10),
                ..Default::default()
            })
            .expect("search")
            .hits
            .into_iter()
            .map(|hit| hit.path)
            .collect();
        out.sort();
        out
    }

    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    let first = entry("/usr/one.txt", NOW, 1);
    let second = entry("/opt/two.txt", NOW, 2);
    let third = entry("/var/three.txt", NOW, 3);
    // And one the walk will not find, to prove the sweep still does its job.
    let gone = entry("/opt/gone.txt", NOW, 4);
    index
        .apply(
            &mut [
                Change::Upsert(first.clone()),
                Change::Upsert(second.clone()),
                Change::Upsert(third.clone()),
                Change::Upsert(gone),
            ]
            .into_iter(),
        )
        .expect("apply");
    index.commit().expect("commit");

    // A walk that finds three of the four rows exactly as they are: nothing is
    // rewritten, and each is marked as seen instead.
    let generation = index.begin_generation().expect("generation");
    index
        .apply(
            &mut [
                Change::Upsert(first),
                Change::Upsert(second),
                Change::Upsert(third),
            ]
            .into_iter(),
        )
        .expect("re-apply unchanged");
    index.commit().expect("commit");

    let roots = ["/usr".to_string(), "/opt".to_string(), "/var".to_string()];
    let removed = index
        .sweep(SourceId(0), &roots, generation, &PrefixSet::default())
        .expect("sweep every root of the walk");

    assert_eq!(removed, 1, "only the row the walk did not find");
    assert_eq!(
        all_paths(&index),
        ["/opt/two.txt", "/usr/one.txt", "/var/three.txt"],
        "a root swept after the first must keep the rows the walk saw"
    );
}
