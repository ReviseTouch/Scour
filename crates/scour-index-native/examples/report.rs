//! What the disk-usage report costs, with a query and without one.
//!
//! The report used to walk every live row whatever was being asked, because it
//! had no way to be asked anything narrower than a folder. Giving
//! [`UsageRequest`] a query was expected to be free — the rollup then walks
//! what a *search* walks, which is less — and "expected to be free" is the
//! kind of claim this file exists to check.
//!
//! Two things are measured and both matter:
//!
//! * the **unfiltered** answer, which is the one the tab opens on and which
//!   must not have got slower;
//! * the **filtered** answers, against the number of rows each query matches,
//!   because that is the ratio the whole argument rests on.
//!
//! Read-only. Point it at a copy, so a live service is neither blocked nor
//! believed:
//!
//!   cp -a ~/.local/share/scour/index /tmp/idx
//!   cargo run --release -p scour-index-native --example report -- /tmp/idx/native [scope]

use std::time::Instant;

use scour_core::{Index, Page, SearchRequest, SortKey, UsageRequest};
use scour_index_native::NativeIndex;

/// Queries worth timing, chosen to span the shapes the planner handles: a word
/// (trigram-narrowed), an extension (read from the name), a kind (a column and
/// a zone map), a date (a column), and one that matches nothing.
const QUERIES: [&str; 6] = [
    "",
    "osman",
    "ext:rs",
    "kind:image",
    "dm:>1y",
    "ext:sokakyok",
];

fn main() {
    let dir = std::env::args().nth(1).expect("index directory (…/native)");
    let scope = std::env::args().nth(2).unwrap_or_default();
    let index = NativeIndex::open_or_create(std::path::Path::new(&dir)).expect("open");

    let stats = index.stats().expect("stats");
    println!(
        "{} rows · {} directories · {} segments\nscope: {}\n",
        stats.entries,
        stats.dirs,
        stats.segments,
        if scope.is_empty() {
            "(everything)"
        } else {
            &scope
        }
    );

    println!(
        "  {:>14}  {:>9}  {:>9}  {:>7}  query",
        "matched rows", "count", "report", "×"
    );
    let mut baseline = 0f64;
    for q in QUERIES {
        // How many rows the query matches, and what counting them costs — the
        // work the rollup now does instead of walking everything.
        let began = Instant::now();
        let found = index
            .search(&SearchRequest {
                query: scour_query::parse(q),
                sort: SortKey::Modified,
                descending: true,
                page: Page {
                    offset: 0,
                    limit: 0,
                    count_cap: u32::MAX,
                },
            })
            .expect("search");
        let counting = began.elapsed();

        // Three runs, least reported: the first pays for whatever of the map
        // is not resident, and a report is not a thing anybody runs cold twice.
        let mut best = f64::MAX;
        let mut answer = None;
        for _ in 0..3 {
            let began = Instant::now();
            let res = index
                .usage(&UsageRequest {
                    path: scope.clone(),
                    top: 5,
                    query: scour_query::parse(q),
                })
                .expect("usage");
            best = best.min(began.elapsed().as_secs_f64() * 1e3);
            answer = Some(res);
        }
        if q.is_empty() {
            baseline = best;
        }
        println!(
            "  {:>14}  {:>8.1?}  {:>8.1}ms  {:>6.1}×  {}",
            found.total,
            counting,
            best,
            baseline / best,
            if q.is_empty() { "(no filter)" } else { q }
        );

        // And what it answers, because a fast wrong number is worse than a
        // slow right one. The children have to come to the root: they are the
        // same tree the unfiltered report has, with the empty folders still in
        // it, so the sum is a real check rather than a tautology.
        if let Some(r) = answer {
            let kids: u64 = r.children.iter().map(|c| c.disk).sum();
            println!(
                "  {:>14}  {} over {} files, {} in the {} heaviest children",
                "",
                human(r.root.disk),
                r.root.files,
                human(kids),
                r.children.len()
            );
        }
    }
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
