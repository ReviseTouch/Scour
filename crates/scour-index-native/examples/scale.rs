//! How the numbers move when the corpus does: size, memory, build time and
//! query latency across the curve, so that "it scales" is a table.
//!
//! `cargo run --release -p scour-index-native --example scale [max entries]`

use std::time::Instant;

use scour_core::{Change, Index, Maintenance, Page, SearchRequest, SortKey};
use scour_index_native::NativeIndex;
use scour_mock::{MockOptions, generate};
use scour_query::parse_at;

const NOW: i64 = 1_785_000_000;

/// What a search box issues: a page of forty, counting to five hundred.
const BOX: Page = Page {
    offset: 0,
    limit: 40,
    count_cap: 500,
};

const CASES: &[&str] = &[
    "",
    "rapor",
    "ra",
    "ext:rs",
    "*.pdf",
    "kind:image",
    "size:>10mb",
    "under:/home/u/Projeler ext:rs",
];

/// Memory this process owns, as opposed to mapped file pages the kernel can
/// drop: an mmap'd index is not resident cost.
fn anon_mb() -> u64 {
    std::fs::read_to_string("/proc/self/smaps_rollup")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("Anonymous:"))
                .and_then(|l| {
                    l.split_whitespace()
                        .nth(1)
                        .and_then(|v| v.parse::<u64>().ok())
                })
        })
        .map(|kb| kb / 1024)
        .unwrap_or(0)
}

fn best_ms(index: &NativeIndex, q: &str) -> f64 {
    let req = SearchRequest {
        query: parse_at(q, NOW),
        sort: SortKey::Modified,
        descending: true,
        page: BOX,
    };
    let mut best = f64::MAX;
    for _ in 0..5 {
        let t = Instant::now();
        index.search(&req).expect("search");
        best = best.min(t.elapsed().as_secs_f64() * 1000.0);
    }
    best
}

fn main() {
    let max: usize = std::env::args()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(10_000_000);

    println!(
        "  {:>10}{:>10}{:>10}{:>9}{:>10}{:>9}",
        "entries", "MB", "B/entry", "index", "rebuild", "anon MB"
    );
    let mut rows: Vec<(usize, Vec<f64>)> = Vec::new();

    for files in [
        250_000usize,
        500_000,
        1_000_000,
        2_000_000,
        5_000_000,
        10_000_000,
    ] {
        if files > max {
            break;
        }
        let tmp = tempfile::tempdir().expect("tmpdir");
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");

        let fs = generate(&MockOptions {
            files,
            now: NOW,
            ..Default::default()
        });
        let n = fs.entries.len();
        let t = Instant::now();
        for part in fs.entries.chunks(200_000) {
            index
                .apply(&mut part.iter().cloned().map(Change::Upsert))
                .expect("apply");
            index.commit().expect("commit");
        }
        let indexing = t.elapsed();
        drop(fs);

        let t = Instant::now();
        index.maintain(Maintenance::Rebuild).expect("rebuild");
        let rebuild = t.elapsed();

        let s = index.stats().expect("stats");
        // Warm the maps once so the timings below measure the search and not
        // the first page fault.
        for q in CASES {
            best_ms(&index, q);
        }
        println!(
            "  {:>10}{:>10.1}{:>10.1}{:>8.1?}{:>10.1?}{:>9}",
            n,
            s.bytes_on_disk as f64 / 1_048_576.0,
            s.bytes_on_disk as f64 / n as f64,
            indexing,
            rebuild,
            anon_mb()
        );
        rows.push((n, CASES.iter().map(|q| best_ms(&index, q)).collect()));
    }

    println!("\n  best of five, warm, page of 40 counting to 500 — milliseconds\n");
    print!("  {:<32}", "query");
    for (n, _) in &rows {
        print!("{:>10}", format!("{}k", n / 1000));
    }
    println!();
    for (i, q) in CASES.iter().enumerate() {
        print!("  {:<32}", if q.is_empty() { "\"\"" } else { q });
        for (_, times) in &rows {
            print!("{:>10.2}", times[i]);
        }
        println!();
    }
}
