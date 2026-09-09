//! Which rule costs what: each kind on its own and then cumulatively, over the same
//! collected paths, because through a whole walk the difference is inside the noise.
//! Two allow sequences measured 0.68 µs an entry against 0.08–0.11 for the other
//! three kinds together. `cargo run --release --example rulecost -- <root>`

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
    // including the ones the real rules would have pruned.
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
        // Two rounds and the faster reported: this loop is cache-resident.
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
