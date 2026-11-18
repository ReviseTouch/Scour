//! What a list of queries costs on one index, for two builds run turn about.
//! Read-only; point it at a copy. Each line of standard input is
//! `sort direction cap query`, and each is asked once to warm and then
//! `rounds` times; the median, the fastest and the slowest are printed with
//! the count and a digest of the page, so two builds can be diffed.
//!
//!   cargo run --release -p scour-index-native --example querycost -- /tmp/idx/native 7 < queries.txt

use std::hash::{Hash, Hasher};
use std::io::BufRead;
use std::time::Instant;

use scour_core::{Index, Page, SearchRequest};
use scour_index_native::NativeIndex;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args.next().expect("index directory (…/native)");
    let rounds: usize = args.next().and_then(|n| n.parse().ok()).unwrap_or(7);
    let index = NativeIndex::open_or_create(std::path::Path::new(&dir)).expect("open");
    for line in std::io::stdin().lock().lines() {
        let line = line.expect("a line");
        let mut parts = line.splitn(4, ' ');
        let (Some(sort), Some(direction), Some(cap)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let query = parts.next().unwrap_or("");
        let request = SearchRequest {
            query: scour_query::parse(query),
            sort: scour_core::SortKey::named(sort).expect("a sort key"),
            descending: direction == "desc",
            page: Page {
                offset: 0,
                limit: 40,
                count_cap: cap.parse().expect("a count cap"),
            },
        };
        let mut times: Vec<f64> = Vec::with_capacity(rounds);
        let mut last = index.search(&request).expect("search");
        for _ in 0..rounds {
            let started = Instant::now();
            last = index.search(&request).expect("search");
            times.push(started.elapsed().as_secs_f64() * 1e3);
        }
        times.sort_by(f64::total_cmp);
        let mut digest = std::collections::hash_map::DefaultHasher::new();
        for hit in &last.hits {
            hit.path.hash(&mut digest);
        }
        println!(
            "{:8.2} ms ({:.2}–{:.2}) {:>8}{} rows {:>8} read {:016x}  {sort} {query}",
            times[times.len() / 2],
            times[0],
            times[times.len() - 1],
            last.total,
            if last.capped { "+" } else { " " },
            last.rows_visited,
            digest.finish(),
        );
    }
}
