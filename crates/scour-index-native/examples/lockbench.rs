//! Temporary research harness, not committed: search latency with and without
//! a writer committing once a second, on a copy of an index.
//!
//!   lockbench <index/native> <query>

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use scour_core::{Change, Entry, EntryId, Index, Meta, Page, SearchRequest, SortKey, SourceId};
use scour_index_native::NativeIndex;

fn searches(index: &NativeIndex, query: &str, n: usize) -> Vec<f64> {
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let began = Instant::now();
        let res = index
            .search(&SearchRequest {
                query: scour_query::parse(query),
                sort: SortKey::Modified,
                descending: true,
                page: Page {
                    offset: 0,
                    limit: 200,
                    count_cap: 1_000,
                },
            })
            .expect("search");
        std::hint::black_box(res.hits.len());
        out.push(began.elapsed().as_secs_f64() * 1000.0);
        std::thread::sleep(Duration::from_millis(40));
    }
    out
}

fn summary(label: &str, mut v: Vec<f64>) {
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let q = |p: f64| v[((v.len() - 1) as f64 * p) as usize];
    let slow = v.iter().filter(|&&x| x > 20.0).count();
    println!(
        "{label:<28} p50 {:7.2}  p90 {:7.2}  p99 {:7.2}  max {:7.2} ms   >20 ms: {slow}/{}",
        q(0.5),
        q(0.9),
        q(0.99),
        v[v.len() - 1],
        v.len()
    );
}

fn one(index: &NativeIndex, query: &str) -> (f64, Vec<scour_core::Hit>) {
    let began = Instant::now();
    let res = index
        .search(&SearchRequest {
            query: scour_query::parse(query),
            sort: SortKey::Modified,
            descending: true,
            page: Page { offset: 0, limit: 200, count_cap: 1_000 },
        })
        .expect("search");
    let paths: Vec<String> = res.hits.iter().filter(|h| h.is_dir).map(|h| h.path.clone()).collect();
    let sized = Instant::now();
    std::hint::black_box(index.subtree_sizes(&paths).expect("sizes"));
    let _ = sized;
    (began.elapsed().as_secs_f64() * 1000.0, res.hits)
}

/// A folder page after one row of the big segment changes: the first pays for
/// the folder-size table, the second does not.
fn sizes(dir: &str) {
    let index = NativeIndex::open_or_create(std::path::Path::new(dir)).expect("open");
    // The oldest file, which is in the big segment and nowhere else.
    let files = index
        .search(&SearchRequest {
            query: scour_query::parse("kind:file"),
            sort: SortKey::Modified,
            descending: false,
            page: Page { offset: 0, limit: 5, count_cap: 1_000 },
        })
        .expect("search")
        .hits;
    for _ in 0..2 { one(&index, "kind:folder"); }
    let (warm, _) = one(&index, "kind:folder");
    println!("folder page, warm: {warm:.1} ms");
    let victim = files.iter().rev().find(|h| !h.is_dir).expect("a file").clone();
    for round in 0..5 {
        let mut meta = victim.meta;
        meta.mtime += round + 1;
        let began = Instant::now();
        index.apply(&mut std::iter::once(Change::Upsert(Entry { id: victim.id.clone(), path: victim.path.clone(), is_dir: false, meta }))).expect("apply");
        index.commit().expect("commit");
        let commit = began.elapsed().as_secs_f64() * 1000.0;
        let (first, _) = one(&index, "kind:folder");
        let (second, _) = one(&index, "kind:folder");
        let (nofolder, _) = one(&index, "kind:file fatura");
        println!("round {round}: commit {commit:6.1} ms · folder page first {first:7.1} ms · again {second:6.1} ms · a page with no folder {nofolder:5.1} ms");
    }
}

/// Costly query shapes, least of three, on whatever the index holds now.
fn classes(index: &NativeIndex, label: &str) {
    let shapes = [
        ("a", SortKey::Modified, true),
        ("e", SortKey::Name, false),
        ("fatura", SortKey::Modified, true),
        ("rapor", SortKey::Name, false),
        ("rapor", SortKey::Path, false),
        ("*.rs", SortKey::Modified, true),
        ("ext:pdf", SortKey::Size, true),
        ("kind:image", SortKey::Modified, true),
        ("regex:^[0-9]{8}", SortKey::Modified, true),
        ("/src/ main", SortKey::Modified, true),
        ("", SortKey::Name, false),
        ("", SortKey::Path, false),
        ("dm:7d", SortKey::Size, true),
        ("size:>100mb", SortKey::Size, true),
    ];
    println!("-- {label}");
    for (q, sort, desc) in shapes {
        let mut best = f64::MAX;
        let mut total = 0;
        for _ in 0..3 {
            let began = Instant::now();
            let res = index
                .search(&SearchRequest {
                    query: scour_query::parse(q),
                    sort,
                    descending: desc,
                    page: Page { offset: 0, limit: 200, count_cap: 1_000 },
                })
                .expect("search");
            best = best.min(began.elapsed().as_secs_f64() * 1000.0);
            total = res.total;
        }
        println!("  {best:8.1} ms  {q:<18} {sort:?}{}  ({total})", if desc { " desc" } else { "" });
    }
}

