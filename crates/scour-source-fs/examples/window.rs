//! What a wider collection window would deduplicate, on this machine.
//!
//! The fanotify reader collects events for [`WINDOW`] before it reads them, and
//! reduces whatever it collected to distinct paths — the same file written a
//! hundred times inside the window costs one `statx` and one `Upsert`. How much
//! that saves is a property of the machine's actual churn, not of the code, so
//! it has to be measured rather than argued.
//!
//! Diagnostic rather than test: point it at the real roots, leave it for a few
//! minutes, and it prints the deduplication ratio for a range of window lengths
//! **against one trace**. One trace and several windows rather than several
//! runs, because a desktop's churn differs more between two minutes than the
//! windows differ between themselves.
//!
//! It records through `Source::watch`, so what it sees is exactly what the
//! engine would have been sent. On Linux without the privileged helper that is
//! inotify, which emits once per event and no more — which is what makes the
//! trace a *raw* one and the replay below meaningful.
//!
//! ```text
//! cargo run --release --example window -- 300 /home/you
//! ```
//!
//! [`WINDOW`]: fanotify's collection window, 200 ms at the time of writing.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use scour_core::{Change, ChangeSink, ScanOptions, Source, SourceId};
use scour_source_fs::FsSource;

/// Every event, with the moment it arrived.
#[derive(Debug, Default)]
struct Trace(Mutex<Vec<(Instant, String)>>);

impl ChangeSink for Trace {
    fn emit(&self, change: Change) {
        let at = Instant::now();
        if let Ok(mut v) = self.0.lock() {
            v.push((at, change.path().to_owned()));
        }
    }
}

/// The windows worth asking about. The first is what the reader uses today;
/// the last two are the engine's own commit clocks, which is the comparison
/// that started this.
const WINDOWS: [Duration; 8] = [
    Duration::from_millis(200),
    Duration::from_millis(500),
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(15),
    Duration::from_secs(30),
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let secs: u64 = args
        .next()
        .ok_or("usage: window <seconds> <root>...")?
        .parse()?;
    let roots: Vec<String> = args.collect();
    if roots.is_empty() {
        return Err("at least one root is required".into());
    }

    let trace = Arc::new(Trace::default());
    let opts = ScanOptions::default();
    let mut handles = Vec::new();
    for (i, root) in roots.iter().enumerate() {
        let source = FsSource::new(SourceId(i as u32), root.clone(), vec![root.into()]);
        let sink = Arc::clone(&trace);
        handles.push(source.watch(&opts, Box::new(Forward(sink)))?);
        println!("watching {root}");
    }

    let started = Instant::now();
    std::thread::sleep(Duration::from_secs(secs));
    let events = trace
        .0
        .lock()
        .map_err(|_| "the trace was poisoned")?
        .clone();
    drop(handles);

    let elapsed = started.elapsed().as_secs_f64();
    let distinct: HashSet<&str> = events.iter().map(|(_, p)| p.as_str()).collect();
    println!(
        "\n{} events in {:.0} s ({:.1}/s) · {} distinct paths over the whole trace",
        events.len(),
        elapsed,
        events.len() as f64 / elapsed,
        distinct.len(),
    );

    println!(
        "\n{:>8}  {:>8}  {:>9}  {:>8}  {:>7}  {:>9}",
        "window", "windows", "looks", "looks/s", "saved", "max batch"
    );
    for w in WINDOWS {
        let r = replay(&events, w);
        println!(
            "{:>8}  {:>8}  {:>9}  {:>8.2}  {:>6.1}%  {:>9}",
            format!("{:?}", w),
            r.windows,
            r.looks,
            r.looks as f64 / elapsed,
            100.0 * (1.0 - r.looks as f64 / events.len().max(1) as f64),
            r.max_batch,
        );
    }

    println!("\nthe loudest paths");
    let mut by_path: HashMap<&str, usize> = HashMap::new();
    for (_, p) in &events {
        *by_path.entry(p.as_str()).or_default() += 1;
    }
    let mut loud: Vec<(&str, usize)> = by_path.into_iter().collect();
    loud.sort_unstable_by_key(|&(_, n)| std::cmp::Reverse(n));
    for (path, n) in loud.iter().take(15) {
        println!("{n:>7}  {path}");
    }

    println!("\nthe loudest directories");
    let mut by_dir: HashMap<&str, usize> = HashMap::new();
    for (_, p) in &events {
        let dir = p.rsplit_once('/').map_or("/", |(d, _)| d);
        *by_dir.entry(dir).or_default() += 1;
    }
    let mut dirs: Vec<(&str, usize)> = by_dir.into_iter().collect();
    dirs.sort_unstable_by_key(|&(_, n)| std::cmp::Reverse(n));
    for (dir, n) in dirs.iter().take(15) {
        println!("{n:>7}  {dir}");
    }
    Ok(())
}

#[derive(Debug, Default)]
struct Replay {
    windows: usize,
    /// How many `crate::watch::look` calls the window would have produced —
    /// one `statx` and one `Upsert` each.
    looks: usize,
    max_batch: usize,
}

/// The reader's loop, on a trace that has already happened.
///
/// A window opens when the first event after the last one arrives — `poll`
/// returns on it — and closes `w` later; everything inside becomes one set of
/// distinct paths. The processing between two windows is not modelled because
/// it is microseconds against a window of hundreds of milliseconds.
fn replay(events: &[(Instant, String)], w: Duration) -> Replay {
    let mut r = Replay::default();
    let mut seen: HashSet<&str> = HashSet::new();
    let mut i = 0usize;
    while i < events.len() {
        let end = events[i].0 + w;
        seen.clear();
        let mut j = i;
        while j < events.len() && events[j].0 < end {
            seen.insert(events[j].1.as_str());
            j += 1;
        }
        r.windows += 1;
        r.looks += seen.len();
        r.max_batch = r.max_batch.max(j - i);
        i = j;
    }
    r
}

/// `Source::watch` wants an owned sink and the trace is shared.
#[derive(Debug)]
struct Forward(Arc<Trace>);

impl ChangeSink for Forward {
    fn emit(&self, change: Change) {
        self.0.emit(change);
    }
}
