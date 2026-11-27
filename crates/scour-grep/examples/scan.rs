//! Paths on standard input, one a line; what they hold of a needle, and what
//! reading them cost. For measuring: `scour -n 100000 'ext:rs' | …`.
//!
//!     cargo run --release -p scour-grep --example scan -- \
//!         [--binary] [--regex | --hex | --expr] [--cold] [--list] [--quiet] NEEDLE
//!
//! `--expr` reads the needle as a content pattern does: `;`, `!`, `~word`.
//!
//! `--cold` asks the kernel to drop each file from the page cache first; a
//! file it would not drop is counted, so a "cold" run says how cold it was.

use std::io::BufRead as _;
use std::time::Instant;

use scour_grep::{Budget, Expr, File, Ground, Needle, Options, Outcome, Pattern, Scanner, Skip};

struct Args {
    binary: bool,
    /// Read at the lowest priority, as a long reading does.
    quiet: bool,
    /// The paths with a find and the line of the first, on standard error.
    list: bool,
    cold: bool,
    expr: bool,
    needle: Needle,
}

fn args() -> Args {
    let mut rest: Vec<String> = std::env::args().skip(1).collect();
    let mut flag = |name: &str| {
        rest.iter()
            .position(|a| a == name)
            .map(|i| rest.remove(i))
            .is_some()
    };
    let (binary, cold, regex, hex, list, quiet, expr) = (
        flag("--binary"),
        flag("--cold"),
        flag("--regex"),
        flag("--hex"),
        flag("--list"),
        flag("--quiet"),
        flag("--expr"),
    );
    let Some(text) = rest.pop() else {
        eprintln!("usage: scan [--binary] [--regex | --hex] [--cold] NEEDLE < paths");
        std::process::exit(2);
    };
    let needle = match (regex, hex) {
        (true, _) => Needle::Regex(text),
        (_, true) => Needle::hex(&text).unwrap_or_else(|| {
            eprintln!("not hex: {text}");
            std::process::exit(2)
        }),
        _ => Needle::Text(text),
    };
    Args {
        binary,
        quiet,
        list,
        cold,
        expr,
        needle,
    }
}

fn main() {
    let args = args();
    let files: Vec<File> = std::io::stdin()
        .lock()
        .lines()
        .map_while(Result::ok)
        .filter(|line| !line.trim().is_empty())
        .map(|line| File {
            path: line.into(),
            size: None,
            class: None,
        })
        .collect();
    let kept = if args.cold { drop_cached(&files) } else { 0 };
    // One root a top-level directory: enough to tell this machine's disks apart.
    let mut roots: Vec<_> = files
        .iter()
        .filter_map(|f| f.path.ancestors().filter(|a| a.parent().is_some()).last())
        .map(|root| root.to_path_buf())
        .collect();
    roots.sort();
    roots.dedup();
    // `SCAN_ROOTS=/a:/b` names the mounts outright; the top directory of a
    // path is not one under `/run/media`.
    if let Some(given) = std::env::var_os("SCAN_ROOTS") {
        roots = std::env::split_paths(&given).collect();
    }
    let options = Options {
        binary: args.binary,
        quiet: args.quiet,
        max_size: 0,
        ..Options::default()
    };
    for root in &roots {
        eprintln!(
            "# {} {:?}",
            root.display(),
            surefoot::probe::medium_of(root)
        );
    }
    let ground = Ground::probe(&roots);
    // `SCAN_RATE_MIB` paces the reading, as `rate_mib` does a service's.
    let pace = std::env::var("SCAN_RATE_MIB")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0);
    let budget = Budget::unlimited().paced(pace << 20);
    let (began, cpu_before) = (Instant::now(), usage());
    let outcomes = if args.expr {
        Expr::forgiving(&args.needle, false, scour_core::mistakes_allowed)
            .expect("a pattern")
            .scan(&options, &ground, None, None, &files, &budget)
    } else {
        let pattern = Pattern::new(&args.needle, false).expect("a needle");
        Scanner::new(pattern, options, ground).scan(&files, &budget)
    };
    let took = began.elapsed().as_secs_f64();
    let (cpu_after, peak_kib) = (usage(), peak_rss_kib());
    let count = |want: &dyn Fn(&Outcome) -> bool| outcomes.iter().filter(|o| want(o)).count();
    let mib = budget.spent() as f64 / (1 << 20) as f64;
    // One `key value` a line, for `scripts/content-bench` to put side by side.
    println!("files {}", files.len());
    println!("found {}", count(&|o| matches!(o, Outcome::Found { .. })));
    println!("clean {}", count(&|o| matches!(o, Outcome::Clean)));
    for (key, skip) in [
        ("binary", Skip::Binary),
        ("too_big", Skip::TooBig),
        ("not_a_file", Skip::NotAFile),
        ("header_only", Skip::HeaderOnly),
        ("no_reader", Skip::NoReader),
        ("locked", Skip::Locked),
    ] {
        println!("{key} {}", count(&|o| *o == Outcome::Skipped(skip)));
    }
    println!(
        "unreadable {}",
        count(&|o| matches!(o, Outcome::Unreadable(_)))
    );
    println!("cold_kept {kept}");
    println!("seconds {took:.3}");
    println!("mib {mib:.1}");
    println!("mib_per_s {:.0}", mib / took);
    println!("files_per_s {:.0}", files.len() as f64 / took);
    println!("cpu_s {:.3}", cpu_after - cpu_before);
    println!("peak_rss_mib {:.1}", peak_kib as f64 / 1024.0);
    if args.list {
        for (file, outcome) in files.iter().zip(&outcomes) {
            if let Outcome::Found { hits, .. } = outcome {
                let line = hits.first().and_then(|h| h.line).unwrap_or(0);
                eprintln!("{}\t{line}", file.path.display());
            }
        }
    }
}

