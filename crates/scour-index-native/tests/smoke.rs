//! The index, checked against the truth.
//!
//! Every search here is compared with [`scour_mock::brute_force`], which looks
//! at every entry and cannot take a shortcut. The engine this replaces had
//! five defects found this way, three of which returned a *fast, wrong*
//! answer — a timing benchmark called all three a success.
//!
//! A hand-written index has strictly more surfaces like that than a library
//! does, which is exactly why this file exists before the index is wired to
//! anything.

use scour_core::{Entry, EntryId, Meta, SortKey, SourceId};
use scour_index_native::{
    ColumnBlocks, DirTable, ExtensionOrder, NameArena, NameOrder, PathOrder, Plan, Segment,
    SegmentBytes, TrigramIndex, Wanted, build, run,
};
use scour_mock::{MockOptions, brute_force, generate};
use scour_query::parse_at;

/// Fixed, so relative date terms are assertable.
const NOW: i64 = 1_785_000_000;

struct Fixture {
    bytes: SegmentBytes,
    entries: Vec<Entry>,
}

impl Fixture {
    fn new(files: usize) -> Fixture {
        let fs = generate(&MockOptions {
            files,
            now: NOW,
            ..Default::default()
        });
        Fixture {
            bytes: build(&fs.entries),
            entries: fs.entries,
        }
    }

    fn segment(&self) -> Segment<'_> {
        Segment {
            names: NameArena::open(&self.bytes.names).expect("names"),
            folded: NameArena::open(&self.bytes.fnames).expect("fnames"),
            cols: ColumnBlocks::open(&self.bytes.cols).expect("cols"),
            dirs: DirTable::open(&self.bytes.dirs).expect("dirs"),
            tri: TrigramIndex::open(&self.bytes.tri_dict, &self.bytes.tri_post).expect("tri"),
            porder: PathOrder::open(&self.bytes.porder),
            norder: NameOrder::open(&self.bytes.norder),
            eorder: ExtensionOrder::open(&self.bytes.eorder),
            alive: &self.bytes.alive,
        }
    }

    fn search(&self, q: &str, sort: SortKey, desc: bool, limit: usize) -> Vec<String> {
        let seg = self.segment();
        let plan = Plan::compile(&parse_at(q, NOW), &seg).expect("compile");
        run(
            &seg,
            &plan,
            Wanted {
                sort,
                descending: desc,
                offset: 0,
                limit,
                count_cap: 10_000_000,
                rank_only: false,
            },
        )
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

    fn expected_count(&self, q: &str) -> u64 {
        brute_force(
            &self.entries,
            &parse_at(q, NOW),
            SortKey::Modified,
            true,
            usize::MAX,
        )
        .len() as u64
    }

    fn check(&self, q: &str, sort: SortKey, desc: bool) {
        assert_eq!(
            self.search(q, sort, desc, 50),
            self.expected(q, sort, desc, 50),
            "query {q:?} sorted by {sort:?} (desc={desc}) disagrees with brute force"
        );
    }
}

#[test]
fn results_match_brute_force_for_every_kind_of_term() {
    let f = Fixture::new(20_000);
    for q in [
        "",
        "rapor",
        "colpan",
        "main",
        "zzzznothing",
        "ab",
        "ext:rs",
        "ext:rs;toml",
        "*.rs",
        "rap*or",
        "folder:",
        "file:",
        "kind:code",
        "kind:image",
        "size:>1mb",
        "size:<1kb",
        "size:=0",
        "path:eski-arsiv",
        // **The three shapes a path term comes in**, because the fast answer
        // is built from a directory table and each of them reaches it a
        // different way: inside a directory, straddling the separator between
        // the directory and the name, and — with no separator at all — inside
        // the name.
        "Projeler/eski-arsiv",
        "eski-arsiv/rapor",
        "path:/home/u",
        "arsiv/",
        "under:/home/u/Projeler",
        "under:/home/u/Projeler/eski-arsiv",
        "parent:/home/u",
        "under:/home/u/Projeler ext:rs",
        "rapor ext:pdf",
        "rapor|belge",
        "ext:rs !main",
        "dm:30d",
        "dc:>2024-01-01",
        "İSTANBUL",
    ] {
        f.check(q, SortKey::Modified, true);
    }
}

