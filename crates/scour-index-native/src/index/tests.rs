use super::fold::{Member, head_of, size_tiers};
use super::*;
use crate::build::build;
use crate::segment::Live;
use scour_core::{Change, Entry, Hit, Index, Maintenance, SearchRequest, SourceId};
use std::path::{Path, PathBuf};

fn row(source: u32, path: &str, size: i64) -> Entry {
    Entry {
        id: scour_core::EntryId::path_hash(SourceId(source), path),
        path: path.to_owned(),
        is_dir: false,
        meta: scour_core::Meta {
            mtime: size,
            size,
            ..scour_core::Meta::UNKNOWN
        },
    }
}

fn commit_rows(index: &NativeIndex, rows: impl IntoIterator<Item = Entry>) {
    index
        .apply(&mut rows.into_iter().map(Change::Upsert))
        .expect("apply rows");
    index.commit().expect("commit rows");
}

fn hits(index: &NativeIndex) -> Vec<Hit> {
    index
        .search(&SearchRequest {
            page: scour_core::Page::new(0, 100),
            ..SearchRequest::default()
        })
        .expect("search")
        .hits
}

fn publish_rows(index: &NativeIndex, rows: impl IntoIterator<Item = Entry>) {
    index
        .apply(&mut rows.into_iter().map(Change::Upsert))
        .expect("apply rows");
    index.publish().expect("publish rows");
}

fn paths(index: &NativeIndex) -> Vec<String> {
    let mut out: Vec<String> = hits(index).into_iter().map(|h| h.path).collect();
    out.sort();
    out
}

fn segment_files(dir: &Path) -> Vec<String> {
    let mut out: Vec<String> = std::fs::read_dir(dir)
        .expect("read dir")
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.starts_with("seg-"))
        .collect();
    out.sort();
    out
}

/// A publish is searchable at once and writes nothing: no segment file, no
/// manifest line. A commit writes it down, and a reopen finds it.
#[test]
fn a_published_row_is_found_at_once_and_written_by_the_next_commit() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    {
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        commit_rows(&index, [row(0, "/w/old.txt", 1)]);
        let files = segment_files(tmp.path());
        let manifest = std::fs::read(tmp.path().join(META_FILE)).expect("manifest");

        publish_rows(&index, [row(0, "/w/new.txt", 2)]);
        assert_eq!(paths(&index), ["/w/new.txt", "/w/old.txt"]);
        assert_eq!(segment_files(tmp.path()), files, "a publish wrote a file");
        assert_eq!(
            std::fs::read(tmp.path().join(META_FILE)).expect("manifest"),
            manifest,
            "a publish rewrote the manifest"
        );

        index.commit().expect("commit");
        assert!(index.inner.read().unwritten.is_empty());
    }
    let reopened = NativeIndex::open_or_create(tmp.path()).expect("reopen");
    assert_eq!(paths(&reopened), ["/w/new.txt", "/w/old.txt"]);
}

/// What a crash does to a publish: the index opens, what was committed is
/// there, and what was only published is not — the next walk's to find.
#[test]
fn an_index_closed_without_a_commit_opens_with_what_was_committed() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    {
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        commit_rows(&index, [row(0, "/w/kept.txt", 1), row(0, "/w/gone.txt", 2)]);
        publish_rows(&index, [row(0, "/w/published.txt", 3)]);
        index
            .apply(&mut std::iter::once(Change::RemoveSubtree {
                path: "/w/gone.txt".into(),
            }))
            .expect("remove");
        index.publish().expect("publish removal");
        assert_eq!(paths(&index), ["/w/kept.txt", "/w/published.txt"]);
        // No commit: dropped as a killed process leaves it.
    }
    let reopened = NativeIndex::open_or_create(tmp.path()).expect("reopen");
    assert_eq!(paths(&reopened), ["/w/gone.txt", "/w/kept.txt"]);
}

