//! Where a query's time actually goes, stage by stage, on a real segment.
//!
//! `cargo run --release -p scour-index-native --example innerloop <index-dir> [term]`
//!
//! The question this exists to answer is not "is it fast" but "**what is the
//! floor**". A search walks candidate rows and does three things to each one:
//! reads the name out of the arena, folds it, and looks for the term in it.
//! Everything else — the columns, the scoring, the page — is paid per *match*
//! rather than per candidate, and there are two orders of magnitude fewer
//! matches than candidates.
//!
//! So the four numbers below bracket the answer. If reading the names alone is
//! most of the total, the layout is the ceiling and no amount of clever
//! filtering helps. If folding is most of it, the fold is worth attacking —
//! and storing names already folded becomes an obvious trade.
//!
//! Point it at a **copy** of the index directory: the service holds a writer
//! lock on the real one, which is the whole point of that lock.

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
    //
    // What the query would cost if the arena held a folded copy. Built here in
    // memory rather than on disk, so the number says what the trade is worth
    // before anybody pays for it.
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
    //
    // The four above are the inner loop in isolation. This is the engine
    // around it: the plan, the trigram narrowing, the block walk, the
    // scoring and the page. The gap between them is the overhead, and it is
    // the number that says whether the loop or the machinery is the problem.
    let plan =
        scour_index_native::Plan::compile(&scour_query::parse_at(&term, 1_785_000_000), &seg)
            .expect("plan");
    let want = scour_index_native::Wanted {
        sort: scour_core::SortKey::Relevance,
        descending: true,
        offset: 0,
        limit: 200,
        count_cap: 100_000,
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
