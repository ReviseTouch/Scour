//! What a commit costs when it deletes one file out of a large index.
//!
//! The number this exists to produce is a *slope*, not a point: the old
//! write-back copied and rewrote the whole live bitmap, so the cost of
//! recording one deletion grew with how much had been indexed rather than with
//! how much had changed. Run it at two sizes and the shape is the answer.
//!
//!   cargo run --release -p scour-index-native --example alive -- [rows] [commits]

use std::time::Instant;

use scour_core::{Change, Entry, EntryId, Index, Meta, SourceId};
use scour_index_native::NativeIndex;

/// One row.
///
/// **The mtime is not decoration.** Rows are stored newest-first, so entries
/// that all share a timestamp fall back to path order — and a path-ordered
/// index clusters every directory's rows into a few blocks, which is the best
/// case for anything that skips blocks by directory number and is not what a
/// real index looks like. `SCOUR_BENCH_SCATTER=1` gives each row an unrelated
/// mtime, which is the real shape: one directory's rows spread over every
/// block.
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
    // **Where the index sits decides what is being measured.** `tempdir` lands
    // in `/tmp`, which is tmpfs here, and an `fsync` to memory costs nothing —
    // so a commit measured there is the work and not the write. That is the
    // right choice for anything CPU-bound and the wrong one for the commit
    // clock, which is `fsync` almost all the way down. `SCOUR_BENCH_DIR` puts
    // it on a real filesystem.
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

    // **What a start-up costs when nothing changed.** The walk hands the
    // index every entry it saw, unchanged or not, so a rescan of an untouched
    // filesystem writes the whole index again. This is the number any fix has
    // to beat.
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

    // Two kinds of commit, the same size, so the difference is the work and
    // not the write. An upsert of a path that is already there and a removal
    // of one file both change exactly one row.
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
    // The other shape of the same call, and the one block skipping cannot
    // help: a whole directory, whose rows are spread over every block because
    // rows are stored newest-first rather than by directory. If the skip costs
    // anything, it costs it here.
    timed(
        "subtree",
        Box::new(|c| Change::RemoveSubtree {
            path: format!("/corpus/{:04}", 1000 + c),
        }),
    );
}
