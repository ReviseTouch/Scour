//! Which rule costs what.
//!
//! A good part of a rescan's walk is the rule tests, and measuring them through
//! a whole walk could not say which rule: the first fix aimed at them measured
//! *flat* across four runs each way, because it was worth 0.9 core-seconds of a
//! number that swung between 5.6 and 6.2 on its own.
//!
//! So each kind goes in on its own and then cumulatively, over the same
//! collected paths, and each line is one kind's share rather than a total to be
//! subtracted from another total. The answer was not close:
//!
//! | rule kind | µs an entry |
//! |---|---|
//! | none, short-circuited | 0.08 |
//! | 7 excluded paths | 0.11 |
//! | 11 excluded directory names | 0.08 |
//! | 3 excluded file names | 0.08 |
//! | **2 allow sequences** | **0.68** |
//!
//! All of it in the one rule the settings file treats as an afterthought.
//! `target/release` turns on two whole-path folds an entry — `Rules::allows`
//! to place the tail, `Rules::inside_excluded` to check every ancestor — where
//! the other three kinds are a hash lookup and a few prefix comparisons.
//! Reading the tail from the end and folding into a reused buffer took the
//! allow line to 0.35 and the real configuration from 1.09 to 0.75.
//!
//!   cargo run --release -p scour-source-fs --example rulecost -- <root>

use std::time::Instant;

use scour_core::{Entry, EntrySink, Flow, ScanOptions, Source, SourceId};
use scour_source_fs::{FsSource, Rules};

/// Keeps the paths, so the walk is paid for once and the rules many times.
#[derive(Default)]
struct Keep {
    rows: Vec<(String, bool)>,
}

impl EntrySink for Keep {
    fn push(&mut self, e: Entry) -> Flow {
        self.rows.push((e.path, e.is_dir));
        Flow::Continue
    }
}

fn main() {
    let root = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/mnt/depo".into());
    let (def_paths, def_dirs, def_files) = scour_source_fs::platform_defaults();
    let allow = vec!["target/release".to_owned(), "target/debug".to_owned()];

    // Collected with no rules at all, so every entry is present to be judged —
    // including the ones the real rules would have pruned, which are exactly
    // the ones the tests have to run on.
    let source = FsSource::new(SourceId(0), "probe", vec![root.clone().into()]);
    let mut keep = Keep::default();
    let began = Instant::now();
    let _ = source.scan(
        &ScanOptions {
            hidden: true,
            skip_metadata: true,
            threads: 2,
            ..Default::default()
        },
        &mut keep,
    );
    println!(
        "{root}: {} yol toplandi, {:.2?}",
        keep.rows.len(),
        began.elapsed()
    );

    let n = keep.rows.len().max(1);
    let time = |what: &str, opts: ScanOptions| {
        let rules = Rules::from_options(&opts);
        // Two rounds, and the faster reported: this is a cache-resident loop
        // over the same data, so the slower one is measuring the machine.
        let mut best = f64::MAX;
        for _ in 0..2 {
            let t = Instant::now();
            let mut hits = 0u64;
            for (path, is_dir) in &keep.rows {
                let name = path.rsplit('/').next().unwrap_or(path);
                if rules.excludes(path, name, *is_dir) {
                    hits += 1;
                }
            }
            let took = t.elapsed().as_secs_f64();
            best = best.min(took);
            std::hint::black_box(hits);
        }
        println!(
            "  {what:<26} {:>7.2} ms   {:>5.2} us/kayit",
            best * 1e3,
            best * 1e6 / n as f64
        );
    };

    println!("her kural turu tek basina:");
    time("bos (kisa devre)", ScanOptions::default());
    time(
        "yalniz exclude_paths (7)",
        ScanOptions {
            exclude_paths: def_paths.clone(),
            ..Default::default()
        },
    );
    time(
        "yalniz exclude_dirs (11)",
        ScanOptions {
            exclude_dirs: def_dirs.clone(),
            ..Default::default()
        },
    );
    time(
        "yalniz exclude_files (3)",
        ScanOptions {
            exclude_files: def_files.clone(),
            ..Default::default()
        },
    );
    time(
        "yalniz allow (2 dizi)",
        ScanOptions {
            allow: allow.clone(),
            ..Default::default()
        },
    );

    println!("birikimli:");
    time(
        "paths + dirs",
        ScanOptions {
            exclude_paths: def_paths.clone(),
            exclude_dirs: def_dirs.clone(),
            ..Default::default()
        },
    );
    time(
        "paths + dirs + files",
        ScanOptions {
            exclude_paths: def_paths.clone(),
            exclude_dirs: def_dirs.clone(),
            exclude_files: def_files.clone(),
            ..Default::default()
        },
    );
    time(
        "hepsi (gercek ayar)",
        ScanOptions {
            exclude_paths: def_paths,
            exclude_dirs: def_dirs,
            exclude_files: def_files,
            allow,
            ..Default::default()
        },
    );
}
