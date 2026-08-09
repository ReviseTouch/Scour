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

fn entry(i: usize) -> Entry {
    let path = format!("/corpus/{:04}/file{i:08}.txt", i % 4096);
    Entry {
        id: EntryId::path_hash(SourceId(0), &path),
        path,
        is_dir: false,
        meta: Meta::UNKNOWN,
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let rows: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(1_000_000);
    let commits: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(50);

    let dir = tempfile::tempdir().expect("tempdir");
    let index = NativeIndex::open_or_create(dir.path()).expect("open");

    let t = Instant::now();
    let mut it = (0..rows).map(|i| Change::Upsert(entry(i)));
    index.apply(&mut it).expect("apply");
    index.commit().expect("commit");
    let built = t.elapsed();

    let stats = index.stats().expect("stats");
    let bitmap = stats.entries.div_ceil(8);
    println!("kuruldu    : {rows} satir, {built:.2?}");
    println!("canlilik   : {} bayt ({} satir)", bitmap, stats.entries);

    // One deletion a commit, which is the case the old path was worst at: the
    // whole bitmap was copied and rewritten to record a single bit.
    let mut worst = std::time::Duration::ZERO;
    let t = Instant::now();
    for c in 0..commits {
        let path = format!("/corpus/{:04}/file{:08}.txt", c % 4096, c);
        let mut one = std::iter::once(Change::RemoveSubtree { path });
        index.apply(&mut one).expect("apply");
        let at = Instant::now();
        index.commit().expect("commit");
        worst = worst.max(at.elapsed());
    }
    let total = t.elapsed();

    println!(
        "commit     : {commits} adet, toplam {total:.2?}, ortalama {:.3?}, en kotu {worst:.3?}",
        total / commits as u32
    );
    println!(
        "satir basi : {:.1} bayt/commit yazilmis olurdu (eski yol: butun bitmap)",
        bitmap as f64
    );
}
