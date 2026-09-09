//! Can a folder's size be a column in a list, or only a report?
//!
//! Directory numbers are in sorted path order, so a subtree is one or two
//! contiguous runs and a prefix sum over them is an O(1) total. This measures
//! what the prefix costs to build, what it weighs, what a lookup costs, and
//! whether it agrees with the rollup the report already trusts.
//!
//!   cargo run --release -p scour-index-native --example dirsize -- /tmp/idx/native

use std::time::Instant;

use scour_core::{Ast, Index, UsageRequest};
use scour_index_native::NativeIndex;

/// What one segment contributes, laid out for O(1) subtree answers.
struct Fast {
    /// Own totals by directory number, then prefix-summed: `pre[i]` covers
    /// directories `0..i`, with one extra slot so a range is a subtraction.
    disk: Vec<u64>,
    files: Vec<u64>,
}

fn main() {
    let dir = std::env::args().nth(1).expect("index directory (…/native)");
    let index = NativeIndex::open_or_create(std::path::Path::new(&dir)).expect("open");

    // ---- build ------------------------------------------------------------
    let began = Instant::now();
    let mut per_segment: Vec<Fast> = Vec::new();
    let mut rows_seen = 0u64;
    let mut dirs_total = 0usize;
    index
        .for_each_segment(&mut |_, seg| {
            let n = seg.dirs.len();
            let mut disk = vec![0u64; n + 1];
            let mut files = vec![0u64; n + 1];
            for row in 0..seg.rows() {
                if !seg.is_alive(row) || seg.num_of(scour_index_native::Field::IsDir, row) != 0 {
                    continue;
                }
                let d = seg.dir_id(row) as usize;
                if d >= n {
                    continue;
                }
                // A name's share of a file that may have several, exactly as
                // the report does it, or a hard-linked tree is counted twice.
                let links = seg.num_of(scour_index_native::Field::Links, row).max(1) as u64;
                disk[d] += seg.num_of(scour_index_native::Field::Disk, row).max(0) as u64 / links;
                files[d] += 1;
                rows_seen += 1;
            }
            // In place: `disk[i]` becomes everything strictly before `i`.
            let mut run = 0u64;
            for v in disk.iter_mut() {
                let own = *v;
                *v = run;
                run += own;
            }
            let mut run = 0u64;
            for v in files.iter_mut() {
                let own = *v;
                *v = run;
                run += own;
            }
            dirs_total += n;
            per_segment.push(Fast { disk, files });
        })
        .expect("segments");
    let built = began.elapsed();

    let bytes: usize = per_segment
        .iter()
        .map(|f| (f.disk.len() + f.files.len()) * 8)
        .sum();
    println!(
        "{} segment · {} dizin · {} canli satir",
        per_segment.len(),
        dirs_total,
        rows_seen
    );
    println!(
        "kurulum {:.1?} · bellek {:.1} MB",
        built,
        bytes as f64 / 1_048_576.0
    );

    // ---- one lookup -------------------------------------------------------
    // Two binary searches a segment for the ranges, then arithmetic.
    let ask = |path: &str| -> (u64, u64) {
        let (mut disk, mut files) = (0u64, 0u64);
        let mut at = 0usize;
        index
            .for_each_segment(&mut |_, seg| {
                let f = &per_segment[at];
                at += 1;
                let scope = seg.dirs.subtree(path);
                if let Some(own) = scope.own {
                    let i = own as usize;
                    disk += f.disk[i + 1] - f.disk[i];
                    files += f.files[i + 1] - f.files[i];
                }
                let (s, e) = (scope.below.start as usize, scope.below.end as usize);
                if e > s {
                    disk += f.disk[e] - f.disk[s];
                    files += f.files[e] - f.files[s];
                }
            })
            .expect("segments");
        (disk, files)
    };

    // Something to ask about: every directory in the biggest segment, so the
    // timing is over real paths of every depth rather than a hand-picked few.
    let mut paths: Vec<String> = Vec::new();
    index
        .for_each_segment(&mut |_, seg| {
            if paths.is_empty() {
                for id in (0..seg.dirs.len() as u32).step_by(seg.dirs.len().max(1) / 500 + 1) {
                    if let Some(p) = seg.dirs.get(id) {
                        paths.push(p);
                    }
                }
            }
        })
        .expect("segments");

    let began = Instant::now();
    let mut sink = 0u64;
    for p in &paths {
        sink += ask(p).0;
    }
    let per = began.elapsed().as_nanos() as f64 / paths.len() as f64;
    println!(
        "{} dizin sorgusu · {:.1} µs/sorgu · sayfa basina (30 klasor) {:.2} ms  [{sink}]",
        paths.len(),
        per / 1000.0,
        per * 30.0 / 1_000_000.0
    );

    // Where the time goes: the arithmetic is three subtractions, so microseconds
    // are the binary search — `lower_bound` per probe, which every `under:`
    // search pays as well.
    let began = Instant::now();
    let mut probes = 0u64;
    for p in &paths {
        index
            .for_each_segment(&mut |_, seg| {
                let scope = seg.dirs.subtree(p);
                probes += scope.below.end as u64 - scope.below.start as u64;
            })
            .expect("segments");
    }
    let search_only = began.elapsed().as_nanos() as f64 / paths.len() as f64;
    println!(
        "  bunun {:.1} µs'i ikili arama ({:.0}%), gerisi aritmetik  [{probes}]",
        search_only / 1000.0,
        search_only / per * 100.0
    );

    // ---- does it agree with the report? -----------------------------------
    // The rollup in `usage.rs` is the trusted answer and this is a different
    // route to the same total; it has to land on it exactly.
    println!("\ndogrulama — rapor ile karsilastirma:");
    let mut checked = 0;
    let mut wrong = 0;
    for p in paths.iter().take(40) {
        let report = index
            .usage(&UsageRequest {
                path: p.clone(),
                top: 0,
                query: Ast::default(),
            })
            .expect("usage");
        let (disk, files) = ask(p);
        checked += 1;
        if report.root.disk != disk || report.root.files != files {
            wrong += 1;
            if wrong <= 5 {
                println!(
                    "  AYRIM {p}\n    rapor {} B / {} dosya\n    hizli {} B / {} dosya",
                    report.root.disk, report.root.files, disk, files
                );
            }
        }
    }
    println!("  {checked} dizin karsilastirildi, {wrong} ayrim");

    // ---- what the report costs, for comparison ----------------------------
    let began = Instant::now();
    let _ = index.usage(&UsageRequest {
        path: String::new(),
        top: 0,
        query: Ast::default(),
    });
    println!("\nrapor (tum disk, tek sorgu) {:.1?}", began.elapsed());
}