#[test]
fn every_sort_key_matches_brute_force() {
    let f = Fixture::new(8_000);
    for key in [
        SortKey::Name,
        SortKey::Path,
        SortKey::Size,
        SortKey::Modified,
        SortKey::Created,
        SortKey::Accessed,
        SortKey::Ext,
        SortKey::Kind,
        SortKey::Disk,
        SortKey::Mode,
    ] {
        f.check("ext:rs", key, false);
        f.check("ext:rs", key, true);
    }
}

#[test]
fn the_newest_page_stops_almost_immediately() {
    // The claim the whole layout rests on: rows are in date order, so the
    // default view reads a page and stops instead of visiting every match.
    let f = Fixture::new(50_000);
    let seg = f.segment();
    let plan = Plan::compile(&parse_at("", NOW), &seg).expect("compile");
    let found = run(
        &seg,
        &plan,
        Wanted {
            sort: SortKey::Modified,
            descending: true,
            offset: 0,
            limit: 40,
            count_cap: 200,
            rank_only: false,
        },
    );
    assert!(found.early_exit, "the stored order should let this stop");
    assert!(
        found.rows_visited <= 260,
        "should visit about the cap, not the corpus: {} of {}",
        found.rows_visited,
        seg.rows()
    );
    assert_eq!(found.hits.len(), 40);
    assert!(found.capped);

    // And the same query sorted another way genuinely cannot stop.
    let all = run(
        &seg,
        &plan,
        Wanted {
            sort: SortKey::Size,
            descending: true,
            offset: 0,
            limit: 40,
            count_cap: 10_000_000,
            rank_only: false,
        },
    );
    assert!(!all.early_exit);
    assert_eq!(all.rows_visited as usize, seg.rows());
}

#[test]
fn empty_relevance_has_one_tie_order_in_both_directions() {
    let f = Fixture::new(50_000);
    let ascending = f.search("", SortKey::Relevance, false, 50);
    let descending = f.search("", SortKey::Relevance, true, 50);
    assert_eq!(ascending, descending);
    assert_eq!(
        ascending,
        f.expected("", SortKey::Relevance, false, 50),
        "the stored tie order must be the brute-force order"
    );

    let seg = f.segment();
    let plan = Plan::compile(&parse_at("", NOW), &seg).expect("compile");
    let found = run(
        &seg,
        &plan,
        Wanted {
            sort: SortKey::Relevance,
            descending: false,
            offset: 0,
            limit: 50,
            count_cap: 200,
            rank_only: false,
        },
    );
    assert!(
        found.rows_visited < 500,
        "an equal relevance key should read a page, not {} rows",
        found.rows_visited
    );
}

#[test]
fn a_count_cap_never_changes_which_rows_win() {
    // The bug this exists for: capping the count also stopped the walk, so
    // "the largest forty" quietly became "the largest forty among the five
    // hundred newest" — a plausible answer to a different question. The cap
    // may bound the *total*; it may never bound the *result*.
    let f = Fixture::new(20_000);
    let seg = f.segment();
    let plan = Plan::compile(&parse_at("ext:rs", NOW), &seg).expect("compile");
    let by_size = |cap: usize| {
        run(
            &seg,
            &plan,
            Wanted {
                sort: SortKey::Size,
                descending: true,
                offset: 0,
                limit: 40,
                count_cap: cap,
                rank_only: false,
            },
        )
        .hits
        .into_iter()
        .map(|h| h.path)
        .collect::<Vec<_>>()
    };
    let want = f.expected("ext:rs", SortKey::Size, true, 40);
    assert_eq!(by_size(10_000_000), want);
    assert_eq!(by_size(100), want, "a small cap must not change the answer");
    assert_eq!(by_size(1), want);
}