/// A removal made by a publish is written by the commit after it.
#[test]
fn a_published_removal_is_written_by_the_next_commit() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    {
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        commit_rows(&index, [row(0, "/w/kept.txt", 1), row(0, "/w/gone.txt", 2)]);
        index
            .apply(&mut std::iter::once(Change::RemoveSubtree {
                path: "/w/gone.txt".into(),
            }))
            .expect("remove");
        index.publish().expect("publish removal");
        assert_eq!(paths(&index), ["/w/kept.txt"]);
        index.commit().expect("commit");
    }
    let reopened = NativeIndex::open_or_create(tmp.path()).expect("reopen");
    assert_eq!(paths(&reopened), ["/w/kept.txt"]);
}

/// Published a second apart, merged in memory by a compaction, written as
/// one segment by the commit: many publishes cost one set of files.
#[test]
fn publishes_merge_in_memory_and_are_written_as_one_segment() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    {
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        commit_rows(&index, [row(0, "/w/base.txt", 1)]);
        let files = segment_files(tmp.path());
        for i in 0..12 {
            publish_rows(&index, [row(0, &format!("/w/p{i:02}.txt"), 10 + i)]);
        }
        // Replaced while in memory: one row for the path, the newer one. The
        // segment the old row was alone in is empty now, and forgotten.
        publish_rows(&index, [row(0, "/w/p03.txt", 99)]);
        assert_eq!(index.inner.read().unwritten.len(), 12);
        index.maintain(Maintenance::Compact).expect("compact");
        assert_eq!(index.inner.read().unwritten.len(), 1, "merged in memory");
        assert_eq!(
            segment_files(tmp.path()),
            files,
            "a merge in memory wrote a file"
        );
        assert_eq!(paths(&index).len(), 13);

        index.commit().expect("commit");
        assert!(index.inner.read().unwritten.is_empty());
        assert_eq!(index.inner.read().segments.len(), 2);
    }
    let reopened = NativeIndex::open_or_create(tmp.path()).expect("reopen");
    let found = paths(&reopened);
    assert_eq!(found.len(), 13);
    let p03 = hits(&reopened)
        .into_iter()
        .find(|h| h.path == "/w/p03.txt")
        .expect("p03");
    assert_eq!(p03.meta.size, 99);
}

/// While a walk is open its marks pin segment numbers, so a fold is refused
/// and each published segment is written under its own number — and the
/// sweep that ends the walk still reads the marks right.
#[test]
fn a_persist_during_a_walk_writes_each_segment_under_its_number() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    commit_rows(&index, [row(0, "/w/a.txt", 1), row(0, "/w/b.txt", 2)]);
    let generation = index.begin_generation().expect("generation");
    // The walk sees a.txt unchanged, a new c.txt and d.txt, and no b.txt.
    publish_rows(&index, [row(0, "/w/a.txt", 1), row(0, "/w/c.txt", 3)]);
    publish_rows(&index, [row(0, "/w/d.txt", 4)]);
    index.commit().expect("commit during the walk");
    assert!(index.inner.read().unwritten.is_empty());
    index
        .sweep(
            SourceId(0),
            &["/w".to_string()],
            generation,
            &scour_core::PrefixSet::default(),
        )
        .expect("sweep");
    assert_eq!(paths(&index), ["/w/a.txt", "/w/c.txt", "/w/d.txt"]);
    index.commit().expect("commit");
    drop(index);
    let reopened = NativeIndex::open_or_create(tmp.path()).expect("reopen");
    assert_eq!(paths(&reopened), ["/w/a.txt", "/w/c.txt", "/w/d.txt"]);
}

/// What a pass had to write: the new row and the changed one, not the row it
/// found as it was — however the rows were published or committed on the way.
#[test]
fn a_pass_counts_what_it_wrote_and_not_what_it_found_unchanged() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    commit_rows(&index, [row(0, "/w/same.txt", 1), row(0, "/w/grew.txt", 2)]);
    let generation = index.begin_generation().expect("generation");
    publish_rows(
        &index,
        [row(0, "/w/same.txt", 1), row(0, "/w/grew.txt", 20)],
    );
    index.commit().expect("commit during the walk");
    publish_rows(&index, [row(0, "/w/new.txt", 3)]);
    index
        .sweep(
            SourceId(0),
            &["/w".to_string()],
            generation,
            &scour_core::PrefixSet::default(),
        )
        .expect("sweep");
    assert_eq!(index.fresh(generation), 2);
    assert_eq!(index.fresh(generation + 1), 0);
}

