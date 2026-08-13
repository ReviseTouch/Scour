//! What one page of results costs, in every order a heading can ask for.
//!
//! The list fetches two hundred rows at a time and the window it is looking at
//! decides the offset, so this is the shape of nearly every request the
//! interface makes. It is also the request that stopped being fast: measured
//! from outside on a live service, the empty query — the one a window opens on
//! — went from single-digit milliseconds to forty-six.
//!
//! Read-only, and pointed at a copy so a running service is neither blocked
//! nor believed:
//!
//!   cp -a ~/.local/share/scour/index /tmp/idx && rm -f /tmp/idx/index.lock
//!   cargo run --release -p scour-index-native --example searchcost -- /tmp/idx/native
//!
//! The count cap is the one the bridge sends rather than `u32::MAX`, because
//! an uncapped total is the one piece of work proportional to the number of
//! matches and the interface has never asked for it.

use std::time::Instant;

use scour_core::{Index, Page, SearchRequest, SortKey};
use scour_index_native::NativeIndex;

/// The orders a heading offers, plus the two directions. `Modified` descending
/// is the stored order and the one everything opens on; `Modified` ascending is
/// the same order walked backwards; the rest have to visit every match.
const ORDERS: [(SortKey, bool, &str); 6] = [
    (SortKey::Modified, true, "modified ↓ (stored order)"),
    (SortKey::Modified, false, "modified ↑ (backwards)"),
    (SortKey::Name, true, "name ↓"),
    (SortKey::Size, true, "size ↓"),
    (SortKey::Path, true, "path ↓"),
    (SortKey::Relevance, true, "relevance ↓"),
];

/// The offsets a window actually reaches: the first screen, a page down, and
/// the far end of what the list will scroll to.
const OFFSETS: [u32; 3] = [0, 2_000, 19_800];

fn main() {
    let dir = std::env::args().nth(1).expect("index directory (…/native)");
    let query = std::env::args().nth(2).unwrap_or_default();
    let index = NativeIndex::open_or_create(std::path::Path::new(&dir)).expect("open");
    let stats = index.stats().expect("stats");
    println!(
        "{} rows · {} segments · query {:?}\n",
        stats.entries, stats.segments, query
    );

    println!("  {:>8}  {:>8}  {:>8}   order / offset", "offset 0", "2 000", "19 800");
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
