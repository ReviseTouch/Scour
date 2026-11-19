//! Paths on standard input, one a line; what they hold of a needle, and how
//! long reading them took. For measuring: `scour -n 100000 'ext:rs' | …`.
//!
//!     cargo run --release -p scour-grep --example scan -- [--binary] NEEDLE

use std::io::BufRead as _;
use std::time::Instant;

use scour_grep::{Budget, File, Ground, Needle, Options, Outcome, Pattern, Scanner};

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let binary = args
        .iter()
        .position(|a| a == "--binary")
        .map(|i| args.remove(i))
        .is_some();
    let Some(needle) = args.pop() else {
        eprintln!("usage: scan [--binary] NEEDLE < paths");
        std::process::exit(2);
    };
    let files: Vec<File> = std::io::stdin()
        .lock()
        .lines()
        .map_while(Result::ok)
        .map(|line| File {
            path: line.into(),
            size: None,
        })
        .collect();
    let pattern = Pattern::new(&Needle::Text(needle), false).expect("a needle");
    // One root a top-level directory: enough to tell this machine's disks apart.
    let mut roots: Vec<_> = files
        .iter()
        .filter_map(|f| f.path.ancestors().filter(|a| a.parent().is_some()).last())
        .map(|root| root.to_path_buf())
        .collect();
    roots.sort();
    roots.dedup();
    let options = Options {
        binary,
        max_size: 0,
        ..Options::default()
    };
    let scanner = Scanner::new(pattern, options, Ground::probe(&roots));
    let (budget, began) = (Budget::unlimited(), Instant::now());
    let outcomes = scanner.scan(&files, &budget);
    let took = began.elapsed().as_secs_f64();
    let count = |want: fn(&Outcome) -> bool| outcomes.iter().filter(|o| want(o)).count();
    println!(
        "{} files, {} with a find, {} skipped, {} unreadable",
        files.len(),
        count(|o| matches!(o, Outcome::Found { .. })),
        count(|o| matches!(o, Outcome::Skipped(_))),
        count(|o| matches!(o, Outcome::Unreadable(_))),
    );
    let mib = budget.spent() as f64 / (1 << 20) as f64;
    println!(
        "{mib:.1} MiB in {took:.3} s: {:.0} MiB/s, {:.0} files/s",
        mib / took,
        files.len() as f64 / took
    );
}