#[test]
fn traced_rows_are_counted_by_the_folder_six_levels_down() {
    let paths: Vec<String> = [
        "/home/u/.config/app/Cache/a/b/c/one",
        "/home/u/.config/app/Cache/a/b/two",
        "/home/u/.config/app/Cache/a/three",
        "/home/u/notes.txt",
    ]
    .map(String::from)
    .to_vec();
    assert_eq!(
        by_folder(&paths),
        [
            ("/home/u/.config/app/Cache/a".to_owned(), 3),
            ("/home/u".to_owned(), 1)
        ]
    );
}

/// Two persists at once — a client's flush and the service's own — each
/// wrote the same published segment: one truncated a file the other was
/// opening, and the flush answered that the index was corrupt.
#[test]
fn two_persists_at_once_write_a_published_segment_once() {
    for round in 0..4 {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let index = std::sync::Arc::new(NativeIndex::open_or_create(tmp.path()).expect("create"));
        let rows: Vec<Entry> = (0..20_000)
            .map(|i| row(0, &format!("/w/d{}/f{i}.txt", i % 97), i as i64))
            .collect();
        publish_rows(&index, rows);
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let persists: Vec<_> = (0..2)
            .map(|_| {
                let (index, start) = (index.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    index.persist_unwritten()
                })
            })
            .collect();
        for p in persists {
            p.join()
                .expect("thread")
                .unwrap_or_else(|e| panic!("round {round}: {e}"));
        }
        assert_eq!(
            index
                .unwritten_writes
                .load(std::sync::atomic::Ordering::Relaxed),
            1,
            "round {round}: the segment was written by both"
        );
        drop(index);
        let reopened = NativeIndex::open_or_create(tmp.path()).expect("reopen");
        assert_eq!(
            reopened.stats().expect("stats").entries,
            20_000,
            "round {round}"
        );
    }
}

/// Every file of the index directory with its bytes, the lock aside.
fn disk_state(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out: Vec<(String, Vec<u8>)> = std::fs::read_dir(dir)
        .expect("read dir")
        .filter_map(|e| {
            let e = e.ok()?;
            let name = e.file_name().into_string().ok()?;
            (name != "index.lock").then(|| (name, std::fs::read(e.path()).unwrap_or_default()))
        })
        .collect();
    out.sort();
    out
}

/// A subtree walk from beginning to sweep writes nothing: each of its
/// syncs was taken with the lock held, seconds apiece on a busy disk. The
/// removal is searched at once, lost to a crash, and kept by a commit.
#[test]
fn a_walk_and_its_sweep_write_nothing_until_the_commit() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let walk = |index: &NativeIndex| {
        let generation = index.begin_generation().expect("generation");
        publish_rows(index, [row(0, "/w/a.txt", 1), row(0, "/w/c.txt", 3)]);
        index
            .sweep(
                SourceId(0),
                &["/w".to_string()],
                generation,
                &scour_core::PrefixSet::default(),
            )
            .expect("sweep");
        assert_eq!(paths(index), ["/w/a.txt", "/w/c.txt"]);
    };
    {
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        commit_rows(&index, [row(0, "/w/a.txt", 1), row(0, "/w/b.txt", 2)]);
        let before = disk_state(tmp.path());
        walk(&index);
        assert!(
            disk_state(tmp.path()) == before,
            "the walk wrote to the index directory"
        );
        // No commit: dropped as a killed process leaves it.
    }
    let index = NativeIndex::open_or_create(tmp.path()).expect("reopen");
    assert_eq!(paths(&index), ["/w/a.txt", "/w/b.txt"]);
    walk(&index);
    index.commit().expect("commit");
    drop(index);
    let reopened = NativeIndex::open_or_create(tmp.path()).expect("reopen");
    assert_eq!(paths(&reopened), ["/w/a.txt", "/w/c.txt"]);
}

