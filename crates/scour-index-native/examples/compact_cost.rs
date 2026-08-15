//! CPU, wall time and anonymous-memory peak of one native compaction.
//!
//! Diagnostic rather than a benchmark harness. It mutates the index passed to
//! it, so use a copy: compaction replaces eligible segment groups in place.
//!
//! A second argument of `rebuild` folds everything into one segment instead of
//! folding the eligible groups. That is the heavier of the two and the one
//! worth being able to price: it is what somebody with an existing index runs
//! to give it the current stored text orders, and it reads the index rather
//! than the filesystem.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use scour_core::{Index, Maintenance};
use scour_index_native::NativeIndex;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let dir = std::env::args_os()
        .nth(1)
        .ok_or("usage: compact_cost <index> [compact|rebuild]")?;
    let level = match std::env::args().nth(2).as_deref() {
        Some("rebuild") => Maintenance::Rebuild,
        _ => Maintenance::Compact,
    };
    let index = NativeIndex::open_or_create(std::path::Path::new(&dir))?;
    let before = memory();
    let running = Arc::new(AtomicBool::new(true));
    let peak = Arc::new(AtomicU64::new(before.1));
    let sampler = {
        let running = Arc::clone(&running);
        let peak = Arc::clone(&peak);
        std::thread::spawn(move || {
            while running.load(Ordering::Relaxed) {
                peak.fetch_max(memory().1, Ordering::Relaxed);
                std::thread::sleep(Duration::from_millis(50));
            }
        })
    };
    let wall = Instant::now();
    let cpu = thread_cpu();
    let report = index.maintain(level)?;
    let cpu = thread_cpu().saturating_sub(cpu);
    let wall = wall.elapsed();
    running.store(false, Ordering::Relaxed);
    let _ = sampler.join();
    let after = memory();
    println!(
        "{level:?}: {:.1} ms wall / {:.1} ms CPU · anonymous {:.1} MiB before, {:.1} MiB peak, {:.1} MiB after · {} segments",
        millis(wall),
        millis(cpu),
        before.1 as f64 / 1024.0,
        peak.load(Ordering::Relaxed) as f64 / 1024.0,
        after.1 as f64 / 1024.0,
        index.stats()?.segments,
    );
    let _ = report;
    Ok(())
}

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

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

#[cfg(target_os = "linux")]
fn thread_cpu() -> Duration {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) } == 0 {
        Duration::new(time.tv_sec as u64, time.tv_nsec as u32)
    } else {
        Duration::ZERO
    }
}

#[cfg(not(target_os = "linux"))]
fn thread_cpu() -> Duration {
    Duration::ZERO
}
