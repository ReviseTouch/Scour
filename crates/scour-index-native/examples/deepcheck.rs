//! Deep pages in name and path order, printed as digests to diff a reach
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
    let mut offsets: Vec<u64> = vec![2_000, 2_001, 77_777, rows / 2, rows - 300, rows - 1, rows];
    offsets.extend((0..40).map(|_| 2_000 + next() % rows));
    let mut spent = std::time::Duration::ZERO;
    for sort in [SortKey::Name, SortKey::Path] {
        for descending in [true, false] {
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
        }
    }
    eprintln!("{} pages in {:.1?}", offsets.len() * 4, spent);
}
