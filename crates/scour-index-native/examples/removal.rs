//! What a `rm -rf` costs the index.
//!
//! `cargo run --release -p scour-index-native --example removal [rows] [dirs]`
//!
//! The watcher reports a removed path per file and per directory, so deleting a
//! tree of fifty thousand files arrives as fifty thousand `RemoveSubtree`
//! changes — and a commit lands once a second, so a few thousand of them are in
//! one batch. This measures what that batch costs with the write lock held,
//! which is what every search issued during it waits for.
//!
//! Reports the commit alone, so the number is comparable across runs of
//! different index sizes.

use std::time::Instant;

use scour_core::{Change, Entry, EntryId, Index, Meta, SourceId};
use scour_index_native::NativeIndex;

fn main() {
    let mut args = std::env::args().skip(1);
    let rows: usize = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);
    let dirs: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(2_000);

    let tmp = tempfile::tempdir().expect("tmpdir");
    let index = NativeIndex::open_or_create(tmp.path()).expect("create");

    // A corpus shaped like a real one: a wide tree of directories, most of the
    // index somewhere else entirely, and the removal touching only part of it.
    let build = Instant::now();
    let mut entries: Vec<Entry> = Vec::with_capacity(rows);
    for i in 0..rows {
        let d = i % dirs;
        let path = if i % 3 == 0 {
            format!("/corpus/node_modules/pkg{d}/lib/file{i}.js")
        } else {
            format!("/corpus/elsewhere/part{d}/file{i}.rs")
        };
        entries.push(Entry {
            id: EntryId::inode(SourceId(0), 66_310, i as u64),
            path,
            is_dir: false,
            meta: Meta {
                mtime: 1_700_000_000 + i as i64,
                size: 4096,
                ..Meta::UNKNOWN
            },
        });
    }
    for part in entries.chunks(200_000) {
        index
            .apply(&mut part.iter().cloned().map(Change::Upsert))
            .expect("apply");
        index.commit().expect("commit");
    }
    index
        .maintain(scour_core::Maintenance::Rebuild)
        .expect("rebuild");
    let stats = index.stats().expect("stats");
    println!(
        "{} rows in {} segment(s), built in {:.1?}\n",
        stats.entries,
        stats.segments,
        build.elapsed()
    );

    // The batches a delete really produces. `rm -rf` reports the leaves first,
    // so most of these are paths *under* other paths in the same batch.
    for batch in [1usize, 16, 256, 1_024, 4_096] {
        let mut removals: Vec<Change> = Vec::with_capacity(batch);
        for i in 0..batch {
            let d = i % dirs;
            removals.push(Change::RemoveSubtree {
                path: format!("/corpus/node_modules/pkg{d}/lib/file{i}.js"),
            });
        }
        index
            .apply(&mut removals.into_iter())
            .expect("apply removals");
        let t = Instant::now();
        index.commit().expect("commit");
        let took = t.elapsed();
        println!(
            "  {batch:>5} removed paths   commit {:>9.2?}   {:>8.1} µs a path",
            took,
            took.as_micros() as f64 / batch as f64
        );
    }

    // And the shape that should be cheap however large it is: one prefix that
    // covers the whole subtree, which is what the walk-driven path produces.
    index
        .apply(&mut std::iter::once(Change::RemoveSubtree {
            path: "/corpus/node_modules".into(),
        }))
        .expect("apply");
    let t = Instant::now();
    index.commit().expect("commit");
    println!(
        "\n  one prefix over the whole subtree   commit {:.2?}",
        t.elapsed()
    );
    println!("  {} rows left", index.stats().expect("stats").entries);
}