/// Ordering by a number opens a page of blocks, not all of them.
///
/// The claim: each block records the range of every column, so the blocks can
/// be put in the order of what they can reach and abandoned once the page is
/// beyond them. What has to survive it is the answer — every one of these is
/// compared against the reference that looks at every entry.
///
/// `Kind` is the awkward one and is here for that. A handful of values covers
/// the whole corpus, so a block that can only *equal* the worst row held is
/// commonplace — and such a block is not out of reach, because the page breaks
/// ties on the row and its rows may come first. A version that stopped on
/// "equal" would return the right values from the wrong rows.
#[test]
fn a_numeric_order_opens_a_page_of_blocks_rather_than_all_of_them() {
    let f = Fixture::new(50_000);
    let seg = f.segment();
    let plan = Plan::compile(&parse_at("", NOW), &seg).expect("compile");
    for (sort, desc) in [
        (SortKey::Size, true),
        (SortKey::Size, false),
        (SortKey::Created, true),
        (SortKey::Created, false),
        (SortKey::Kind, true),
        (SortKey::Kind, false),
        (SortKey::Disk, true),
        (SortKey::Mode, true),
    ] {
        // A cap the count reaches long before the selection does, which is the
        // shape of every request the interface makes.
        let found = run(
            &seg,
            &plan,
            Wanted {
                sort,
                descending: desc,
                offset: 0,
                limit: 40,
                count_cap: 200,
                rank_only: false,
            },
        );
        assert!(
            found.early_exit,
            "{sort:?} (desc={desc}) opened every block"
        );
        assert!(
            found.rows_visited * 4 < seg.rows() as u64,
            "{sort:?} (desc={desc}) visited {} of {} rows",
            found.rows_visited,
            seg.rows()
        );
        assert_eq!(
            found.hits.into_iter().map(|h| h.path).collect::<Vec<_>>(),
            f.expected("", sort, desc, 40),
            "{sort:?} (desc={desc}) disagrees with brute force"
        );
        // Stopping the selection says nothing about the total. It is still the
        // cap, and still a floor.
        assert_eq!(found.total, 200, "{sort:?} (desc={desc}) miscounted");
        assert!(found.capped);
    }
}

/// A block that can only *equal* the page's worst row is still opened.
///
/// **The one-character version of this optimisation that is wrong.** Blocks are
/// opened best first and the walk ends when the page's worst row beats
/// everything the next block could hold — but "beats" may not be weakened to
/// "is not beaten by". The page's second key is the row, so a block whose best
/// merely ties the worst row held still displaces it whenever its rows come
/// first.
///
/// The corpus is shaped to make that difference visible rather than to look
/// like a disk. Dates fall by one a row, so a row number is known rather than
/// guessed at. Every file is empty except twenty-five, one in each of the last
/// twenty-five blocks — so those blocks are opened first, and the page fills
/// with their *empty* rows, which live at the very end of the segment. Every
/// remaining block ties them at zero, and every one of them holds earlier rows
/// that belong in the page instead.
///
/// Stopping on the tie returns twenty-five eight-kilobyte files and fifteen
/// empty ones: the right sizes, in the right order, and the wrong fifteen
/// files. Only the reference can tell the two apart.
#[test]
fn a_block_that_only_ties_the_page_is_still_opened() {
    let entries: Vec<Entry> = (0..4_000i64)
        .map(|i| {
            let path = format!("/t/{i:05}.bin");
            Entry {
                id: EntryId::path_hash(SourceId(0), &path),
                path,
                is_dir: false,
                meta: Meta {
                    mtime: NOW - i,
                    // One a block, in the last quarter of the segment. A block
                    // holds thirty-two rows — see `columns::BLOCK`, which this
                    // deliberately depends on.
                    size: if i >= 3_200 && i % 32 == 0 { 8_192 } else { 0 },
                    ..Meta::UNKNOWN
                },
            }
        })
        .collect();
    let f = Fixture {
        bytes: build(&entries),
        entries,
    };
    assert_eq!(
        f.search("", SortKey::Size, true, 40),
        f.expected("", SortKey::Size, true, 40),
        "the tie at the edge of the page came from the wrong rows"
    );
    // The mirror, where zero is what every block can reach and the twenty-five
    // large rows are the ones that must not be.
    assert_eq!(
        f.search("", SortKey::Size, false, 40),
        f.expected("", SortKey::Size, false, 40)
    );
}