fn block_existing_file(path: &Path) -> PathBuf {
    let saved = path.with_extension("test-saved");
    std::fs::rename(path, &saved).expect("move file aside");
    std::fs::create_dir(path).expect("put directory in the file's place");
    saved
}

fn restore_blocked_file(path: &Path, saved: &Path) {
    std::fs::remove_dir(path).expect("remove blocker");
    std::fs::rename(saved, path).expect("restore file");
}

fn two_segment_index(dir: &Path) -> std::sync::Arc<NativeIndex> {
    let index = std::sync::Arc::new(NativeIndex::open_or_create(dir).expect("create"));
    commit_rows(&index, [row(0, "/w/victim.txt", 1)]);
    commit_rows(&index, [row(0, "/w/keeper.txt", 2)]);
    assert_eq!(index.inner.read().segments.len(), 2);
    index
}

#[test]
fn closing_a_scan_starts_a_new_compaction_cohort() {
    let mut inner = Inner {
        generation: 7,
        open: Some(7),
        ..Inner::default()
    };
    close_generation(&mut inner, 7);
    assert_eq!(inner.open, None);
    assert_eq!(inner.generation, 8);
    assert!(inner.meta_dirty);

    // A source with several vouched roots sweeps once per root. Only the
    // first closes the scan; the remaining roots must not keep advancing.
    close_generation(&mut inner, 7);
    assert_eq!(inner.generation, 8);
}

fn member(number: u64, rows: u64) -> Member {
    Member {
        number,
        rows,
        dead: 0,
    }
}

/// The cut, at the sizes the live index actually holds: a 4.6 M-row body, a
/// 454,184-row tail, and watcher commits of a few rows.
#[test]
fn size_tiers_cut_where_a_member_outweighs_everything_below_it() {
    // Nothing below the tail comes to a quarter of it, so it stays where it
    // is and the trickle folds on its own.
    assert_eq!(
        size_tiers([(1, 4_600_000), (2, 454_184), (3, 40), (4, 40), (5, 40)]),
        vec![vec![3, 4, 5], vec![2], vec![1]]
    );
    // Once the trickle has grown to a quarter of the tail, the tail takes it
    // in — worth ~113k rows of churn rather than sixty seconds.
    assert_eq!(
        size_tiers([(1, 4_600_000), (2, 454_184), (3, 113_546)]),
        vec![vec![3, 2], vec![1]]
    );
    // Equal sizes are one tier: a fixture of ten 800-row segments has no
    // body to spare, and folding them together is the whole job.
    assert_eq!(
        size_tiers((1..=4).map(|n| (n, 800))),
        vec![vec![1, 2, 3, 4]]
    );
    // Order in, and ties, must not decide the answer.
    assert_eq!(
        size_tiers([(9, 40), (1, 4_600_000), (5, 40)]),
        size_tiers([(1, 4_600_000), (5, 40), (9, 40)])
    );
    assert_eq!(size_tiers([]), Vec::<Vec<u64>>::new());
}

