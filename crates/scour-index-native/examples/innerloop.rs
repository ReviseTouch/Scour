//! Where a query's time actually goes, stage by stage, on a real segment.
//!
//! `cargo run --release -p scour-index-native --example innerloop <index-dir> [term]`
//!
//! A search reads each candidate's name, folds it and looks for the term; the
//! four numbers below bracket that floor. Point it at a **copy**, and read the
//! second run of each figure: the first pays a page fault per mapped page.

use std::time::Instant;

use scour_core::text::{DefaultFolder, Folder};
use scour_index_native::{Folded, Live};

fn main() {
    let dir = std::env::args().nth(1).expect("index dir");
    let term = std::env::args().nth(2).unwrap_or_else(|| "rapor".into());
    let dir = std::path::Path::new(&dir);

    // The largest segment, which on a settled index is the whole index.
    let mut best: Option<(u64, usize)> = None;
    for entry in std::fs::read_dir(dir).expect("read_dir").flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(rest) = name.strip_prefix("seg-") else {
            continue;
        };
        let Some(num) = rest.split('.').next().and_then(|n| n.parse::<u64>().ok()) else {
            continue;
        };
        let len = entry.metadata().map(|m| m.len() as usize).unwrap_or(0);
        if best.is_none_or(|(_, b)| len > b) && name.ends_with(".names") {
            best = Some((num, len));
        }
    }
    let (number, _) = best.expect("no segment in that directory");
    let live = Live::open(dir, number, 0).expect("open");
    let seg = live.view().expect("view");
    let rows = seg.rows();
    println!("segment {number}: {rows} rows\n");

    let needle = DefaultFolder.fold(&term);
    let finder = memchr::memmem::Finder::new(needle.as_bytes());

    // --- 1. read every name and nothing else ------------------------------
    let t = Instant::now();
    let mut bytes = 0usize;
    seg.names.walk(0, |_, name| {
        bytes += name.len();
        true
    });
    let read = t.elapsed();

    // --- 2. read and fold --------------------------------------------------
    let mut fold = Folded::new();
    let t = Instant::now();
    let mut folded_bytes = 0usize;
    seg.names.walk(0, |_, name| {
        folded_bytes += fold.fold_bytes(name).len();
        true
    });
    let folded = t.elapsed();

    // --- 3. read, fold, and search ----------------------------------------
    let t = Instant::now();
    let mut hits = 0usize;
    seg.names.walk(0, |_, name| {
        if finder.find(fold.fold_bytes(name)).is_some() {
            hits += 1;
        }
        true
    });
    let searched = t.elapsed();

    // --- 4. the same search on names that were already folded -------------
    // What the query would cost with a folded arena, built here in memory.
    let mut flat: Vec<u8> = Vec::with_capacity(bytes + rows);
    let mut ends: Vec<u32> = Vec::with_capacity(rows);
    seg.names.walk(0, |_, name| {
        flat.extend_from_slice(fold.fold_bytes(name));
        ends.push(flat.len() as u32);
        true
    });
    let t = Instant::now();
    let mut prefolded_hits = 0usize;
    let mut from = 0usize;
    for &end in &ends {
        if finder.find(&flat[from..end as usize]).is_some() {
            prefolded_hits += 1;
        }
        from = end as usize;
    }
    let prefolded = t.elapsed();

    // --- 5. what `run` actually costs, same segment, same term -----------
    // The engine around the loop: plan, trigram narrowing, block walk, scoring
    // and page. The gap from the four above is the overhead.
    let plan =
        scour_index_native::Plan::compile(&scour_query::parse_at(&term, 1_785_000_000), &seg)
            .expect("plan");
    let want = scour_index_native::Wanted {
        sort: scour_core::SortKey::Relevance,
        descending: true,
        offset: 0,
        limit: 200,
        count_cap: 100_000,
        rank_only: false,
    };
    let t = Instant::now();
    let found = scour_index_native::run(&seg, &plan, want);
    let whole = t.elapsed();
    println!(
        "  {:<28} {:>8.1?}  {:>6.1} ns a *visited* row  ({} visited, {} matched)\n",
        "run(), relevance, 200 rows",
        whole,
        whole.as_nanos() as f64 / found.rows_visited.max(1) as f64,
        found.rows_visited,
        found.total
    );
    let t = Instant::now();
    let found2 = scour_index_native::run(
        &seg,
        &plan,
        scour_index_native::Wanted {
            sort: scour_core::SortKey::Modified,
            ..want
        },
    );
    println!(
        "  {:<28} {:>8.1?}  {:>6.1} ns a *visited* row  ({} visited)",
        "run(), stored order",
        t.elapsed(),
        t.elapsed().as_nanos() as f64 / found2.rows_visited.max(1) as f64,
        found2.rows_visited
    );

    // --- 5b. the same walk under other full-walk sorts ---------------------
    // Size and name pay the same walk without paying `relevance`, which makes
    // the three a subtraction rather than a guess.
    for (label, key) in [
        ("run(), by size", scour_core::SortKey::Size),
        ("run(), by name", scour_core::SortKey::Name),
        ("run(), by kind", scour_core::SortKey::Kind),
    ] {
        let t = Instant::now();
        let f2 = scour_index_native::run(
            &seg,
            &plan,
            scour_index_native::Wanted { sort: key, ..want },
        );
        println!(
            "  {label:<28} {:>8.1?}  {:>6.1} ns a *visited* row",
            t.elapsed(),
            t.elapsed().as_nanos() as f64 / f2.rows_visited.max(1) as f64
        );
    }

    // --- 6. the candidate rows, with nothing but the needle ---------------
    // The same block set walked by hand with only the substring test; what is
    // left against `run` is the alive bit, clause dispatch, scoring and page.
    let mut candidate_rows = 0usize;
    let t = Instant::now();
    let mut bare_hits = 0usize;
    if let Some(cand) = plan.candidate_blocks() {
        let mut i = 0usize;
        while i < cand.len() {
            let mut j = i;
            while j + 1 < cand.len() && cand[j + 1] == cand[j] + 1 {
                j += 1;
            }
            let lo = cand[i] as usize * 128;
            let hi = ((cand[j] as usize + 1) * 128).min(rows);
            seg.folded.walk_range(lo, hi, |_, name| {
                candidate_rows += 1;
                if finder.find(name).is_some() {
                    bare_hits += 1;
                }
                true
            });
            i = j + 1;
        }
    }
    let bare = t.elapsed();
    if candidate_rows > 0 {
        println!(
            "  {:<28} {:>8.1?}  {:>6.1} ns a candidate row  ({candidate_rows} of them, {bare_hits} hit)",
            "candidates, needle only",
            bare,
            bare.as_nanos() as f64 / candidate_rows as f64
        );
    }

    let per = |d: std::time::Duration| d.as_nanos() as f64 / rows as f64;
    println!(
        "  {:<28} {:>8.1?}  {:>6.1} ns a row",
        "read the names",
        read,
        per(read)
    );
    println!(
        "  {:<28} {:>8.1?}  {:>6.1} ns  (+{:.1})",
        "read and fold",
        folded,
        per(folded),
        per(folded) - per(read)
    );
    println!(
        "  {:<28} {:>8.1?}  {:>6.1} ns  (+{:.1})",
        "read, fold, search",
        searched,
        per(searched),
        per(searched) - per(folded)
    );
    println!(
        "  {:<28} {:>8.1?}  {:>6.1} ns   <- if names were stored folded",
        "search only",
        prefolded,
        per(prefolded)
    );
    println!(
        "\n  {} bytes of names, {} folded, {hits} rows contain {term:?} ({prefolded_hits} the other way)",
        bytes, folded_bytes
    );
}