/// The count is not what stops the selection, and a deep page proves it.
///
/// At an offset the page is twenty thousand rows wide, so the boundary that
/// ends the walk is the twenty-thousandth best rather than the fortieth — and
/// a bound taken from the wrong end of the selection would still look right at
/// offset zero.
#[test]
fn a_deep_page_of_a_numeric_order_is_the_page_it_would_have_been() {
    let f = Fixture::new(20_000);
    let seg = f.segment();
    let plan = Plan::compile(&parse_at("ext:rs", NOW), &seg).expect("compile");
    let page = |sort: SortKey, desc: bool, offset: usize| {
        run(
            &seg,
            &plan,
            Wanted {
                sort,
                descending: desc,
                offset,
                limit: 20,
                count_cap: 100,
                rank_only: false,
            },
        )
        .hits
        .into_iter()
        .map(|h| h.path)
        .collect::<Vec<_>>()
    };
    for sort in [SortKey::Size, SortKey::Created, SortKey::Kind] {
        for desc in [true, false] {
            let whole = f.expected("ext:rs", sort, desc, 1_000);
            for offset in [0, 19, 200, 500] {
                if offset + 20 > whole.len() {
                    continue;
                }
                assert_eq!(
                    page(sort, desc, offset),
                    whole[offset..offset + 20],
                    "{sort:?} (desc={desc}) at {offset}"
                );
            }
        }
    }
}

#[test]
fn a_capped_count_is_a_floor_and_says_so() {
    let f = Fixture::new(20_000);
    let seg = f.segment();
    let plan = Plan::compile(&parse_at("", NOW), &seg).expect("compile");
    let capped = run(
        &seg,
        &plan,
        Wanted {
            sort: SortKey::Modified,
            descending: true,
            offset: 0,
            limit: 10,
            count_cap: 100,
            rank_only: false,
        },
    );
    assert_eq!(capped.total, 100);
    assert!(capped.capped);

    let full = run(
        &seg,
        &plan,
        Wanted {
            sort: SortKey::Modified,
            descending: true,
            offset: 0,
            limit: 10,
            count_cap: 10_000_000,
            rank_only: false,
        },
    );
    assert!(!full.capped);
    assert_eq!(full.total, f.entries.len() as u64);
}

#[test]
fn paging_reconstructs_the_head_of_the_list() {
    let f = Fixture::new(5_000);
    let seg = f.segment();
    let plan = Plan::compile(&parse_at("ext:rs", NOW), &seg).expect("compile");
    let page = |offset: usize| {
        run(
            &seg,
            &plan,
            Wanted {
                sort: SortKey::Name,
                descending: false,
                offset,
                limit: 20,
                count_cap: 10_000_000,
                rank_only: false,
            },
        )
        .hits
        .into_iter()
        .map(|h| h.path)
        .collect::<Vec<_>>()
    };
    let mut got = page(0);
    got.extend(page(20));
    got.extend(page(40));
    assert_eq!(got, f.expected("ext:rs", SortKey::Name, false, 60));
}

