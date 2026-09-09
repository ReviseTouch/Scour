//! What the folder-size column adds to a page of results.
//!
//! The search and the weighing of whatever folders it turned up, measured
//! apart: "the list got slow" and "the size column got slow" are one sentence
//! from outside. Read-only; point it at a copy:
//!
//!   cargo run --release -p scour-index-native --example foldercost -- /tmp/idx/native

use std::time::Instant;

use scour_core::{Index, Page, SearchRequest, SortKey};
use scour_index_native::NativeIndex;

fn main() {
    let dir = std::env::args().nth(1).expect("index directory (…/native)");
    let query = std::env::args().nth(2).unwrap_or_default();
    let index = NativeIndex::open_or_create(std::path::Path::new(&dir)).expect("open");

    for limit in [1u32, 40, 200] {
        let mut best_search = f64::MAX;
        let mut hits = Vec::new();
        for _ in 0..3 {
            let began = Instant::now();
            let res = index
                .search(&SearchRequest {
                    query: scour_query::parse(&query),
                    sort: SortKey::Modified,
                    descending: true,
                    page: Page {
                        offset: 0,
                        limit,
                        count_cap: 100_000,
                    },
                })
                .expect("search");
            best_search = best_search.min(began.elapsed().as_secs_f64() * 1000.0);
            hits = res.hits;
        }

        // Exactly what `Engine::weigh_folders` does with that page.
        let paths: Vec<String> = hits
            .iter()
            .filter(|h| h.is_dir)
            .map(|h| h.path.clone())
            .collect();
        let mut best_weigh = f64::MAX;
        for _ in 0..3 {
            let began = Instant::now();
            let sizes = index.subtree_sizes(&paths).expect("subtree sizes");
            std::hint::black_box(sizes.len());
            best_weigh = best_weigh.min(began.elapsed().as_secs_f64() * 1000.0);
        }

        println!(
            "limit {limit:3}: arama {best_search:7.2} ms · {:3} klasör tartıldı {best_weigh:8.2} ms · toplam {:8.2} ms",
            paths.len(),
            best_search + best_weigh
        );
    }
}
