//! What each column costs, on the real corpus.
//!
//! `cargo run --release -p scour-index-native --example columns <index-dir>`
//!
//! Sixteen numbers a row sounds like a lot and the total is 33 bytes an entry,
//! so the obvious question is which of them to drop. The obvious answer is
//! wrong in both directions, which is why this exists: the columns are
//! bit-packed per block against that block's minimum, so a column that barely
//! varies costs almost nothing however many rows there are, and a column of
//! independent timestamps costs nearly its full width.
//!
//! Point it at a **copy** of an index — the service holds a writer lock.

use scour_index_native::{ColumnWriter, Field, Live};

fn main() {
    let dir = std::env::args().nth(1).expect("index dir");
    let dir = std::path::Path::new(&dir);

    // The largest segment: on a settled index it is the whole thing.
    let mut best: Option<(u64, u64)> = None;
    for entry in std::fs::read_dir(dir).expect("read_dir").flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.ends_with(".names") {
            continue;
        }
        let Some(num) = name
            .strip_prefix("seg-")
            .and_then(|r| r.split('.').next())
            .and_then(|n| n.parse::<u64>().ok())
        else {
            continue;
        };
        let len = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if best.is_none_or(|(_, b)| len > b) {
            best = Some((num, len));
        }
    }
    let (number, _) = best.expect("no segment there");
    let live = Live::open(dir, number, 0).expect("open");
    let seg = live.view().expect("view");
    let rows = seg.rows();

    // Every field's values, read out once.
    let all: Vec<Vec<i64>> = Field::ALL
        .iter()
        .map(|&f| (0..rows).map(|r| seg.num_of(f, r)).collect())
        .collect();

    // One writer per field, with every other field held at zero, so what is
    // measured is that column and the per-block overhead it cannot avoid.
    println!("{rows} rows\n");
    println!(
        "  {:<10} {:>12} {:>10}  distinct",
        "column", "bytes", "per row"
    );
    let mut total = 0usize;
    for (i, &f) in Field::ALL.iter().enumerate() {
        let mut w = ColumnWriter::new();
        for &v in &all[i] {
            let mut row = [0i64; Field::ALL.len()];
            row[f as usize] = v;
            w.push(row);
        }
        // The empty writer is the floor: headers and the per-block bookkeeping
        // of the other columns held at zero. Subtracting it leaves what this
        // column actually costs.
        let with = w.finish().len();
        let mut e = ColumnWriter::new();
        for _ in 0..rows {
            e.push([0i64; Field::ALL.len()]);
        }
        let floor = e.finish().len();
        let cost = with.saturating_sub(floor);
        total += cost;
        let mut seen: Vec<i64> = all[i].clone();
        seen.sort_unstable();
        seen.dedup();
        println!(
            "  {:<10} {:>12} {:>10.2}  {}",
            format!("{f:?}"),
            cost,
            cost as f64 / rows as f64,
            seen.len()
        );
    }
    println!(
        "\n  {:<10} {:>12} {:>10.2}",
        "sum",
        total,
        total as f64 / rows as f64
    );
}