/// Seconds of CPU this process has used, user and system.
#[cfg(unix)]
fn usage() -> f64 {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    let secs = |t: libc::timeval| t.tv_sec as f64 + t.tv_usec as f64 / 1e6;
    secs(ru.ru_utime) + secs(ru.ru_stime)
}

#[cfg(unix)]
fn peak_rss_kib() -> i64 {
    let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
    // Kilobytes on Linux, bytes on macOS.
    if cfg!(target_os = "macos") {
        ru.ru_maxrss / 1024
    } else {
        ru.ru_maxrss
    }
}

/// How many files still had pages in the cache after being asked to drop them.
#[cfg(target_os = "linux")]
fn drop_cached(files: &[File]) -> usize {
    use std::os::fd::AsRawFd as _;
    let mut kept = 0;
    for file in files {
        let Ok(f) = std::fs::File::open(&file.path) else {
            continue;
        };
        let fd = f.as_raw_fd();
        unsafe { libc::posix_fadvise(fd, 0, 0, libc::POSIX_FADV_DONTNEED) };
        if resident(&f) {
            kept += 1;
        }
    }
    kept
}

/// Whether any page of the file is in memory: `mincore` over a mapping of it.
#[cfg(target_os = "linux")]
fn resident(f: &std::fs::File) -> bool {
    use std::os::fd::AsRawFd as _;
    let len = f.metadata().map_or(0, |m| m.len()) as usize;
    if len == 0 {
        return false;
    }
    let page = 4096usize;
    let len = len.min(64 << 20);
    unsafe {
        let map = libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_SHARED,
            f.as_raw_fd(),
            0,
        );
        if map == libc::MAP_FAILED {
            return false;
        }
        let mut vec = vec![0u8; len.div_ceil(page)];
        let ok = libc::mincore(map, len, vec.as_mut_ptr()) == 0;
        libc::munmap(map, len);
        ok && vec.iter().any(|b| b & 1 == 1)
    }
}

#[cfg(not(target_os = "linux"))]
fn drop_cached(_files: &[File]) -> usize {
    eprintln!("--cold: only on Linux");
    0
}

#[cfg(not(unix))]
fn usage() -> f64 {
    0.0
}

#[cfg(not(unix))]
fn peak_rss_kib() -> i64 {
    0
}
