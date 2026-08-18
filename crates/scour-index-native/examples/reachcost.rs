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

const OFFSETS: [u32; 9] = [
    0, 1_000, 10_000, 100_000, 500_000, 1_000_000, 2_000_000, 2_400_000, 2_600_000,
];

fn main() {
    let dir = std::env::args().nth(1).expect("give the index directory");
    let index = NativeIndex::open_or_create(std::path::Path::new(&dir)).expect("open");
    let walked = std::env::var_os("SCOUR_NO_REACH").is_some();
    println!("{} — {}", dir, if walked { "walked to" } else { "reached" });
    println!(
        "{:>10}  {:>10}  {:>14}  {:>6}",
        "offset", "median ms", "rows visited", "rows"
    );
    for offset in OFFSETS {
        let mut runs = Vec::new();
        let mut visited = 0;
        let mut rows = 0;
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
        }
        runs.sort_by(f64::total_cmp);
        println!("{offset:>10}  {:>10.2}  {visited:>14}  {rows:>6}", runs[1]);
    }
}