/// The head is the trickle, and the tail is not in it.
#[test]
fn a_trickle_is_folded_without_the_tail() {
    let live = [
        member(1, 4_600_000),
        member(2, 454_184),
        member(3, 40),
        member(4, 40),
        member(5, 40),
    ];
    assert_eq!(head_of(&live), Some(vec![3, 4, 5]));

    // With the trickle already folded there is nothing left worth a
    // rewrite: three segments, and none of them touched.
    assert_eq!(
        head_of(&[member(1, 4_600_000), member(2, 454_184), member(6, 120)]),
        None
    );

    // The old rule is kept where it earns its keep: a largest segment a
    // quarter of which is dead is rewritten, and everything rides along.
    let dead = [
        Member {
            number: 1,
            rows: 4_600_000,
            dead: 2_000_000,
        },
        member(2, 454_184),
        member(3, 40),
    ];
    assert_eq!(head_of(&dead), Some(vec![1, 2, 3]));

    // And a middle segment alone in its tier can still be shrunk, or
    // deleting a large directory would leave its rows on disk until the
    // next rebuild. `fold` accepts a group of one on exactly this test.
    let hollow = [
        member(1, 4_600_000),
        Member {
            number: 2,
            rows: 454_184,
            dead: 300_000,
        },
        member(3, 40),
    ];
    assert_eq!(head_of(&hollow), Some(vec![2]));
}

/// The segment count stays bounded under a trickle that never stops — the rule
/// this replaces left 241 segments across 205 generations with nothing
/// foldable. Driven through `head_of` because what matters is the count after
/// thousands of rounds at live sizes.
#[test]
fn a_long_trickle_stays_under_the_geometric_bound() {
    // Five hundred rounds of sixty forty-row commits on a 4.6 M-row body,
    // through both rules: this file's, and everything but the largest.
    let trickle = |pick: &dyn Fn(&[Member]) -> Option<Vec<u64>>| {
        let mut segments = vec![member(0, 4_600_000)];
        let mut next = 1u64;
        let (mut worst, mut folds, mut rewritten) = (0usize, 0usize, 0u64);
        for _ in 0..500 {
            for _ in 0..60 {
                segments.push(member(next, 40));
                next += 1;
            }
            // `maintain(Compact)`: fold what `next_head` hands back until it
            // hands back nothing. `groups` offers three or more, or nothing.
            while segments.len() >= 3 {
                let Some(head) = pick(&segments) else { break };
                if head.len() < 2 {
                    break;
                }
                let rows: u64 = segments
                    .iter()
                    .filter(|m| head.contains(&m.number))
                    .map(|m| m.rows)
                    .sum();
                segments.retain(|m| !head.contains(&m.number));
                segments.push(member(next, rows));
                next += 1;
                folds += 1;
                rewritten += rows;
            }
            worst = worst.max(segments.len());
        }
        let total: u64 = segments.iter().map(|m| m.rows).sum();
        (worst, folds, rewritten, total)
    };

    let (worst, folds, rewritten, total) = trickle(&|m| head_of(m));
    // `ceil(log4(total)) + 1`: each tier below the largest holds one segment
    // more than four times the one under it, from a single row up.
    let bound = (64 - total.leading_zeros()).div_ceil(2) as usize + 1;
    assert_eq!(bound, 13, "1.2 M rows of trickle on a 4.6 M-row body");
    // Measured at five when this was written. The assertion is the bound,
    // because that is the promise; the five is how much room it has.
    assert!(
        worst <= bound,
        "{worst} segments at worst, against a bound of {bound}"
    );

    let (_, was_folds, was_rewritten, _) = trickle(&|members: &[Member]| {
        let biggest = members.iter().max_by_key(|m| m.rows)?;
        Some(
            members
                .iter()
                .map(|m| m.number)
                .filter(|&n| n != biggest.number)
                .collect(),
        )
    });
    // And this is the whole of it: the same trickle, the same number of
    // compactions, and the tail dragged through every one of them.
    assert_eq!(folds, was_folds, "the same rounds either way");
    assert!(
        rewritten * 20 < was_rewritten,
        "{rewritten} rows rewritten over {folds} folds against the old rule's \
             {was_rewritten}, on an index holding {total}"
    );
}

