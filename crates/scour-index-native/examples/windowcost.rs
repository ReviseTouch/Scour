//! What a window costs the service as it opens on the empty query: the
//! narrowing line's facets and usage, the rail's facets, and a first page by
//! size. Each one from released pages, with what it read and what it held, and
//! its answer printed so two builds can be diffed. Use a copy — opening takes
//! the writer lock:
//!
//!   cargo run --release -p scour-index-native --example windowcost -- /var/tmp/idx/native /home/hasan

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use scour_core::{FacetBy, FacetRequest, Index, Page, SearchRequest, SortKey, UsageRequest};
use scour_index_native::NativeIndex;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let dir = args.next().ok_or("usage: windowcost <index copy> <home>")?;
    let home = args.next().ok_or("usage: windowcost <index copy> <home>")?;
    let index = NativeIndex::open_or_create(std::path::Path::new(&dir))?;
    let empty = scour_query::parse("");

    for round in 0..2 {
        println!("round {round}");
        measure("narrow facets: kind, ext 6, age 7", &index, || {
            let f = index.facets(&FacetRequest {
                query: empty.clone(),
                by: vec![
                    FacetBy::Kind,
                    FacetBy::Ext { top: 6 },
                    FacetBy::Age { edges: vec![7] },
                ],
            })?;
            Ok(format!("{} rows {:?}", f.total, f.groups))
        })?;
        measure("narrow usage: home, top 8", &index, || {
            let u = index.usage(&UsageRequest {
                path: home.clone(),
                top: 8,
                query: empty.clone(),
            })?;
            Ok(format!(
                "{:?} {} children {:?}",
                u.root, u.child_count, u.children
            ))
        })?;
        measure("narrow usage: everything, top 8", &index, || {
            let u = index.usage(&UsageRequest {
                path: String::new(),
                top: 8,
                query: empty.clone(),
            })?;
            Ok(format!(
                "{:?} {} children {:?}",
                u.root, u.child_count, u.children
            ))
        })?;
        measure("narrow usage: home, `rapor`", &index, || {
            let u = index.usage(&UsageRequest {
                path: home.clone(),
                top: 8,
                query: scour_query::parse("rapor"),
            })?;
            Ok(format!(
                "{:?} {} children {:?}",
                u.root, u.child_count, u.children
            ))
        })?;
        measure("rail facets: kind, age 1 7 30 365", &index, || {
            let f = index.facets(&FacetRequest {
                query: empty.clone(),
                by: vec![
                    FacetBy::Kind,
                    FacetBy::Age {
                        edges: vec![1, 7, 30, 365],
                    },
                ],
            })?;
            Ok(format!("{} rows {:?}", f.total, f.groups))
        })?;
        measure("facets `rapor`: kind, ext 6, age 7", &index, || {
            let f = index.facets(&FacetRequest {
                query: scour_query::parse("rapor"),
                by: vec![
                    FacetBy::Kind,
                    FacetBy::Ext { top: 6 },
                    FacetBy::Age { edges: vec![7] },
                ],
            })?;
            Ok(format!("{} rows {:?}", f.total, f.groups))
        })?;
        // A page shows folders, and the window weighs them: that builds the
        // prefix sums the size order then reads its folders' totals from.
        measure("folder sizes for a page", &index, || {
            let paths = vec![home.clone(), "/mnt/depo".to_owned(), "/usr".to_owned()];
            Ok(format!("{:?}", index.subtree_sizes(&paths)?))
        })?;
        measure("first page by size", &index, || {
            let r = index.search(&SearchRequest {
                query: empty.clone(),
                sort: SortKey::Size,
                descending: true,
                page: Page::new(0, 200),
            })?;
            Ok(r.hits
                .iter()
                .map(|h| format!("{} {}", h.path, h.meta.size))
                .collect::<Vec<_>>()
                .join("\n"))
        })?;
    }
    Ok(())
}

/// Run one request from released pages and print what it took; the answer
/// goes to stdout after a `=` line so a diff of two builds shows any change.
fn measure(
    what: &str,
    index: &NativeIndex,
    run: impl FnOnce() -> scour_core::Result<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Not before the size page: a release takes the folder table with it now,
    // and the page is measured as a window meets it, after the folder sizes.
    if what != "first page by size" {
        index.release_memory();
    }
    let before = memory();
    let running = Arc::new(AtomicBool::new(true));
    let peak_anon = Arc::new(AtomicU64::new(before.1));
    let peak_file = Arc::new(AtomicU64::new(before.0 - before.1));
    let sampler = {
        let (running, peak_anon, peak_file) = (
            Arc::clone(&running),
            Arc::clone(&peak_anon),
            Arc::clone(&peak_file),
        );
        std::thread::spawn(move || {
            while running.load(Ordering::Relaxed) {
                let (rss, anon) = memory();
                peak_anon.fetch_max(anon, Ordering::Relaxed);
                peak_file.fetch_max(rss.saturating_sub(anon), Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(2));
            }
        })
    };
    let wall = Instant::now();
    let answer = run()?;
    let wall = wall.elapsed();
    running.store(false, Ordering::Relaxed);
    let _ = sampler.join();
    let after = memory();
    eprintln!(
        "{what:38} {:8.1} ms · anonymous {:4} → peak {:4} → {:4} MiB · file pages {:4} → peak {:4} MiB",
        wall.as_secs_f64() * 1_000.0,
        before.1 / 1024,
        peak_anon.load(Ordering::Relaxed) / 1024,
        after.1 / 1024,
        (before.0 - before.1) / 1024,
        peak_file.load(Ordering::Relaxed) / 1024,
    );
    println!("= {what}\n{answer}");
    Ok(())
}

/// Resident and anonymous KiB.
fn memory() -> (u64, u64) {
    let text = std::fs::read_to_string("/proc/self/smaps_rollup").unwrap_or_default();
    (value(&text, "Rss:"), value(&text, "Anonymous:"))
}

fn value(text: &str, key: &str) -> u64 {
    text.lines()
        .find(|line| line.starts_with(key))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}
