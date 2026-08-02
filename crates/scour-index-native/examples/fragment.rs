//! What a segment count costs.
//!
//! Every segment is internally in date order, so each one can stop early on
//! its own — but each one has to find its *own* page before it does, and every
//! commit has to look for the previous row of everything it writes. Both of
//! those grow with the number of segments, and neither is visible in a
//! single-segment benchmark.
//!
//! This is the measurement that sets the compaction policy.
//!
//! `cargo run --release -p scour-index-native --example fragment [entries]`

use std::time::{Duration, Instant};

use scour_core::{Change, Entry, Index, Maintenance, Page, SearchRequest, SortKey};
use scour_index_native::NativeIndex;
use scour_mock::{MockOptions, generate};
use scour_query::parse_at;

const NOW: i64 = 1_785_000_000;

const CASES: &[(&str, SortKey, bool)] = &[
    ("", SortKey::Modified, true),
    ("rapor", SortKey::Modified, true),
    ("ext:rs", SortKey::Modified, true),
    ("kind:code dm:30d", SortKey::Modified, true),
    ("under:/home/u/Projeler ext:rs", SortKey::Modified, true),
    ("ext:rs", SortKey::Size, true),
];

struct Layout {
    label: String,
    _tmp: tempfile::TempDir,
    index: NativeIndex,
}

fn index_in_chunks(entries: &[Entry], chunk: usize) -> (Layout, Duration) {
    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");
    let t = Instant::now();
    for part in entries.chunks(chunk.max(1)) {
        index
            .apply(&mut part.iter().cloned().map(Change::Upsert))
            .expect("apply");
        index.commit().expect("commit");
    }
    let took = t.elapsed();
    (
        Layout {
            label: String::new(),
            _tmp: tmp,
            index,
        },
        took,
    )
}

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(1_000_000);
    let fs = generate(&MockOptions {
        files: n,
        now: NOW,
        ..Default::default()
    });
    let rows = fs.entries.len();
    println!("{rows} entries\n");

    println!(
        "  {:<14}{:>10}{:>10}{:>10}{:>12}",
        "layout", "segments", "index", "MB", "B/entry"
    );

    let mut layouts: Vec<Layout> = Vec::new();

    // One segment, reached the only way it can be: by folding.
    {
        let (mut l, indexing) = index_in_chunks(&fs.entries, rows);
        let t = Instant::now();
        l.index.maintain(Maintenance::Rebuild).expect("rebuild");
        let rebuild = t.elapsed();
        let s = l.index.stats().expect("stats");
        l.label = format!("{} seg", s.segments);
        println!(
            "  {:<14}{:>10}{:>9.1?}{:>10.1}{:>10.1}   (rebuild {:.1?})",
            "rebuilt",
            s.segments,
            indexing,
            s.bytes_on_disk as f64 / 1_048_576.0,
            s.bytes_on_disk as f64 / rows as f64,
            rebuild
        );
        layouts.push(l);
    }

    // What a scan actually leaves, then what one compaction does to it.
    {
        let (mut l, indexing) = index_in_chunks(&fs.entries, rows);
        let t = Instant::now();
        l.index.maintain(Maintenance::Compact).expect("compact");
        let compact = t.elapsed();
        let s = l.index.stats().expect("stats");
        l.label = format!("{} seg", s.segments);
        println!(
            "  {:<14}{:>10}{:>9.1?}{:>10.1}{:>10.1}   (compact {:.1?})",
            "compacted",
            s.segments,
            indexing,
            s.bytes_on_disk as f64 / 1_048_576.0,
            s.bytes_on_disk as f64 / rows as f64,
            compact
        );
        layouts.push(l);
    }

    for target in [1usize, 2, 8, 32] {
        let (mut l, indexing) = index_in_chunks(&fs.entries, rows.div_ceil(target));
        let s = l.index.stats().expect("stats");
        l.label = format!("{} seg", s.segments);
        println!(
            "  {:<14}{:>10}{:>9.1?}{:>10.1}{:>10.1}",
            format!("{target} commit(s)"),
            s.segments,
            indexing,
            s.bytes_on_disk as f64 / 1_048_576.0,
            s.bytes_on_disk as f64 / rows as f64,
        );
        layouts.push(l);
    }

    println!("\n  best of five, warm, count cap 500 — milliseconds\n");
    print!("  {:<34}", "query");
    for l in &layouts {
        print!("{:>9}", l.label);
    }
    println!();

    for &(q, sort, desc) in CASES {
        let label = if desc && sort == SortKey::Modified {
            format!("{q:?}")
        } else {
            format!("{q:?} by {sort:?}")
        };
        print!("  {label:<34}");
        for l in &layouts {
            let req = SearchRequest {
                query: parse_at(q, NOW),
                sort,
                descending: desc,
                page: Page {
                    offset: 0,
                    limit: 40,
                    count_cap: 500,
                },
            };
            let mut best = f64::MAX;
            for _ in 0..5 {
                let t = Instant::now();
                l.index.search(&req).expect("search");
                best = best.min(t.elapsed().as_secs_f64() * 1000.0);
            }
            print!("{best:>9.2}");
        }
        println!();
    }
}
