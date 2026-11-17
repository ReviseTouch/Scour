//! A deep page in a numeric order, gathered rather than walked to. A file of its
//! own because the offset it starts at is read once per process: a real index
//! gathers only from 300,000 rows down, and this one is set to start far
//! shallower before anything searches.

use scour_core::{Change, Entry, EntryId, Index, Meta, Page, SearchRequest, SortKey, SourceId};
use scour_index_native::NativeIndex;
use scour_query::parse_at;

const NOW: i64 = 1_785_000_000;

/// A deep page in a numeric order is gathered from two passes instead of being
/// walked to: it has to be the page the walk finds, ties at both ends, removed
/// rows, pending removals, folders sized by what they hold, and a filter.
#[test]
fn a_deep_numeric_page_is_the_page_the_walk_finds() {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    // SAFETY: the only test in this process, and nothing has searched yet.
    unsafe { std::env::set_var("SCOUR_GATHER_FROM", "30000") };
    let mut ino = 0u64;
    let names = ["a.txt", "b.rs", "c.pdf", "d.png", "e.mp3", "f.zip", "g"];
    for part in 0..5i64 {
        let entries: Vec<Entry> = (0..25_000i64)
            .map(|i| {
                ino += 1;
                let dir = i % 600 == 0;
                let name = names[(i % 7) as usize];
                // Few sizes, some huge, so bins hold one value and several.
                let size = match i % 11 {
                    0 => 0,
                    1..=4 => (i * 37 + part) % 300,
                    5..=8 => ((i * 7919) % 50_000) * 1_000,
                    _ => 1 << (20 + i % 20),
                };
                Entry {
                    id: EntryId::inode(SourceId(0), 66_310, ino),
                    path: if dir {
                        format!("/n/p{part}/d{:03}", i / 600)
                    } else {
                        format!("/n/p{part}/d{:03}/{i}{name}", i / 600)
                    },
                    is_dir: dir,
                    meta: Meta {
                        mtime: NOW - (i * 13 + part * 7) % 900,
                        size,
                        ctime: NOW - (i % 50) * 86_400,
                        atime: NOW - ((i * 31) % 4_000) * 3_600,
                        ..Meta::UNKNOWN
                    },
                }
            })
            .collect();
        index
            .apply(&mut entries.into_iter().map(Change::Upsert))
            .expect("apply");
        index.commit().expect("commit");
    }
    let search = |q: &str, sort, descending, offset: usize, limit: usize| -> (Vec<String>, u64) {
        let r = index
            .search(&SearchRequest {
                query: parse_at(q, NOW),
                sort,
                descending,
                page: Page {
                    offset: offset as u32,
                    limit: limit as u32,
                    count_cap: 10_000_000,
                },
            })
            .expect("search");
        (r.hits.into_iter().map(|h| h.path).collect(), r.total)
    };
    // Removed and committed from every segment, then a few left pending.
    let gone: Vec<String> = search("", SortKey::Path, false, 0, 200_000)
        .0
        .into_iter()
        .enumerate()
        .filter(|(i, _)| i % 13 == 6)
        .map(|(_, p)| p)
        .collect();
    let (now, later) = gone.split_at(gone.len() - 40);
    index
        .apply(
            &mut now
                .iter()
                .map(|p| Change::RemoveSubtree { path: p.clone() }),
        )
        .expect("remove");
    index.commit().expect("commit");
    index
        .apply(
            &mut later
                .iter()
                .map(|p| Change::RemoveSubtree { path: p.clone() }),
        )
        .expect("remove pending");

    for q in ["", "p1 | p3", "!pdf"] {
        for sort in [
            SortKey::Size,
            SortKey::Created,
            SortKey::Accessed,
            SortKey::Kind,
        ] {
            for descending in [false, true] {
                // Offset zero is always walked.
                let (whole, total) = search(q, sort, descending, 0, 200_000);
                assert_eq!(whole.len() as u64, total);
                let last = whole.len();
                let mut offsets: Vec<usize> = (30_000..last).step_by(1_733).collect();
                offsets.extend([last.saturating_sub(1), last, last + 9]);
                for (n, &offset) in offsets.iter().enumerate() {
                    let limit = [1usize, 7, 40, 300][n % 4];
                    let (got, count) = search(q, sort, descending, offset, limit);
                    let from = offset.min(last);
                    let to = (offset + limit).min(last);
                    assert_eq!(
                        got,
                        whole[from..to],
                        "{q:?} {sort:?} descending={descending} at {offset}+{limit}"
                    );
                    assert_eq!(count, total, "{q:?} {sort:?} total");
                }
            }
        }
    }
}