#[test]
fn a_dead_row_disappears_without_the_files_being_rewritten() {
    // Removal is one bit. Nothing else in the segment changes.
    let f = Fixture::new(2_000);
    let mut alive = f.bytes.alive.clone();
    let victim = {
        let seg = f.segment();
        seg.entry(7).expect("row 7").path
    };
    alive[7 / 8] &= !(1 << 7);

    let seg = Segment {
        names: NameArena::open(&f.bytes.names).expect("names"),
        folded: NameArena::open(&f.bytes.fnames).expect("fnames"),
        cols: ColumnBlocks::open(&f.bytes.cols).expect("cols"),
        dirs: DirTable::open(&f.bytes.dirs).expect("dirs"),
        tri: TrigramIndex::open(&f.bytes.tri_dict, &f.bytes.tri_post).expect("tri"),
        porder: PathOrder::open(&f.bytes.porder),
        norder: NameOrder::open(&f.bytes.norder),
        eorder: ExtensionOrder::open(&f.bytes.eorder),
        alive: &alive,
    };
    let plan = Plan::compile(&parse_at("", NOW), &seg).expect("compile");
    let found = run(
        &seg,
        &plan,
        Wanted {
            sort: SortKey::Modified,
            descending: true,
            offset: 0,
            limit: 100,
            count_cap: 10_000_000,
            rank_only: false,
        },
    );
    assert!(
        !found.hits.iter().any(|h| h.path == victim),
        "{victim} was removed"
    );
    assert_eq!(found.total, f.entries.len() as u64 - 1);
}

#[test]
fn a_query_the_index_cannot_answer_is_refused_rather_than_guessed_at() {
    let f = Fixture::new(200);
    let seg = f.segment();
    let err = Plan::compile(&parse_at("content:gizli", NOW), &seg).unwrap_err();
    assert_eq!(err.code(), "content_not_indexed");
}

#[test]
fn a_scope_naming_nothing_matches_nothing_rather_than_everything() {
    // The failure that would be worst: an unknown folder compiling to a
    // condition that is trivially true.
    let f = Fixture::new(2_000);
    assert!(
        f.search("under:/nowhere/at/all", SortKey::Modified, true, 50)
            .is_empty()
    );
    assert!(
        f.search("parent:/nowhere", SortKey::Modified, true, 50)
            .is_empty()
    );
}

#[test]
fn short_terms_are_answered_rather_than_refused() {
    // A trigram index cannot answer one or two characters and has to say so.
    // A scan has no such limit, and this is one of the things it buys.
    let f = Fixture::new(5_000);
    for q in ["a", "rs", "x"] {
        f.check(q, SortKey::Modified, true);
    }
}

#[test]
fn a_selective_term_stops_reading_the_corpus() {
    // The reason the trigram filter exists, and the assertion that it is
    // actually being taken: a name that occurs once must not cost a walk of
    // fifty thousand rows.
    let f = Fixture::new(50_000);
    let mut times: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for e in &f.entries {
        *times.entry(e.name()).or_default() += 1;
    }
    let needle = f
        .entries
        .iter()
        .map(|e| e.name())
        .find(|n| times[n] == 1 && n.len() >= 6 && n.is_ascii())
        .expect("some name occurs exactly once");

    let seg = f.segment();
    let plan = Plan::compile(&parse_at(needle, NOW), &seg).expect("compile");
    let found = run(
        &seg,
        &plan,
        Wanted {
            sort: SortKey::Modified,
            descending: true,
            offset: 0,
            limit: 40,
            count_cap: 10_000_000,
            rank_only: false,
        },
    );
    assert!(
        found.rows_visited * 10 < seg.rows() as u64,
        "visited {} of {} rows for {needle:?}",
        found.rows_visited,
        seg.rows()
    );
    assert_eq!(
        found.hits.into_iter().map(|h| h.path).collect::<Vec<_>>(),
        f.expected(needle, SortKey::Modified, true, 40),
        "narrowing changed the answer"
    );
}

#[test]
fn narrowing_never_loses_a_match() {
    // Every substring of every name, checked against the walk that cannot take
    // a shortcut. A false positive here is microseconds; a false negative is a
    // file the user cannot find, and nothing would report it.
    let f = Fixture::new(20_000);
    let mut tried = 0;
    for e in f.entries.iter().step_by(97) {
        let name = e.name();
        if name.len() < 5 || !name.is_ascii() {
            continue;
        }
        for q in [&name[..4], &name[1..5], &name[name.len() - 4..]] {
            assert_eq!(
                f.search(q, SortKey::Modified, true, 50),
                f.expected(q, SortKey::Modified, true, 50),
                "query {q:?} taken from {name:?}"
            );
            tried += 1;
        }
    }
    assert!(tried > 100, "only {tried} substrings were tried");
}

