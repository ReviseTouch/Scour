//! What a commit costs when it deletes one file out of a large index.
//!
//! The number is a *slope*, not a point: run it at two sizes to see whether
//! recording one deletion grows with how much is indexed.
//!
//!   cargo run --release -p scour-index-native --example alive -- [rows] [commits]

use std::time::Instant;

use scour_core::{Change, Entry, EntryId, Index, Meta, SourceId};
use scour_index_native::NativeIndex;

/// One row. The mtime is not decoration: rows are stored newest-first, so a
/// shared timestamp falls back to path order and clusters each directory into
/// a few blocks. `SCOUR_BENCH_SCATTER=1` spreads a directory over every block.
fn entry(i: usize, scatter: bool) -> Entry {
    let path = format!("/corpus/{:04}/file{i:08}.txt", i % 4096);
    let mut meta = Meta::UNKNOWN;
    if scatter {
        // A cheap decorrelated shuffle: consecutive paths get distant times.
        meta.mtime = (i as i64).wrapping_mul(2_654_435_761) & 0x7fff_ffff;
    }
    Entry {
        id: EntryId::path_hash(SourceId(0), &path),
        path,
        is_dir: false,
        meta,
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let rows: usize = args
        .next()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1_000_000);
    let commits: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(50);

    let scatter = std::env::var_os("SCOUR_BENCH_SCATTER").is_some();
    println!(
        "mtime      : {}",
        if scatter {
            "dagitik (gercekci)"
        } else {
            "esit (yol sirali)"
        }
    );
    // Where the index sits decides what is measured: `tempdir` lands on tmpfs,
    // where an `fsync` costs nothing — right for anything CPU-bound and wrong
    // for the commit clock. `SCOUR_BENCH_DIR` puts it on a real filesystem.
    let held;
    let dir: &std::path::Path = match std::env::var_os("SCOUR_BENCH_DIR") {
        Some(d) => {
            held = std::path::PathBuf::from(d);
            let _ = std::fs::remove_dir_all(&held);
            std::fs::create_dir_all(&held).expect("mkdir");
            println!("indeks     : {} (gercek disk)", held.display());
            &held
        }
        None => {
            let t = tempfile::tempdir().expect("tempdir");
            println!("indeks     : {} (tmpfs — fsync bedava)", t.path().display());
            held = t.keep();
            &held
        }
    };
    let index = NativeIndex::open_or_create(dir).expect("open");

    let t = Instant::now();
    let mut it = (0..rows).map(|i| Change::Upsert(entry(i, scatter)));
    index.apply(&mut it).expect("apply");
    index.commit().expect("commit");
    let built = t.elapsed();

    let stats = index.stats().expect("stats");
    let bitmap = stats.entries.div_ceil(8);
    println!("kuruldu    : {rows} satir, {built:.2?}");
    println!("canlilik   : {} bayt ({} satir)", bitmap, stats.entries);

    // What a start-up costs when nothing changed: the walk hands the index
    // every entry it saw, so a rescan of an untouched filesystem rewrites it.
    let _g = index.begin_generation().expect("generation");
    let t = Instant::now();
    let mut again = (0..rows).map(|i| Change::Upsert(entry(i, scatter)));
    let rep = index.apply(&mut again).expect("reapply");
    let applied = t.elapsed();
    println!("degismeyen : {} / {}", rep.unchanged, rep.seen());
    let t = Instant::now();
    index.commit().expect("commit");
    let committed = t.elapsed();
    println!("yeniden    : apply {applied:.2?}, commit {committed:.2?}");
    let st = index.stats().expect("stats");
    println!(
        "segment    : {} · sirasiz {}",
        st.segments, st.unsorted_entries
    );

    // Two kinds of commit, the same size, so the difference is the work and not
    // the write. An upsert of an existing path and a removal each change a row.
    let timed = |what: &str, mut make: Box<dyn FnMut(usize) -> Change>| {
        let mut worst = std::time::Duration::ZERO;
        let t = Instant::now();
        for c in 0..commits {
            let mut one = std::iter::once(make(c));
            index.apply(&mut one).expect("apply");
            let at = Instant::now();
            index.commit().expect("commit");
            worst = worst.max(at.elapsed());
        }
        let total = t.elapsed();
        println!(
            "{what:<10} : ortalama {:.3?}, en kotu {worst:.3?}",
            total / commits as u32
        );
    };

    timed(
        "upsert",
        Box::new(|c| Change::Upsert(entry(rows + c, scatter))),
    );
    timed(
        "remove",
        Box::new(|c| Change::RemoveSubtree {
            path: format!("/corpus/{:04}/file{:08}.txt", c % 4096, c),
        }),
    );
    // The shape block skipping cannot help: a whole directory, whose rows are
    // spread over every block because rows are stored newest-first.
    timed(
        "subtree",
        Box::new(|c| Change::RemoveSubtree {
            path: format!("/corpus/{:04}", 1000 + c),
        }),
    );
}