/// Folding by tiers answers exactly what folding the whole rest answered.
/// Two indexes, the same rows and removals, compacted both ways: how the rows
/// are divided into segments is not something a query can see.
#[test]
fn a_tiered_fold_answers_what_folding_the_whole_rest_did() {
    let tiered = tempfile::tempdir().expect("tmpdir");
    let whole = tempfile::tempdir().expect("tmpdir");
    let a = NativeIndex::open_or_create(tiered.path()).expect("create");
    let b = NativeIndex::open_or_create(whole.path()).expect("create");
    for index in [&a, &b] {
        // A body, a tail, and a trickle: the live shape, scaled down.
        commit_rows(
            index,
            (0..2_000).map(|i| row(0, &format!("/body/rapor-{i}.txt"), i % 97)),
        );
        commit_rows(
            index,
            (0..500).map(|i| row(0, &format!("/tail/rapor-{i}.md"), i % 89)),
        );
        for round in 0..9 {
            commit_rows(
                index,
                (0..3).map(|i| row(0, &format!("/w/{round}-{i}.txt"), round * 7 + i)),
            );
        }
        // Deaths, so the live bitmaps and the row counts disagree.
        index
            .apply(&mut (0..40).map(|i| Change::RemoveSubtree {
                path: format!("/body/rapor-{}.txt", i * 7),
            }))
            .expect("apply removals");
        index.commit().expect("commit removals");
    }

    let fingerprint = |index: &NativeIndex| -> Vec<String> {
        let mut out = Vec::new();
        for sort in [
            scour_core::SortKey::Modified,
            scour_core::SortKey::Name,
            scour_core::SortKey::Path,
            scour_core::SortKey::Ext,
            scour_core::SortKey::Size,
        ] {
            for descending in [true, false] {
                let answer = index
                    .search(&SearchRequest {
                        query: scour_query::parse("rapor"),
                        sort,
                        descending,
                        page: scour_core::Page::new(0, 4_000),
                    })
                    .expect("search");
                out.push(format!("{sort:?} {descending} {}", answer.total));
                out.extend(
                    answer
                        .hits
                        .iter()
                        .map(|h| format!("{} {} {}", h.path, h.meta.size, h.meta.mtime)),
                );
            }
        }
        out
    };

    let before = fingerprint(&a);
    assert_eq!(fingerprint(&b), before, "the two fixtures start equal");

    a.maintain(Maintenance::Compact).expect("compact");
    // The rule this replaces, by hand: one group, everything but the
    // largest member, until folding stops changing anything.
    loop {
        let head = {
            let inner = b.inner.read();
            if inner.segments.len() < 3 {
                break;
            }
            let biggest = inner
                .segments
                .iter()
                .max_by_key(|s| s.rows())
                .expect("a biggest")
                .number;
            inner
                .segments
                .iter()
                .map(|s| s.number)
                .filter(|&n| n != biggest)
                .collect::<Vec<u64>>()
        };
        if !b.fold(&head).expect("fold") {
            break;
        }
    }

    assert_eq!(fingerprint(&a), before, "tiered folding changed an answer");
    assert_eq!(fingerprint(&b), before, "the old rule changed an answer");
    assert_eq!(
        a.stats().expect("stats").entries,
        b.stats().expect("stats").entries
    );
    // Which is the whole trade: the same answers out of one more segment,
    // for a fold that did not touch the body or the tail.
    assert!(
        a.inner.read().segments.len() <= 3,
        "{} segments after a tiered compaction",
        a.inner.read().segments.len()
    );
}

/// A trickle does not drag the tail through a fold with it. The tail's
/// 11.70 MB of names reappeared under a new segment number nine times in eight
/// and a half minutes, 1.5–1.8 s of worker CPU and 51–68 MB each.
#[test]
fn a_trickle_of_small_segments_does_not_refold_the_tail() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    commit_rows(
        &index,
        (0..4_000).map(|i| row(0, &format!("/body/{i}.txt"), i)),
    );
    commit_rows(
        &index,
        (0..1_000).map(|i| row(0, &format!("/tail/{i}.txt"), i)),
    );
    let numbers = || -> Vec<u64> {
        index
            .inner
            .read()
            .segments
            .iter()
            .map(|s| s.number)
            .collect()
    };
    let started = numbers();
    assert_eq!(started.len(), 2, "the fixture is a body and a tail");
    let (body, tail) = (started[0], started[1]);

    for round in 0..4 {
        for i in 0..6 {
            commit_rows(&index, [row(0, &format!("/w/{round}-{i}.txt"), i)]);
        }
        index.maintain(Maintenance::Compact).expect("compact");
        let after = numbers();
        assert!(after.contains(&body), "round {round} rewrote the body");
        assert!(
            after.contains(&tail),
            "round {round} rewrote the tail: this is the fold that cost 2.5-3% of a core"
        );
        assert_eq!(
            after.len(),
            3,
            "body, tail, one folded trickle — and nothing accumulating: {after:?}"
        );
    }
    assert_eq!(index.stats().expect("stats").entries, 4_000 + 1_000 + 24);
}

