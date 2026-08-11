//! What one search actually does, segment by segment.
//!
//! `cargo run --release -p scour-index-native --example probe <index-dir> <query>`

use scour_index_native::{NativeIndex, Plan, Wanted, run};
use scour_query::parse_at;

fn main() {
    let dir = std::env::args().nth(1).expect("index dir");
    let q = std::env::args().nth(2).unwrap_or_default();
    let sort = match std::env::args().nth(4).unwrap_or_default().as_str() {
        "name" => scour_core::SortKey::Name,
        "path" => scour_core::SortKey::Path,
        "size" => scour_core::SortKey::Size,
        "ext" => scour_core::SortKey::Ext,
        "kind" => scour_core::SortKey::Kind,
        _ => scour_core::SortKey::Modified,
    };
    let cap: usize = std::env::args()
        .nth(3)
        .and_then(|v| v.parse().ok())
        .unwrap_or(500);
    let now = 1_785_000_000;
    let index = NativeIndex::open_or_create(std::path::Path::new(&dir)).expect("open");
    println!("query {q:?}  cap {cap}");
    index
        .for_each_segment(&mut |i, seg| {
            let plan = Plan::compile(&parse_at(&q, now), seg).expect("compile");
            let t = std::time::Instant::now();
            let found = run(
                seg,
                &plan,
                Wanted {
                    sort,
                    descending: true,
                    offset: 0,
                    limit: 40,
                    count_cap: cap,
                    rank_only: false,
                },
            );
            println!(
                "  seg {i}: {} rows, visited {}, built {}, counted {}, in {:.1?}",
                seg.rows(),
                found.rows_visited,
                found.rows_built,
                found.total,
                t.elapsed()
            );
        })
        .expect("walk");
}
