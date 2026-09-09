//! Whether a page deep in a result can be *reached* instead of walked to.
//!
//! A page at offset two million costs 105 ms and visits 2,080,974 rows. Seeking
//! rests on two claims measured here: `Mtime` is non-increasing with the row
//! number in *every* segment, and a rank over the live bitmap is cheap enough
//! to build per request. Read-only; point it at a copy:
//!
//!   cargo run --release -p scour-index-native --example rankcheck -- /var/tmp/idx/native

use std::path::PathBuf;
use std::time::Instant;

use scour_index_native::{Field, Live};

/// Live rows per entry of the prefix. One `u32` per this many rows.
const STRIDE: usize = 512;

fn main() {
    let dir = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("give the index's `native` directory"),
    );
    let mut numbers: Vec<u64> = std::fs::read_dir(&dir)
        .expect("the directory cannot be read")
        .filter_map(|e| {
            let name = e.ok()?.file_name().into_string().ok()?;
            let stem = name.strip_prefix("seg-")?.strip_suffix(".cols")?;
            stem.parse().ok()
        })
        .collect();
    numbers.sort_unstable();
    println!("{} segments in {}", numbers.len(), dir.display());

    let mut rows_all = 0usize;
    let mut live_all = 0u64;
    let mut breaks_all = 0usize;
    let mut build_us = 0u128;

    for number in numbers {
        // The generation is a stamp on the segment and nothing here reads it.
        let live = match Live::open(&dir, number, 0) {
            Ok(live) => live,
            Err(e) => {
                println!("seg-{number:08}: unreadable — {e}");
                continue;
            }
        };
        let seg = match live.view() {
            Ok(seg) => seg,
            Err(e) => {
                println!("seg-{number:08}: no view — {e}");
                continue;
            }
        };
        let rows = live.rows();

        // 1. Is the column monotone? Every row, not a sample: one inversion
        //    anywhere is a binary search that returns the wrong boundary.
        let began = Instant::now();
        let mut breaks = 0usize;
        let mut worst = 0i64;
        let mut previous = i64::MAX;
        for row in 0..rows {
            let when = seg.num_of(Field::Mtime, row);
            if when > previous {
                breaks += 1;
                worst = worst.max(when - previous);
            }
            previous = when;
        }
        let walk_us = began.elapsed().as_micros();

        // 2. What a rank costs to build. One popcount per eight rows, then a
        //    running total every `STRIDE`.
        let began = Instant::now();
        let prefix = rank_prefix(&live, rows);
        let rank_us = began.elapsed().as_micros();
        build_us += rank_us;

        let counted = live.live_rows();
        let summed = *prefix.last().unwrap_or(&0) as u64
            + tail_ones(&live, prefix.len().saturating_sub(1) * STRIDE, rows);
        rows_all += rows;
        live_all += counted;
        breaks_all += breaks;
        println!(
            "seg-{number:08}  {rows:>9} rows  {counted:>9} live  \
             mtime breaks {breaks:>6} (worst {worst}s)  \
             walk {walk_us:>7} µs  rank {rank_us:>5} µs  {}",
            if summed == counted {
                "rank agrees"
            } else {
                "RANK DISAGREES"
            }
        );
    }

    println!(
        "\n{rows_all} rows, {live_all} live, {breaks_all} inversions, \
         rank built in {build_us} µs for the whole index"
    );
    if breaks_all == 0 {
        println!("the column is monotone: a page can be reached by binary search");
    } else {
        println!("NOT monotone — a seek cannot rest on the column alone");
    }
}

/// Live rows before the start of every `STRIDE`-th row.
fn rank_prefix(live: &Live, rows: usize) -> Vec<u32> {
    let entries = rows.div_ceil(STRIDE);
    let mut prefix = Vec::with_capacity(entries);
    let mut running = 0u32;
    for i in 0..entries {
        prefix.push(running);
        let from = i * STRIDE;
        let to = ((i + 1) * STRIDE).min(rows);
        running += ones(live, from, to);
    }
    prefix
}

fn tail_ones(live: &Live, from: usize, to: usize) -> u64 {
    u64::from(ones(live, from, to))
}

/// How many rows in `from..to` are live.
fn ones(live: &Live, from: usize, to: usize) -> u32 {
    (from..to).filter(|&row| live.is_alive(row)).count() as u32
}