#[test]
fn a_landed_segment_retries_late_removals_after_an_alive_error() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    {
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        let number = 1;
        let rows = [row(0, "/w/victim.txt", 1), row(0, "/w/keeper.txt", 2)];
        let bytes = build(&rows);
        let live = index
            .disk_bytes
            .changing(|| Live::write(tmp.path(), number, 0, &bytes))
            .expect("write landed segment");
        let alive = tmp.path().join("seg-00000001.alive");
        let saved = block_existing_file(&alive);
        {
            let mut building = index.building.0.lock();
            building.flights.push(Flight {
                number,
                gone: scour_core::PrefixSet::new(vec!["/w/victim.txt".into()]),
                ..Flight::default()
            });
            building.landed.push(Landed::Built { number, live });
        }

        assert!(index.collect(&mut index.inner.write()).is_err());
        {
            let building = index.building.0.lock();
            assert_eq!(building.flights.len(), 1);
            assert_eq!(building.landed.len(), 1);
        }

        restore_blocked_file(&alive, &saved);
        index
            .collect(&mut index.inner.write())
            .expect("retry collection");
    }

    let reopened = NativeIndex::open_or_create(tmp.path()).expect("reopen");
    let paths: Vec<_> = hits(&reopened).into_iter().map(|hit| hit.path).collect();
    assert_eq!(paths, ["/w/keeper.txt"]);
}

#[test]
fn a_landed_segment_retries_manifest_publication_without_another_landing() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    {
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        let number = 1;
        let bytes = build(&[row(0, "/w/landed.txt", 1)]);
        let live = index
            .disk_bytes
            .changing(|| Live::write(tmp.path(), number, 0, &bytes))
            .expect("write landed segment");
        {
            let mut building = index.building.0.lock();
            building.flights.push(Flight {
                number,
                ..Flight::default()
            });
            building.landed.push(Landed::Built { number, live });
        }
        let manifest = tmp.path().join(META_FILE);
        std::fs::create_dir(&manifest).expect("block first manifest");

        assert!(index.collect(&mut index.inner.write()).is_err());
        assert!(index.inner.read().meta_dirty);
        assert!(index.building.0.lock().landed.is_empty());

        std::fs::remove_dir(&manifest).expect("remove blocker");
        index.commit().expect("retry with an empty commit");
    }

    let reopened = NativeIndex::open_or_create(tmp.path()).expect("reopen");
    assert_eq!(hits(&reopened)[0].path, "/w/landed.txt");
}

#[test]
fn an_empty_commit_retries_a_failed_manifest_publication() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    {
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        commit_rows(&index, [row(0, "/w/first.txt", 1)]);

        // The exact state at the final publication point of a synchronous
        // commit: the segment is mapped and visible, the manifest is not.
        let number = index.inner.read().next_segment;
        let bytes = build(&[row(0, "/w/second.txt", 2)]);
        let live = index
            .disk_bytes
            .changing(|| Live::write(tmp.path(), number, 0, &bytes))
            .expect("write second segment");
        let manifest = tmp.path().join(META_FILE);
        let saved = block_existing_file(&manifest);
        {
            let mut inner = index.inner.write();
            inner.next_segment = number + 1;
            inner.segments.push(live);
            inner.meta_dirty = true;
            assert!(index.save_meta(&mut inner).is_err());
        }

        restore_blocked_file(&manifest, &saved);
        index.commit().expect("empty retry commit");
    }

    let reopened = NativeIndex::open_or_create(tmp.path()).expect("reopen");
    let mut paths: Vec<_> = hits(&reopened).into_iter().map(|hit| hit.path).collect();
    paths.sort();
    assert_eq!(paths, ["/w/first.txt", "/w/second.txt"]);
}