/// A subtree sweep's cost, as a walk of one small folder ends: the rows under it
/// that the walk did not see go, and the time is what the write lock is held for.
fn sweeps(dir: &str) {
    let index = NativeIndex::open_or_create(std::path::Path::new(dir)).expect("open");
    let folders = index
        .search(&SearchRequest {
            query: scour_query::parse("kind:folder /home/"),
            sort: SortKey::Modified,
            descending: true,
            page: Page { offset: 0, limit: 200, count_cap: 1_000 },
        })
        .expect("search")
        .hits;
    let mut times = Vec::new();
    for hit in folders.iter().step_by(20).take(8) {
        let generation = index.begin_generation().expect("generation");
        let began = Instant::now();
        let cpu0 = thread_cpu();
        let gone = index
            .sweep(SourceId(0), std::slice::from_ref(&hit.path), generation, &scour_core::PrefixSet::default())
            .expect("sweep");
        let ms = began.elapsed().as_secs_f64() * 1000.0;
        let cpu = (thread_cpu() - cpu0) * 1000.0;
        println!("  sweep {ms:8.2} ms  CPU {cpu:7.2} ms  gone {gone:5}  {}", hit.path);
        times.push(ms);
    }
    summary("subtree sweeps", times);
}

fn main() {
    if std::env::args().nth(2).as_deref() == Some("--spare") {
        // A walk that finds a million rows unchanged, as a startup's does.
        let dir = std::env::args().nth(1).expect("dir");
        let index = NativeIndex::open_or_create(std::path::Path::new(&dir)).expect("open");
        let mut entries = Vec::new();
        index
            .scan(&scour_core::ScanRequest { query: scour_query::parse("") }, &mut |h: &scour_core::Hit| {
                entries.push(Entry { id: h.id.clone(), path: h.path.clone(), is_dir: h.is_dir, meta: h.meta });
                entries.len() < 1_000_000
            })
            .expect("scan");
        let n = entries.len();
        let generation = index.begin_generation().expect("generation");
        let _ = generation;
        let began = Instant::now();
        let cpu0 = thread_cpu();
        index.apply(&mut entries.into_iter().map(Change::Upsert)).expect("apply");
        index.commit().expect("commit");
        let before = index.stats().expect("stats");
        println!(
            "{n} unchanged rows: {:.0} ms wall, {:.0} ms CPU · {} rows, {} segments afterwards",
            began.elapsed().as_secs_f64() * 1000.0,
            (thread_cpu() - cpu0) * 1000.0,
            before.entries,
            before.segments
        );
        return;
    }
    if std::env::args().nth(2).as_deref() == Some("--shapes") {
        let dir = std::env::args().nth(1).expect("dir");
        let index = NativeIndex::open_or_create(std::path::Path::new(&dir)).expect("open");
        return classes(&index, "shapes");
    }
    if std::env::args().nth(2).as_deref() == Some("--sweep") {
        return sweeps(&std::env::args().nth(1).expect("dir"));
    }
    if std::env::args().nth(2).as_deref() == Some("--classes") {
        let dir = std::env::args().nth(1).expect("dir");
        let index = NativeIndex::open_or_create(std::path::Path::new(&dir)).expect("open");
        let st = index.stats().expect("stats");
        classes(&index, &format!("as it is: {} rows, {} segments, {} unsorted", st.entries, st.segments, st.unsorted_entries));
        let began = Instant::now();
        index.maintain(scour_core::Maintenance::Rebuild).expect("rebuild");
        let st = index.stats().expect("stats");
        println!("rebuild took {:.1} s", began.elapsed().as_secs_f64());
        classes(&index, &format!("rebuilt: {} segments, {} unsorted", st.segments, st.unsorted_entries));
        return;
    }
    if std::env::args().nth(2).as_deref() == Some("--sizes") {
        return sizes(&std::env::args().nth(1).expect("dir"));
    }
    let dir = std::env::args().nth(1).expect("index directory");
    let query = std::env::args().nth(2).unwrap_or_else(|| "fatura".into());
    let index = Arc::new(NativeIndex::open_or_create(std::path::Path::new(&dir)).expect("open"));
    searches(&index, &query, 5);
    summary("no writer", searches(&index, &query, 150));

    let stop = Arc::new(AtomicBool::new(false));
    let writer = {
        let index = index.clone();
        let stop = stop.clone();
        std::thread::spawn(move || {
            let mut commits = Vec::new();
            let mut round = 0u64;
            while !stop.load(Ordering::Relaxed) {
                let changes: Vec<Change> = (0..20)
                    .map(|i| {
                        let path = format!("/home/lockbench/build/out-{i}.o");
                        let mut meta = Meta::UNKNOWN;
                        meta.mtime = 1_790_000_000 + round as i64;
                        meta.size = 4096;
                        Change::Upsert(Entry {
                            id: EntryId::path_hash(SourceId(0), &path),
                            path,
                            is_dir: false,
                            meta,
                        })
                    })
                    .collect();
                let began = Instant::now();
                index.apply(&mut changes.into_iter()).expect("apply");
                index.commit().expect("commit");
                commits.push(began.elapsed().as_secs_f64() * 1000.0);
                round += 1;
                std::thread::sleep(Duration::from_millis(1_000));
            }
            commits
        })
    };
    summary("writer, 1 commit/s", searches(&index, &query, 150));
    stop.store(true, Ordering::Relaxed);
    let commits = writer.join().expect("writer");
    summary("  (the commits themselves)", commits);
}

fn thread_cpu() -> f64 {
    let mut t = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut t) };
    t.tv_sec as f64 + t.tv_nsec as f64 / 1e9
}