#[test]
fn an_extension_and_a_glob_narrow_the_same_way_a_substring_does() {
    // `*.pdf` and `ext:pdf` are the same question as "contains .pdf", and a
    // rare extension is as selective as a rare name. Both still have to agree
    // with the walk that cannot take a shortcut.
    let f = Fixture::new(50_000);
    let seg = f.segment();
    for q in ["ext:pdf", "*.pdf", "rap*or", "ext:rs;toml", "ext:rs"] {
        let plan = Plan::compile(&parse_at(q, NOW), &seg).expect("compile");
        let found = run(
            &seg,
            &plan,
            Wanted {
                sort: SortKey::Modified,
                descending: true,
                offset: 0,
                limit: 40,
                count_cap: 10_000_000,
                rank_only: false,
            },
        );
        assert_eq!(
            found.hits.into_iter().map(|h| h.path).collect::<Vec<_>>(),
            f.expected(q, SortKey::Modified, true, 40),
            "narrowing changed the answer for {q:?}"
        );
    }
}

#[test]
fn a_numeric_filter_skips_blocks_it_cannot_satisfy() {
    // The zone map. A block whose sizes are all under a megabyte cannot hold a
    // file over one, and rejecting it costs two comparisons against numbers
    // that were already in the file.
    let f = Fixture::new(50_000);
    let seg = f.segment();
    for q in ["size:>1mb", "kind:image", "folder:", "dc:>2024-01-01"] {
        let plan = Plan::compile(&parse_at(q, NOW), &seg).expect("compile");
        let found = run(
            &seg,
            &plan,
            Wanted {
                sort: SortKey::Modified,
                descending: true,
                offset: 0,
                limit: 40,
                count_cap: 10_000_000,
                rank_only: false,
            },
        );
        assert_eq!(
            found.hits.into_iter().map(|h| h.path).collect::<Vec<_>>(),
            f.expected(q, SortKey::Modified, true, 40),
            "skipping changed the answer for {q:?}"
        );
    }

    // And it has to actually skip, or it is only a slower way to be correct.
    // Calibrated from the fixture rather than guessed: no file is larger than
    // the largest file, so every block can be rejected on its maximum alone.
    let biggest = f
        .entries
        .iter()
        .map(|e| e.meta.size)
        .max()
        .expect("entries");
    let plan = Plan::compile(&parse_at(&format!("size:>{biggest}"), NOW), &seg).expect("compile");
    let found = run(
        &seg,
        &plan,
        Wanted {
            sort: SortKey::Modified,
            descending: true,
            offset: 0,
            limit: 40,
            count_cap: 10_000_000,
            rank_only: false,
        },
    );
    assert_eq!(
        found.rows_visited, 0,
        "every block should have been rejected"
    );
    assert_eq!(found.total, 0);

    // A block of files cannot hold a directory, which is the same test on a
    // column that only ever holds nought or one.
    let plan = Plan::compile(&parse_at("folder:", NOW), &seg).expect("compile");
    let found = run(
        &seg,
        &plan,
        Wanted {
            sort: SortKey::Modified,
            descending: true,
            offset: 0,
            limit: 40,
            count_cap: 10_000_000,
            rank_only: false,
        },
    );
    assert_eq!(found.total, f.expected_count("folder:"));
}

