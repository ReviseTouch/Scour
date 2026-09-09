//! What it costs to write entries into an index.
//!
//! ```bash
//! cargo run --release -p scour-index-native --example writepath -- 400000
//! ```
//!
//! Written against the public API only, so it compiles against older versions
//! of the crate and the two can be compared.

use std::time::Instant;

use scour_core::{Change, Index};
use scour_index_native::NativeIndex;
use scour_mock::{MockOptions, generate};

fn main() {
    let n: usize = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(400_000);
    let fs = generate(&MockOptions {
        files: n,
        ..Default::default()
    });
    let entries = fs.entries;

    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");

    // A bulk pass: many entries, committed in segment-sized chunks.
    let t0 = Instant::now();
    let (mut ap, mut cm) = (std::time::Duration::ZERO, std::time::Duration::ZERO);
    for part in entries.chunks(100_000) {
        let t = Instant::now();
        index
            .apply(&mut part.iter().cloned().map(Change::Upsert))
            .expect("apply");
        ap += t.elapsed();
        let t = Instant::now();
        index.commit().expect("commit");
        cm += t.elapsed();
    }
    let bulk = t0.elapsed();
    println!("          apply {:.0?}   commit {:.0?}", ap, cm);
    let stats = index.stats().expect("stats");

    // A rescan: the same entries again, every one of which now has an old row
    // that has to be found and killed. The path that pays for verification.
    let t1 = Instant::now();
    for part in entries.chunks(100_000) {
        index
            .apply(&mut part.iter().cloned().map(Change::Upsert))
            .expect("apply");
        index.commit().expect("commit");
    }
    let again_ms = t1.elapsed().as_secs_f64() * 1000.0;

    // And the case a watcher produces all day: a handful of entries that
    // already exist, so every one of them has an old row to find and kill.
    let again: Vec<_> = entries.iter().take(8).cloned().collect();
    let mut worst = 0f64;
    let mut total = 0f64;
    for round in 0..20 {
        let t = Instant::now();
        index
            .apply(&mut again.iter().cloned().map(Change::Upsert))
            .expect("apply");
        index.commit().expect("commit");
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        total += ms;
        if round > 0 {
            worst = worst.max(ms);
        }
    }

    println!(
        "{:>9} entries  bulk {:>7.0} ms  {:>5.2} µs/entry  {:>6.1} MB  {} segments",
        stats.entries,
        bulk.as_secs_f64() * 1000.0,
        bulk.as_secs_f64() * 1e6 / stats.entries as f64,
        stats.bytes_on_disk as f64 / 1e6,
        stats.segments,
    );
    println!(
        "          rescan (same entries again) {:>7.0} ms  {:>5.2} µs/entry",
        again_ms,
        again_ms * 1000.0 / stats.entries as f64,
    );
    println!(
        "          8 re-upserts × 20 commits: mean {:.1} ms, worst {:.1} ms",
        total / 20.0,
        worst
    );
}
