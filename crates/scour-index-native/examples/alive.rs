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
    let dir = tempfile::tempdir().expect("tempdir");
    let index = NativeIndex::open_or_create(dir.path()).expect("open");

    let t = Instant::now();
    let mut it = (0..rows).map(|i| Change::Upsert(entry(i, scatter)));
    index.apply(&mut it).expect("apply");
    index.commit().expect("commit");
    let built = t.elapsed();

    let stats = index.stats().expect("stats");
    let bitmap = stats.entries.div_ceil(8);
    println!("kuruldu    : {rows} satir, {built:.2?}");
    println!("canlilik   : {} bayt ({} satir)", bitmap, stats.entries);

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
