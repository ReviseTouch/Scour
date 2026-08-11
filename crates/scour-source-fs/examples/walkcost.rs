//! What a walk costs before anything is indexed.
//!
//! A start-up scan of `/mnt/depo` was measured at 12.1 core-seconds on one
//! thread — 8 µs an entry, where `find` walks the same tree at 0.55. Almost
//! none of that can be the walk itself, so this splits the two: it runs the
//! real [`FsSource`] with the real rules into a sink that does nothing but
//! count, which leaves the directory reads, the rule tests, the path
//! normalisation and building an [`Entry`] — and no index at all.
//!
//!   cargo run --release -p scour-source-fs --example walkcost -- <root> [threads]
//!
//! `SCOUR_WALK_NOMETA=1` drops the per-entry `stat` as well, which is the other
//! half of the split.

use std::time::Instant;

use scour_core::{Entry, EntrySink, Flow, ScanOptions, Source, SourceId};
use scour_source_fs::FsSource;

/// Counts, and holds on to nothing. Anything this sink costs is the walk's.
#[derive(Default)]
struct Count {
    entries: u64,
    /// Summed so the entry cannot be optimised away unbuilt.
    bytes: u64,
}

impl EntrySink for Count {
    fn push(&mut self, entry: Entry) -> Flow {
        self.entries += 1;
        self.bytes += entry.path.len() as u64;
        Flow::Continue
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let root = args.next().unwrap_or_else(|| "/mnt/depo".into());
    let threads: usize = args.next().and_then(|s| s.parse().ok()).unwrap_or(1);
    let meta = std::env::var_os("SCOUR_WALK_NOMETA").is_none();

    // **The rules are not decoration.** With every list empty the walk skips
    // rule evaluation entirely — `rules.is_empty()` short-circuits it — so a
    // measurement taken that way is of a scan nobody runs. `SCOUR_WALK_NORULES`
    // takes them back out, which is how their share is read off.
    let rules = std::env::var_os("SCOUR_WALK_NORULES").is_none();
    let (def_paths, def_dirs, def_files) = scour_source_fs::platform_defaults();
    let source = FsSource::new(SourceId(0), "probe", vec![root.clone().into()]);
    let opts = ScanOptions {
        hidden: true,
        follow_symlinks: false,
        skip_metadata: !meta,
        threads,
        exclude_paths: if rules { def_paths } else { Vec::new() },
        exclude_dirs: if rules { def_dirs } else { Vec::new() },
        exclude_files: if rules { def_files } else { Vec::new() },
        allow: if rules {
            vec!["target/release".into(), "target/debug".into()]
        } else {
            Vec::new()
        },
        ..Default::default()
    };

    let mut sink = Count::default();
    let began = Instant::now();
    let report = source.scan(&opts, &mut sink);
    let took = began.elapsed();

    let n = sink.entries.max(1);
    println!(
        "{root}  threads {threads}  stat {}",
        if meta { "var" } else { "yok" }
    );
    println!(
        "  {} kayit, {:.2?}, kayit basina {:.2} us  ({} bayt yol)",
        sink.entries,
        took,
        took.as_secs_f64() * 1e6 / n as f64,
        sink.bytes,
    );
    if let Err(e) = report {
        println!("  uyari: {e}");
    }
}
