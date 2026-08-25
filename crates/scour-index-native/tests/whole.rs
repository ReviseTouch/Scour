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

/// Ordering by path is a stored order now, and it has to be the same order.
///
/// **The change this is the gate for.** A segment lists its rows in path order
/// when it is written, so a page ordered by path is a read of two hundred
/// positions instead of a key built for every match — measured on 2,235,402
/// rows, whole table, page of two hundred: **291 ms becomes 0.8**, and the peak
/// resident size of the same benchmark falls from 184 MB to 120 because the
/// discarded keys were two million strings.
///
/// What could go wrong is not subtle and is completely invisible to that
/// measurement: a stored order that is not the order. So every query that
/// reaches it is checked against brute force, in both directions, and at an
/// offset — a stored order that is right at the front and wrong further in is
/// exactly what a page of the first fifty would not show.
///
/// The queries are the ones that reach it: none reads a name, because the
/// folded arena is walked sequentially and positions are not sequential. Every
/// text query narrows through the trigram filter instead and keeps the walk it
/// always had.
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
            // Deep enough to be past the first page and its boundary. The
            // reference is asked for the whole prefix and sliced, because
            // `brute_force` takes a limit and not an offset.
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

    // And the point of it: the walk stops. Without the stored order this
    // visits every row of every segment to find out which two hundred paths
    // come first, and the answer is identical either way — which is why the
    // cost has to be asserted and not just the list.
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

/// Ordering by name is a stored order too, and its shortcut is held to the
/// public reference rather than to another implementation detail.
///
/// These are exactly the queries that may stream the order: none has to read a
/// name to decide whether a row matches. Text queries keep the sequential name
/// walk after the trigram filter narrows their blocks.
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

