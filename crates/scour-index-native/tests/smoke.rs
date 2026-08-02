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

use scour_core::{Entry, SortKey};
use scour_index_native::{
    ColumnBlocks, DirTable, NameArena, Plan, Segment, SegmentBytes, Wanted, build, run,
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
            cols: ColumnBlocks::open(&self.bytes.cols).expect("cols"),
            dirs: DirTable::open(&self.bytes.dirs).expect("dirs"),
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
        },
    );
    assert!(!all.early_exit);
    assert_eq!(all.rows_visited as usize, seg.rows());
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
        cols: ColumnBlocks::open(&f.bytes.cols).expect("cols"),
        dirs: DirTable::open(&f.bytes.dirs).expect("dirs"),
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
