//! What one page of results costs, in every order a heading can ask for.
//!
//! Two hundred rows at an offset is nearly every request the interface makes.
//! The count cap is the one the bridge sends rather than `u32::MAX`. A third
//! argument of `warm` builds the folder-size table first, which widens the
//! block ranges a size sort skips on. Read-only; point it at a copy:
//!
//!   cargo run --release -p scour-index-native --example searchcost -- /tmp/idx/native

use std::time::Instant;

use scour_core::{Index, Page, SearchRequest, SortKey};
use scour_index_native::NativeIndex;

/// The orders a heading offers, in both directions. `Modified` descending is
/// the stored order; `kind` is the awkward one, a hundred thousand rows to a
/// value; `name` and `path` are the two the zone map cannot bound at all.
const ORDERS: [(SortKey, bool, &str); 13] = [
    (SortKey::Modified, true, "modified ↓ (stored order)"),
    (SortKey::Modified, false, "modified ↑ (backwards)"),
    (SortKey::Name, true, "name ↓"),
    (SortKey::Name, false, "name ↑"),
    (SortKey::Ext, true, "extension ↓"),
    (SortKey::Ext, false, "extension ↑"),
    (SortKey::Size, true, "size ↓"),
    (SortKey::Size, false, "size ↑"),
    (SortKey::Created, true, "created ↓"),
    (SortKey::Kind, true, "kind ↓"),
    (SortKey::Path, true, "path ↓"),
    (SortKey::Relevance, true, "relevance ↓"),
    (SortKey::Relevance, false, "relevance ↑"),
];

/// The offsets a window actually reaches: the first screen, a page down, and
/// the far end of what the list will scroll to.
const OFFSETS: [u32; 3] = [0, 2_000, 19_800];

fn main() {
    let dir = std::env::args().nth(1).expect("index directory (…/native)");
    let query = std::env::args().nth(2).unwrap_or_default();
    let index = NativeIndex::open_or_create(std::path::Path::new(&dir)).expect("open");
    let warm = std::env::args().nth(3).as_deref() == Some("warm");
    if warm {
        index.subtree_sizes(&[]).expect("folder sizes");
    }
    let stats = index.stats().expect("stats");
    println!(
        "{} rows · {} segments · query {:?}{}\n",
        stats.entries,
        stats.segments,
        query,
        if warm { " · folder sizes warm" } else { "" }
    );

    println!(
        "  {:>8}  {:>8}  {:>8}   order / offset",
        "offset 0", "2 000", "19 800"
    );
    for (sort, descending, label) in ORDERS {
        let mut row = String::new();
        for offset in OFFSETS {
            // Least of three: the first pays for whatever of the map is not
            // resident, and nobody runs a window cold twice.
            let mut best = f64::MAX;
            for _ in 0..3 {
                let began = Instant::now();
                let res = index
                    .search(&SearchRequest {
                        query: scour_query::parse(&query),
                        sort,
                        descending,
                        page: Page {
                            offset,
                            limit: 200,
                            count_cap: 100_000,
                        },
                    })
                    .expect("search");
                let ms = began.elapsed().as_secs_f64() * 1000.0;
                std::hint::black_box(res.hits.len());
                best = best.min(ms);
            }
            row.push_str(&format!("  {best:8.1}"));
        }
        println!("{row}   {label}");
    }
}
