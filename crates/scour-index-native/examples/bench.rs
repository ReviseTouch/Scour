//! What a query costs, on a tree shaped like a real one.
//!
//! `cargo run --release -p scour-index-native --example bench`

use std::time::Instant;

use scour_core::SortKey;
use scour_index_native::{
    ColumnBlocks, DirTable, ExtensionOrder, NameArena, NameOrder, PathOrder, Plan, Segment,
    TrigramIndex, Wanted, build, run,
};
use scour_mock::{MockOptions, generate};
use scour_query::parse_at;

const NOW: i64 = 1_785_000_000;

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_000_000);
    let built = Instant::now();
    let fs = generate(&MockOptions {
        files: n,
        now: NOW,
        ..Default::default()
    });
    let bytes = build(&fs.entries);
    let rows = fs.entries.len();
    println!(
        "{rows} entries · {:.1} MB · {:.1} B/entry · built in {:.1?}\n",
        bytes.total() as f64 / 1_048_576.0,
        bytes.total() as f64 / rows as f64,
        built.elapsed()
    );

    let seg = Segment {
        names: NameArena::open(&bytes.names).expect("names"),
        folded: NameArena::open(&bytes.fnames).expect("fnames"),
        cols: ColumnBlocks::open(&bytes.cols).expect("cols"),
        dirs: DirTable::open(&bytes.dirs).expect("dirs"),
        tri: TrigramIndex::open(&bytes.tri_dict, &bytes.tri_post).expect("tri"),
        porder: PathOrder::open(&bytes.porder),
        norder: NameOrder::open(&bytes.norder),
        eorder: ExtensionOrder::open(&bytes.eorder),
        alive: &bytes.alive,
    };

    println!(
        "  {:<40}{:>9}{:>10}{:>12}{:>7}",
        "query", "matches", "visited", "time", ""
    );
    let cases: &[(&str, SortKey, bool)] = &[
        ("", SortKey::Modified, true),
        ("rapor", SortKey::Modified, true),
        ("main", SortKey::Modified, true),
        ("ext:rs", SortKey::Modified, true),
        ("kind:image", SortKey::Modified, true),
        ("kind:code dm:30d", SortKey::Modified, true),
        ("under:/home/u/Projeler ext:rs", SortKey::Modified, true),
        ("*.pdf", SortKey::Modified, true),
        ("size:>1mb", SortKey::Modified, true),
        ("ab", SortKey::Modified, true),
        ("", SortKey::Name, false),
        ("", SortKey::Name, true),
        ("ext:rs", SortKey::Size, true),
        ("ext:rs", SortKey::Name, false),
    ];
    let cap: usize = std::env::args()
        .nth(2)
        .and_then(|v| v.parse().ok())
        .unwrap_or(100_000);
    println!("  (count cap {cap})\n");
    for &(q, sort, desc) in cases {
        let plan = Plan::compile(&parse_at(q, NOW), &seg).expect("compile");
        // Warm, then the best of five: this measures the code, not the cache.
        let mut best = f64::MAX;
        let mut found = None;
        for _ in 0..5 {
            let t = Instant::now();
            let f = run(
                &seg,
                &plan,
                Wanted {
                    sort,
                    descending: desc,
                    offset: 0,
                    limit: 40,
                    count_cap: cap,
                    rank_only: false,
                },
            );
            best = best.min(t.elapsed().as_secs_f64() * 1000.0);
            found = Some(f);
        }
        let f = found.expect("ran");
        let label = if desc && sort == SortKey::Modified {
            format!("{q:?}")
        } else {
            format!("{q:?} by {sort:?}{}", if desc { " desc" } else { " asc" })
        };
        println!(
            "  {label:<40}{:>9}{:>10}{:>10.2} ms{:>7}",
            if f.capped {
                format!("{}+", f.total)
            } else {
                f.total.to_string()
            },
            f.rows_visited,
            best,
            if f.early_exit { "stop" } else { "" }
        );
    }
}
