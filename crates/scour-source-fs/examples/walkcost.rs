//! What a walk costs before anything is indexed: the real [`FsSource`] with the
//! real rules into a sink that only counts, so what is left is directory reads,
//! rule tests, path normalisation and building an [`Entry`]. `SCOUR_WALK_NOMETA=1`
//! drops the per-entry `stat`. `cargo run --release --example walkcost -- <root> [n]`

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

    // **The rules are not decoration.** With every list empty the walk short-circuits
    // rule evaluation, so that measurement is of a scan nobody runs. `SCOUR_WALK_NORULES`
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