/// Extension order is persisted for the broad list shown by the GUI.
///
/// Extensions have only a modest number of values, so most rows tie. The
/// order therefore has to preserve both the primary extension and the public
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
///
/// The primary direction changes, while ties remain newest-first and then
/// path-first. Names tie often enough that this is not a corner case: files
/// such as `Cargo.toml`, `index.js`, and `README` occur throughout a tree.
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

    // Both are twelve bytes before folding and eighteen afterwards. Their
    // first sixteen folded bytes are identical, so a merge that treats the
    // `Head` as exact — or resolves it with the whole name — gets the page
    // boundary wrong.
    let common = "Ⱥ".repeat(5);
    let grown_a = format!("{common}Ⱥ");
    let grown_b = format!("{common}Ⱦ");
    assert_eq!(grown_a.len(), 12);
    assert_eq!(grown_b.len(), 12);
    let folded_a = DefaultFolder.fold(&grown_a);
    let folded_b = DefaultFolder.fold(&grown_b);
    assert_eq!(&folded_a.as_bytes()[..16], &folded_b.as_bytes()[..16]);
    assert_ne!(folded_a, folded_b);

    // Seven dotless i characters contract from fourteen raw bytes to seven.
    // They remain ineligible; an ASCII suffix with the same folded spelling is
    // eligible and lets the extension filter expose any post-fold decision.
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
///
/// **The gap this closes is why it went unnoticed.** The agreement test above
/// checks `Modified` *descending* against brute force and every other key in
/// both directions — but never `Modified` ascending, which is precisely the
/// order that has just stopped visiting every match and started walking the
/// stored one backwards.
///
/// Two things are checked and the second is the delicate one:
///
/// * the same queries agree with brute force, oldest-first;
/// * a corpus where **thousands of files share one second** pages correctly.
///   A backwards walk yields dates in order and paths *reversed* inside a
///   date, because inside a segment the row number is the path order. A page
///   whose edge falls inside such a group would otherwise be handed the last
///   paths to choose from rather than the first, which is wrong in a way that
///   looks entirely reasonable — the right dates, plausible names, and
///   nothing in the page to say it is not the answer.
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

    /* **And the walk's own answer, not only the merge's.**
     *
     * `run_with` is public and has two ways out: the merge takes `ranked` and
     * orders it itself, and a single-segment caller takes `hits` already
     * ordered. Everything above goes through the first, so the second was
     * uncovered — removing its sort changed nothing any test could see, which
     * is how a guard becomes a hope. Its rows arrive in date order with the
     * paths reversed inside a date, so without that sort a tie group comes
     * back backwards.
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

/// A page that ends inside a tie on sixteen bytes is still the page.
///
/// **The one group a bounded selection cannot drop.** Ordering by name no
/// longer keeps a candidate per match — the walk holds a page's worth and
/// rejects the rest as it goes — and the name key is the first *sixteen* bytes
/// of the folded name, an abbreviation. So two rows that tie on it are not
/// equal, and which of them wins is decided later against the full name. A
/// selection that keeps only the best `need` returns a page that is
/// deterministic, plausible, and not the one brute force gives, the moment its
/// edge falls inside such a group.
///
/// Nothing above forces that. `many_segments_answer_exactly_what_one_would`
/// compares every key against brute force, but on generated names the boundary
/// landing inside a sixteen-byte tie is luck. Here two hundred names agree on
/// their first sixteen bytes and differ afterwards, twenty sort before them and
/// twenty after, and the page is asked for at every edge of the group.
///
/// Written in the reverse of the order they sort in, so a selection that keeps
/// whichever row it met first cannot pass by accident. Split across segments,
/// because the group is then resolved by the merge comparator in `index.rs` as
/// well as by `narrow` — the two are the same rule written twice, and the
/// second is hardcoded to compare folded *names*.
///
/// Sorted by path as well, where the key is exact and the group must **not** be
/// kept: `key_is_exact` has had `Path` added to it once already.
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
    // nothing else. The dates repeat every third file, so the tie-break behind
    // the name is exercised too.
    for i in 0..200u64 {
        let n = 199 - i;
        all.push(entry(
            &format!("/t/sozlesme_arsivi_{n:03}.rs"),
            NOW - 5_000 - (n as i64 % 3),
            30_000 + i,
        ));
    }
    // The same names spelled differently, in another directory: the key is over
    // the *folded* name, so these join the group and half of them tie with a
    // lowercase one exactly.
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

/// A numeric order stops opening blocks, and stops at the right place.
///
/// **The failure this guards is a plausible page.** Ordering by a number no
/// longer visits every match: a block carries the range of every column, so
/// the blocks are opened in the order of what each can reach and abandoned
/// once the page is beyond all of them. Three things decide whether that is
/// still the same list, and each has its own half of this test.
///
/// * **The tie group at the edge.** Sizes and dates tie in the thousands on a
///   real disk, and the page breaks a tie on the row — so a block that can
///   only *equal* the worst row held may still displace it, when its rows come
///   first. Here every value is shared by a thousand files whose names sort
///   the opposite way from the order they were written in, so nothing can pass
///   by luck.
/// * **The count**, which is a separate obligation. Deciding the page says
///   nothing about the total printed beside it, and a cap allowed to stop the
///   selection returns "the largest forty among the first hundred" — the
///   mistake this engine has already made once.
/// * **The offset**, because the list pages to twenty thousand.
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
                // Four sizes, seven creation dates, three block counts, and
                // one kind for all of them — every one a tie group wider than
                // a page.
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
    // empty query on purpose: an empty one takes the total from the segment's
    // own live count and never hands the cap to the walk at all, which is
    // exactly where this would look fine while being wrong.
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

/// A folder still sorts by what is under it once blocks are being skipped.
///
/// **The trap the block order sets for `sort:size`.** A directory sorts by its
/// rollup and not by its `Size` column, so the column's range is not a bound
/// on what the block can reach — and a block ordered by a range that does not
/// cover its own rows is a block that gets skipped. Here the largest folder in
/// the index sits in a block of nothing but tiny files, so a bound taken from
/// the column alone would put that block last and drop the folder off a page
/// it should be leading.
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

/// A failed bitmap replacement must leave enough state for the next commit.
///
/// The first attempt has already killed the row in memory, so asking the same
/// removal to run again reports zero. Without a separate dirty-bitmap stamp,
/// the retry then writes nothing and a restart resurrects the file.
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
/// bitmap. Otherwise retrying deletes both the missing row and the row the walk
/// explicitly saw.
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
    // The one thing that may not wait for a commit. Deleting a file and still
    // seeing it reads as a broken program, so the removal takes effect in the
    // overlay first and in the files afterwards.
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
    // nine files written one at a time, and a kill in the middle leaves a
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
        .sweep(SourceId(0), &["".to_string()], g, &PrefixSet::default())
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

    // And so does an order the row layout says nothing about, for the other
    // reason: the blocks are opened in the order of the largest size each one
    // holds, so once forty rows beat everything the next block could contain
    // there is nothing left to open.
    //
    // **This assertion used to be the opposite**, and read "an order it cannot
    // serve says so rather than pretending". It was true then. What has to
    // stay true either way is the line below it: a cap may bound the total,
    // never the result.
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

/// An index written before the path order existed is read, not thrown away.
///
/// **The format decision, tested from the outside.** The path order is the one
/// part of a segment that may be missing, and that is what lets an existing
/// index keep working: a rescan of two million files across two volumes is a
/// long time to be without a search box, and nothing about the seven files that
/// were already there has changed meaning. So the version is not bumped, the
/// old segments are read exactly as they were, and each one gains the file the
/// next time it is folded — which is a read of the index rather than of the
/// disk.
///
/// Simulated by deleting what an older build would never have written. Both
/// halves are asserted, and the second is the one that says the fallback is
/// really being taken rather than the file quietly reappearing:
///
/// * every answer is the same as with the order present, and the same as brute
///   force;
/// * the walk visits every row again, because without the order there is
///   nothing to stop it.
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

/// Name-order files are an optional acceleration, independently per segment.
///
/// An upgrade therefore has three ordinary states: all current segments,
/// current and legacy segments mixed, and an entirely legacy index. Every one
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

/// Half the segments having a path order is the ordinary state, not a corner.
///
/// **What an existing index looks like for as long as it takes to compact.**
/// The old segments have no order and the ones a watcher commits do, so a
/// search hands the merge candidates chosen two different ways — streamed out
/// of a stored order in one segment, keyed per match in the next — and it has
/// to be unable to tell. It is, by construction: positions never leave the
/// segment that holds them, and what every segment hands over is the whole
/// path either way. Construction is what the last three attempts on this file
/// were also confident about, so it is measured against brute force instead.
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

/// A path order that is there and wrong is damage, not an older index.
///
/// The two are told apart by one thing — whether the file exists — so the case
/// that has to be nailed down is the file that exists and does not describe the
/// segment. Reading it anyway would produce a page that is ordered, plausible
/// and short of whatever the file stopped before, which is the class of failure
/// this crate keeps a brute-force reference to catch. It is refused instead,
/// and as damage rather than as a version, because nothing about it is old.
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
        .sweep(SourceId(0), &["".to_string()], g, &PrefixSet::default())
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

/// The report can be asked about part of a folder rather than all of it.
///
/// A query narrows *which rows are weighed* and changes nothing else: the tree
/// is the same tree, a folder with no match is still in it, and the children
/// still come to the root. That last one is the property worth a test —
/// dropping the folders that hold no match would leave a match's bytes rolled
/// into some grandparent, so the headline would count what no row beneath it
/// admitted to, and every part of that reads as a bug in the totals.
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
    // Made large, so that a version counting a directory's own row could not
    // pass — and one of these folders is *named* for the query below, which is
    // how that row reaches the rollup at all.
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

    // The same folder, asked only about its logs. The extension filter and the
    // bare word have to land on the same number: `log` matches three file names
    // *and* the directory called `log`, whose 99,000 is not content.
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

/// Sorting by size puts a folder where its number says it is.
///
/// **The failure this guards made folders vanish.** A directory's `Size`
/// column is its own entry table — about four kilobytes — so ordering by it
/// put every folder behind every file larger than a block. On a page of two
/// hundred rows out of two million, that is not "mis-sorted", it is gone: a
/// size-sorted list had no folders in it at all, while the column beside it
/// said one of them held thirteen gigabytes.
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
    // the folder sorts by its own column — the old behaviour, on purpose,
    // because building it here would put ninety milliseconds in a keystroke.
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

/// The scan reaches every row a search would, across every segment.
///
/// **The set, not the order.** `Index::scan` streams and does not sort, so
/// this compares against the *set* brute force produces — both sides sorted
/// here, so that two lists of the same paths compare equal whatever order they
/// arrived in.
///
/// What it guards is a class of bug paging never had: a segment walked but not
/// merged, a hidden removal the walk still emits, a block the zone map skipped
/// that held a match. Each returns a plausible file that is short of the truth
/// by a few thousand rows, and nothing but a comparison against every entry
/// notices.
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

/// A scan and a count answer the same number.
///
/// The one thing an export's reader can check without this repository, so it
/// is the one that must not drift: `scour count` and the row count of `scour
/// export` are the same walk asked two ways.
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

/// A reader that stops is obeyed at once, and the count says where.
///
/// A cancelled download, seen from the bottom of the stack. What must not
/// happen is the walk running to the end anyway: on the owner's machine that
/// is two million rows of front-coded paths rebuilt for somebody who has gone.
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

    // And the index is untouched by having been abandoned: no lock kept, no
    // state left behind, the next question answered in full. Against
    // `f.entries` rather than the 8,000 asked for — the generator makes
    // directories as well as files, and a number typed here would be a fact
    // about the generator rather than about the scan.
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

/// A removal that has not been written yet must not be exported.
///
/// It is hidden from searches the moment it is applied and erased at the next
/// commit, and between those two moments the row is still in the segment and
/// still matches. A scan reading the segment directly would export files that
/// are gone — worse in an export than on a page, because a spreadsheet is
/// acted on later, when the difference is no longer there to see.
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

/// A page **reached** rather than walked to, checked against the truth.
///
/// Past a few thousand rows `search` stops passing over everything above the
/// page: it bisects for the date the page begins at, counts the rows above it
/// out of a rank over the live bitmap, and merges from there. That replaces
/// two million row visits with a few thousand column reads, and every way it
/// can be wrong returns a *fast, plausible* page — one row late, a segment's
/// dead rows counted as live, a group of files sharing a second entered at the
/// wrong place. None of those is visible to a benchmark, so every offset here
/// is compared with brute force.
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
        // **And that it was reached, not walked to.** The list being right is
        // half the claim; the other half is that the rows above it were never
        // visited, and without this assertion a reach that silently declined
        // would leave this test passing and the cost unchanged.
        if offset >= 2_000 {
            // Not merely fewer than the offset: a reach that lands on the
            // wrong date is still *correct* — the merge walks forward from
            // wherever it started — and would pass a looser bound while
            // costing what the walk cost. This corpus has no group of any
            // size sharing a second, so a page here is the page and little
            // else.
            assert!(
                got.rows_visited < (limit * 4) as u64,
                "the page at {offset} visited {} rows for {limit} — it was walked to",
                got.rows_visited
            );
        }
    }
}

/// A reach over segments whose dates do not overlap.
///
/// The case that caught the first version of the bisection. Its bracket was
/// the newest date the segments had *in common* rather than the newest in any
/// of them, so an index holding one segment written this morning and one
/// holding last year's files searched a window that did not contain the
/// answer. It still returned the right page — the merge walks forward from
/// wherever it starts — while visiting ninety thousand rows to do it, which
/// is the shape of a fast path that has quietly stopped being one.
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

/// The same, over an index where a thousand files share every timestamp and
/// one in seven has been deleted.
///
/// Both of those are what the reach has to get right and what a generated
/// corpus is too tidy to exercise. A date shared by a thousand rows has no
/// rank inside it — the merge has to step through the part of the group that
/// precedes the page — and a deleted row is one the bisection must not count
/// but the row numbering still spends a place on.
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
        // A thousand rows share every date here, so a page inside one of
        // those groups steps through the part of it that comes first — but
        // never through the two, five or eleven thousand rows above the group.
        assert!(
            got.1 < 1_200 + limit as u64,
            "the page at {offset} visited {} rows — it was walked to",
            got.1
        );
    }
}

/// A walk of several roots keeps what it saw in every one of them.
///
/// **This is what a live index was doing.** `scourd`'s system source walks
/// `/usr /etc /opt /var` as one source, and the engine swept each root in
/// turn. The unchanged-row marks — one bit a row, the only thing saying "the
/// walk saw this and it had not moved" — were consumed by the first sweep, so
/// every root after it was reconciled against nothing and everything it held
/// was deleted as missing. The next walk found those rows genuinely absent,
/// rewrote them, and the walk after that deleted them again: measured on the
/// live index as `/opt` alternating between 5,477 rows and none, `/etc`
/// between 2,309 and 34, about once a minute for as long as the service was
/// up. The first root never suffered, which is what made it look like a walk
/// stopping early rather than a sweep eating its own evidence.
///
/// A sweep now takes every root of the pass at once, which is what makes the
/// marks last as long as the thing they are evidence for.
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