#[test]
fn ties_come_back_newest_first() {
    // The tie-break, pinned on its own rather than only against the reference:
    // both were changed together, and a test that compares them proves they
    // agree, not that either is right.
    //
    // Sorting by kind puts every code file at the same value. Which forty of
    // them appear is decided by the stored order — newest first — because that
    // is both the useful answer and the one that costs nothing.
    let f = Fixture::new(20_000);
    let seg = f.segment();
    let plan = Plan::compile(&parse_at("kind:code", NOW), &seg).expect("compile");
    let found = run(
        &seg,
        &plan,
        Wanted {
            sort: SortKey::Kind,
            descending: false,
            offset: 0,
            limit: 40,
            count_cap: 10_000_000,
            rank_only: false,
        },
    );
    let times: Vec<i64> = found.hits.iter().map(|h| h.meta.mtime).collect();
    assert!(
        times.len() == 40,
        "expected a full page, got {}",
        times.len()
    );
    assert!(
        times.windows(2).all(|w| w[0] >= w[1]),
        "within one kind the page should be newest first: {times:?}"
    );
    assert_eq!(
        found
            .hits
            .iter()
            .map(|h| h.path.clone())
            .collect::<Vec<_>>(),
        f.expected("kind:code", SortKey::Kind, false, 40)
    );
}

#[test]
fn an_abbreviated_name_key_still_orders_by_the_whole_name() {
    // Names are selected on their first eight bytes. Rows that share those
    // eight still have to be compared properly, or a page of files whose names
    // begin alike comes back in the wrong order.
    let tmp: Vec<Entry> = (0..300)
        .map(|i| Entry {
            id: scour_core::EntryId::inode(scour_core::SourceId(0), 1, i),
            path: format!("/a/samepref{:04}.rs", 299 - i),
            is_dir: false,
            meta: scour_core::Meta {
                mtime: 1000 + i as i64,
                size: 1,
                ..scour_core::Meta::UNKNOWN
            },
        })
        .collect();
    let bytes = build(&tmp);
    let seg = Segment {
        names: NameArena::open(&bytes.names).expect("names"),
        folded: NameArena::open(&bytes.fnames).expect("fnames"),
        cols: ColumnBlocks::open(&bytes.cols).expect("cols"),
        dirs: DirTable::open(&bytes.dirs).expect("dirs"),
        tri: TrigramIndex::open(&bytes.tri_dict, &bytes.tri_post).expect("tri"),
        porder: PathOrder::open(&bytes.porder),
        norder: NameOrder::open(&bytes.norder),
        eorder: ExtensionOrder::open(&bytes.eorder),
        alive: &bytes.alive,
    };
    let plan = Plan::compile(&parse_at("", NOW), &seg).expect("compile");
    let got: Vec<String> = run(
        &seg,
        &plan,
        Wanted {
            sort: SortKey::Name,
            descending: false,
            offset: 0,
            limit: 10,
            count_cap: 10_000_000,
            rank_only: false,
        },
    )
    .hits
    .into_iter()
    .map(|h| h.path)
    .collect();
    let want: Vec<String> = (0..10).map(|i| format!("/a/samepref{i:04}.rs")).collect();
    assert_eq!(got, want);
}

#[test]
fn a_query_with_no_name_test_still_sorts_by_name_correctly() {
    // The row-driven walk skips the name arena, which is right until the sort
    // key is the name. It was not asked, and the result was forty rows chosen
    // out of a corpus where every sort key had come back identical — correct by
    // accident, and at the cost of building every row.
    let f = Fixture::new(20_000);
    for q in ["", "size:>1kb", "kind:code"] {
        for key in [SortKey::Name, SortKey::Ext, SortKey::Path] {
            assert_eq!(
                f.search(q, key, false, 40),
                f.expected(q, key, false, 40),
                "query {q:?} sorted by {key:?}"
            );
        }
    }

    // And it costs a page, not a corpus.
    let seg = f.segment();
    let plan = Plan::compile(&parse_at("", NOW), &seg).expect("compile");
    let found = run(
        &seg,
        &plan,
        Wanted {
            sort: SortKey::Kind,
            descending: true,
            offset: 0,
            limit: 40,
            count_cap: 10_000_000,
            rank_only: false,
        },
    );
    assert!(
        found.rows_built < 200,
        "built {} rows for a page of forty",
        found.rows_built
    );
}