#[test]
fn fold_does_not_publish_a_row_removed_after_its_snapshot() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = two_segment_index(tmp.path());
    let gate = std::sync::Arc::new(FoldGate::new());
    index.gate_next_fold(std::sync::Arc::clone(&gate));
    let worker = std::sync::Arc::clone(&index);
    let folding = std::thread::spawn(move || worker.maintain(Maintenance::Rebuild));
    gate.snapshot_ready.wait();

    let changed = index
        .apply(&mut std::iter::once(Change::RemoveSubtree {
            path: "/w/victim.txt".into(),
        }))
        .and_then(|_| index.commit());
    gate.resume.wait();
    changed.expect("concurrent removal");
    folding.join().expect("fold thread").expect("rebuild");

    assert!(!hits(&index).iter().any(|hit| hit.path == "/w/victim.txt"));
    drop(index);
    let reopened = NativeIndex::open_or_create(tmp.path()).expect("reopen");
    assert!(
        !hits(&reopened)
            .iter()
            .any(|hit| hit.path == "/w/victim.txt")
    );
}

#[test]
fn fold_does_not_publish_an_old_row_after_a_concurrent_upsert() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = two_segment_index(tmp.path());
    let gate = std::sync::Arc::new(FoldGate::new());
    index.gate_next_fold(std::sync::Arc::clone(&gate));
    let worker = std::sync::Arc::clone(&index);
    let folding = std::thread::spawn(move || worker.maintain(Maintenance::Rebuild));
    gate.snapshot_ready.wait();

    let changed = index
        .apply(&mut std::iter::once(Change::Upsert(row(
            0,
            "/w/victim.txt",
            99,
        ))))
        .and_then(|_| index.commit());
    gate.resume.wait();
    changed.expect("concurrent upsert");
    folding.join().expect("fold thread").expect("rebuild");

    let current: Vec<_> = hits(&index)
        .into_iter()
        .filter(|hit| hit.path == "/w/victim.txt")
        .collect();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].meta.size, 99);
    drop(index);
    let reopened = NativeIndex::open_or_create(tmp.path()).expect("reopen");
    let current: Vec<_> = hits(&reopened)
        .into_iter()
        .filter(|hit| hit.path == "/w/victim.txt")
        .collect();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].meta.size, 99);
}

#[test]
fn an_empty_commit_retries_a_fold_manifest_failure() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = two_segment_index(tmp.path());
    let gate = std::sync::Arc::new(FoldGate::new());
    index.gate_next_fold(std::sync::Arc::clone(&gate));
    let worker = std::sync::Arc::clone(&index);
    let folding = std::thread::spawn(move || worker.maintain(Maintenance::Rebuild));
    gate.snapshot_ready.wait();

    let manifest = tmp.path().join(META_FILE);
    let saved = block_existing_file(&manifest);
    gate.resume.wait();
    assert!(
        folding.join().expect("fold thread").is_err(),
        "a directory accepted the replacement manifest"
    );

    restore_blocked_file(&manifest, &saved);
    index.commit().expect("empty retry commit");
    assert_eq!(index.stats().expect("stats").segments, 1);
    assert!(
        !tmp.path().join("seg-00000001.fnames").exists()
            && !tmp.path().join("seg-00000002.fnames").exists(),
        "the publication retry left the folded inputs on disk"
    );
    drop(index);

    let reopened = NativeIndex::open_or_create(tmp.path()).expect("reopen");
    assert_eq!(reopened.stats().expect("stats").segments, 1);
    assert_eq!(hits(&reopened).len(), 2);
}
