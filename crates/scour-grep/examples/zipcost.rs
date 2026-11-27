//! Measure `scour-grep` throughput on a list of paths (one per line on stdin).
//!
//!   ~/.local/bin/scour --json -n 2000 "ext:udf" | ... | zipcost "mahkemesi"

use std::io::{BufRead as _, BufReader};
use std::path::PathBuf;
use std::time::Instant;

use scour_grep::{Budget, File, Ground, Needle, Options, Outcome, Pattern, Scanner, Skip};

fn main() {
    let needle = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "mahkemesi".into());
    let roots = [
        PathBuf::from("/home"),
        PathBuf::from("/mnt/depo"),
        PathBuf::from("/"),
    ];
    let files: Vec<File> = BufReader::new(std::io::stdin().lock())
        .lines()
        .map_while(Result::ok)
        .filter(|l| !l.trim().is_empty())
        .map(|l| File {
            path: PathBuf::from(l),
            size: None,
            class: None,
        })
        .collect();

    let ground = Ground::probe(roots);
    let pat = Pattern::new(&Needle::Text(needle.clone()), false).expect("pattern");
    let scanner = Scanner::new(pat, Options::default(), ground);

    let t0 = Instant::now();
    let budget = Budget::unlimited();
    let outcomes = scanner.scan(&files, &budget);
    let elapsed = t0.elapsed();

    let (mut found, mut clean, mut bin, mut err) = (0usize, 0usize, 0usize, 0usize);
    for o in &outcomes {
        match o {
            Outcome::Found { .. } => found += 1,
            Outcome::Clean => clean += 1,
            Outcome::Skipped(Skip::Binary) => bin += 1,
            _ => err += 1,
        }
    }
    let secs = elapsed.as_secs_f64().max(1e-9);
    let mb = budget.spent() as f64 / (1024.0 * 1024.0);
    println!(
        "files={} found={} clean={} bin_skipped={} other={} read={:.2} MiB time={:.2} ms ({:.0} files/s, {:.1} MiB/s compressed)",
        files.len(),
        found,
        clean,
        bin,
        err,
        mb,
        secs * 1000.0,
        files.len() as f64 / secs,
        mb / secs,
    );
}
