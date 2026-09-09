//! What a path search costs, and what it could cost.
//!
//! A term with a separator asks about the path, and the index builds the path
//! of every row: 2.68 M lookups, joins and folds, 1.7 s. Rows live in far fewer
//! directories than there are rows — 326,450 against 2,684,498 — so this
//! measures reading the table once against a number per row.
//!
//!   cargo run --release -p scour-index-native --example pathcost -- /var/tmp/idx/native rapor

use std::path::PathBuf;
use std::time::Instant;

use scour_index_native::{Field, Live};

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = PathBuf::from(args.next().expect("give the index's `native` directory"));
    let needle = args.next().unwrap_or_else(|| "projeler/scour".into());
    let needle = needle.to_lowercase();

    let mut numbers: Vec<u64> = std::fs::read_dir(&dir)
        .expect("the directory cannot be read")
        .filter_map(|e| {
            let name = e.ok()?.file_name().into_string().ok()?;
            let stem = name.strip_prefix("seg-")?.strip_suffix(".cols")?;
            stem.parse().ok()
        })
        .collect();
    numbers.sort_unstable();

    let mut rows_all = 0usize;
    let mut dirs_all = 0usize;
    let mut whole_us = 0u128;
    let mut table_us = 0u128;
    let mut column_us = 0u128;
    let mut hits_whole = 0usize;
    let mut hits_two_step = 0usize;

    for number in numbers {
        let Ok(live) = Live::open(&dir, number, 0) else {
            continue;
        };
        let Ok(seg) = live.view() else { continue };
        let rows = live.rows();
        rows_all += rows;

        // 1. What it does now: build every row's path and look in it.
        let began = Instant::now();
        for row in 0..rows {
            let name = seg.names.get(row).unwrap_or_default();
            let path = seg.path(row, name);
            if path.to_lowercase().contains(&needle) {
                hits_whole += 1;
            }
        }
        whole_us += began.elapsed().as_micros();

        // 2. What it could do. First the table, once: which directories hold
        //    the term anywhere in them.
        let began = Instant::now();
        let dirs = seg.dirs.len();
        dirs_all += dirs;
        let mut wanted = vec![false; dirs];
        for (id, want) in wanted.iter_mut().enumerate().take(dirs) {
            if let Some(path) = seg.dirs.get(id as u32)
                && path.to_lowercase().contains(&needle)
            {
                *want = true;
            }
        }
        table_us += began.elapsed().as_micros();

        // 3. Then a number per row. No string is built and nothing is folded.
        let began = Instant::now();
        for row in 0..rows {
            let id = seg.num_of(Field::DirId, row) as usize;
            if wanted.get(id).copied().unwrap_or(false) {
                hits_two_step += 1;
            }
        }
        column_us += began.elapsed().as_micros();
    }

    let whole = whole_us as f64 / 1000.0;
    let two = (table_us + column_us) as f64 / 1000.0;
    println!("term `{needle}` over {rows_all} rows in {dirs_all} directories\n");
    println!("  building every path   {whole:>8.1} ms   {hits_whole} rows");
    println!(
        "  the table, once       {:>8.1} ms",
        table_us as f64 / 1000.0
    );
    println!(
        "  a number per row      {:>8.1} ms   {hits_two_step} rows",
        column_us as f64 / 1000.0
    );
    println!("  ————");
    println!(
        "  two steps            {two:>8.1} ms   ({:.0}× faster)",
        whole / two.max(0.001)
    );
    println!(
        "\nThe two-step answer misses rows whose match straddles the last\n\
         separator — `Scour/main.rs` is a directory's tail, a slash and a\n\
         name. That is why the counts differ by {}.",
        hits_whole.saturating_sub(hits_two_step)
    );
}
