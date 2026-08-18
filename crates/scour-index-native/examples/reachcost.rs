//! What a page costs at every depth, walked to and reached.
//!
//! The empty query in the stored order — what a window opens on and what a
//! scrollbar drags through. Run it twice on the same copied index:
//!
//!   cp -a --reflink=auto ~/.local/share/scour/index /var/tmp/idx
//!   rm -f /var/tmp/idx/native/index.lock
//!   cargo run --release -p scour-index-native --example reachcost -- /var/tmp/idx
//!   SCOUR_NO_REACH=1 cargo run --release -p scour-index-native --example reachcost -- /var/tmp/idx

use std::time::Instant;

use scour_core::{Index, Page, SearchRequest, SortKey};
use scour_index_native::NativeIndex;

const OFFSETS: [u32; 18] = [
    0, 1_000, 1_999, 2_000, 2_001, 7_777, 10_000, 99_999, 100_000, 123_456, 500_000, 999_999,
    1_000_000, 1_500_001, 2_000_000, 2_345_678, 2_400_000, 2_600_000,
];

fn main() {
    let dir = std::env::args().nth(1).expect("give the index directory");
    let index = NativeIndex::open_or_create(std::path::Path::new(&dir)).expect("open");
    let walked = std::env::var_os("SCOUR_NO_REACH").is_some();
    println!("{} — {}", dir, if walked { "walked to" } else { "reached" });
    println!(
        "{:>10}  {:>10}  {:>14}  {:>6}  page digest",
        "offset", "median ms", "rows visited", "rows"
    );
    for offset in OFFSETS {
        let mut runs = Vec::new();
        let mut visited = 0;
        let mut rows = 0;
        // **The page itself, not only what it cost.** Two runs of this over one
        // index — one reached, one walked — have to agree digit for digit, and
        // a page that is one row out looks exactly like a page that is not.
        let mut digest = 0u64;
        for _ in 0..3 {
            let began = Instant::now();
            let found = index
                .search(&SearchRequest {
                    query: scour_core::Ast::default(),
                    sort: SortKey::Modified,
                    descending: true,
                    page: Page {
                        offset,
                        limit: 200,
                        count_cap: 1_000,
                    },
                })
                .expect("search");
            runs.push(began.elapsed().as_secs_f64() * 1000.0);
            visited = found.rows_visited;
            rows = found.hits.len();
            digest = found.hits.iter().fold(1469598103934665603u64, |acc, hit| {
                hit.path
                    .bytes()
                    .fold(acc, |a, b| (a ^ u64::from(b)).wrapping_mul(1099511628211))
            });
        }
        runs.sort_by(f64::total_cmp);
        println!(
            "{offset:>10}  {:>10.2}  {visited:>14}  {rows:>6}  {digest:016x}",
            runs[1]
        );
    }
}
