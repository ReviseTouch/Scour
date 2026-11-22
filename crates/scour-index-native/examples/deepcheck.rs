//! Deep pages in the stored orders reached rather than walked to — name, path,
//! extension, and modified oldest first — printed as digests to diff a reach
//! against the walk it replaces. Read-only; point it at a copy:
//!
//!   cargo run --release -p scour-index-native --example deepcheck -- /tmp/idx/native > reach.txt
//!   SCOUR_NO_REACH=1 cargo run --release -p scour-index-native --example deepcheck -- /tmp/idx/native > walk.txt

use std::hash::{Hash, Hasher};
use std::time::Instant;

use scour_core::{Index, Page, SearchRequest, SortKey};
use scour_index_native::NativeIndex;

fn main() {
    let dir = std::env::args().nth(1).expect("index directory (…/native)");
    let index = NativeIndex::open_or_create(std::path::Path::new(&dir)).expect("open");
    let rows = index.stats().expect("stats").entries;
    // A fixed sequence, so two runs ask for the same pages.
    let mut seed = 0x9e37_79b9_7f4a_7c15u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    // And the two that sent a page back to the walk on 6.9 million rows: a name
    // shared by 140 thousand files sits at 3.6 million by name.
    let mut offsets: Vec<u64> = vec![
        2_000,
        2_001,
        77_777,
        rows / 2,
        3_500_000.min(rows),
        3_600_000.min(rows),
        rows - 300,
        rows - 1,
        rows,
    ];
    offsets.extend((0..40).map(|_| 2_000 + next() % rows));
    let mut spent = std::time::Duration::ZERO;
    let orders = [
        (SortKey::Name, true),
        (SortKey::Name, false),
        (SortKey::Path, true),
        (SortKey::Path, false),
        (SortKey::Ext, true),
        (SortKey::Ext, false),
        (SortKey::Modified, false),
    ];
    for (sort, descending) in orders {
        let mut took: Vec<(f64, u64, u32, u64)> = Vec::new();
        for &offset in &offsets {
            let limit = [1, 7, 40, 200][(next() % 4) as usize];
            let started = Instant::now();
            let found = index
                .search(&SearchRequest {
                    query: scour_query::parse(""),
                    sort,
                    descending,
                    page: Page {
                        offset: offset as u32,
                        limit,
                        count_cap: 1_000,
                    },
                })
                .expect("search");
            spent += started.elapsed();
            took.push((
                started.elapsed().as_secs_f64() * 1000.0,
                offset,
                limit,
                found.rows_visited,
            ));
            let mut digest = std::collections::hash_map::DefaultHasher::new();
            for hit in &found.hits {
                hit.path.hash(&mut digest);
            }
            println!(
                "{sort:?} {descending} {offset} {limit}: {} rows {:016x} {}",
                found.hits.len(),
                digest.finish(),
                found.hits.first().map(|h| h.path.as_str()).unwrap_or(""),
            );
        }
        // On stderr, so the two runs' stdout still diff clean.
        took.sort_by(|a, b| a.0.total_cmp(&b.0));
        let (worst, at, limit, visited) = took[took.len() - 1];
        eprintln!(
            "{sort:?} descending={descending}: median {:.1} ms, \
             worst {worst:.1} ms at {at}+{limit}, {visited} rows visited",
            took[took.len() / 2].0,
        );
    }
    eprintln!("{} pages in {:.1?}", offsets.len() * orders.len(), spent);
}
