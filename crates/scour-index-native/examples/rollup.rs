//! What a subtree weighs — the measurement behind the disk-usage report.
//!
//! The question TreeSize answers takes it minutes, because it walks the
//! filesystem. Everything it needs is already in this index, and the layout
//! happens to make the rollup nearly free:
//!
//! * every row carries the number of the directory it sits in;
//! * directory numbers are handed out in **sorted path order**, so a subtree is
//!   a contiguous range of them plus the directory's own number.
//!
//! So one pass over the rows gives every directory the bytes sitting *directly*
//! in it, and one pass over the directory table — which is sorted, so a stack
//! reconstructs the hierarchy — rolls those up into subtree totals.
//!
//! `cargo run --release -p scour-index-native --example rollup <index-dir> [top]`

use std::time::Instant;

use scour_index_native::{Field, NativeIndex};

fn main() {
    let dir = std::env::args().nth(1).expect("index dir");
    let top: usize = std::env::args()
        .nth(2)
        .and_then(|v| v.parse().ok())
        .unwrap_or(15);
    let index = NativeIndex::open_or_create(std::path::Path::new(&dir)).expect("open");

    index
        .for_each_segment(&mut |_, seg| {
            let n_dirs = seg.dirs.len();
            let rows = seg.rows();
            println!("{rows} rows · {n_dirs} directories\n");

            // --- pass one: bytes sitting directly in each directory ---------
            let t = Instant::now();
            let mut own_bytes = vec![0u64; n_dirs];
            let mut own_files = vec![0u32; n_dirs];
            for row in 0..rows {
                if !seg.is_alive(row) {
                    continue;
                }
                let d = seg.dir_id(row) as usize;
                if d < n_dirs {
                    own_bytes[d] += seg.num_of(Field::Size, row).max(0) as u64;
                    own_files[d] += 1;
                }
            }
            let pass1 = t.elapsed();

            // --- pass two: roll children into parents ------------------------
            //
            // The table is sorted by path, so a stack of open ancestors is
            // enough: a directory whose path is not under the top of the stack
            // closes it, and closing adds its total to whatever is below.
            let t = Instant::now();
            let mut total = vec![0u64; n_dirs];
            let mut files = vec![0u64; n_dirs];
            let mut stack: Vec<(String, usize)> = Vec::new();
            for id in 0..n_dirs {
                let Some(path) = seg.dirs.get(id as u32) else {
                    continue;
                };
                while let Some((top_path, top_id)) = stack.last() {
                    if under(&path, top_path) {
                        break;
                    }
                    let (t_id, t_bytes, t_files) = (*top_id, total[*top_id], files[*top_id]);
                    stack.pop();
                    if let Some((_, parent)) = stack.last() {
                        total[*parent] += t_bytes;
                        files[*parent] += t_files;
                    }
                    let _ = t_id;
                }
                total[id] = own_bytes[id];
                files[id] = u64::from(own_files[id]);
                stack.push((path, id));
            }
            while let Some((_, id)) = stack.pop() {
                let (b, f) = (total[id], files[id]);
                if let Some((_, parent)) = stack.last() {
                    total[*parent] += b;
                    files[*parent] += f;
                }
            }
            let pass2 = t.elapsed();

            println!(
                "  rows      {pass1:.1?}\n  rollup    {pass2:.1?}\n  together  {:.1?}\n",
                pass1 + pass2
            );

            let mut order: Vec<usize> = (0..n_dirs).collect();
            order.sort_unstable_by_key(|&i| std::cmp::Reverse(total[i]));
            println!("  {:>10}  {:>9}  {}", "bytes", "files", "directory");
            for &id in order.iter().take(top) {
                println!(
                    "  {:>10}  {:>9}  {}",
                    human(total[id]),
                    files[id],
                    seg.dirs.get(id as u32).unwrap_or_default()
                );
            }
        })
        .expect("walk");
}

/// Is `path` at or below `prefix`?
fn under(path: &str, prefix: &str) -> bool {
    let p = prefix.trim_end_matches('/');
    if p.is_empty() {
        return true;
    }
    path.len() > p.len() && path.starts_with(p) && path.as_bytes()[p.len()] == b'/'
}

fn human(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}
