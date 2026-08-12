//! Does the index still say what the disk says?
//!
//! **The question the architecture turns on, and nobody had asked it.** Keeping
//! an index across restarts means reconciling it — generations, sweeps, the
//! notes a walk leaves about rows it did not rewrite — and that machinery
//! produced three separate faults in one day. Throwing it away and building a
//! fresh index on every start would remove all of it, measured at five more
//! core-seconds and 186 MB of writes each time.
//!
//! Which of those is right depends entirely on whether the kept index actually
//! drifts. So this counts the disagreements, both directions:
//!
//! * **ghosts** — rows the index holds for files that are not there
//! * **missing** — files on the disk with no row
//!
//! Read-only, and it will not touch a running service: point it at a copy.
//!
//!   cp -a ~/.local/share/scour/index /tmp/idx-copy
//!   cargo run --release -p scour-index-native --example verify -- \
//!       /tmp/idx-copy/native /home/hasan /mnt/depo

use std::collections::HashSet;
use std::time::Instant;

use scour_core::{Entry, EntrySink, Flow, ScanOptions, Source, SourceId, path_digest};
use scour_index_native::NativeIndex;
use scour_source_fs::FsSource;

/// The disk side, as digests. 2.2 M paths as strings is a quarter of a
/// gigabyte; as `u64` it is eighteen megabytes and answers the same question.
#[derive(Default)]
struct Seen {
    digests: HashSet<u64>,
    paths: Vec<String>,
    keep: usize,
}

impl EntrySink for Seen {
    fn push(&mut self, e: Entry) -> Flow {
        self.digests.insert(path_digest(SourceId(0), &e.path));
        if self.paths.len() < self.keep {
            self.paths.push(e.path);
        }
        Flow::Continue
    }
}

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = args.next().expect("indeks dizini (…/native)");
    let roots: Vec<String> = args.collect();
    assert!(!roots.is_empty(), "en az bir kok ver");

    let index = NativeIndex::open_or_create(std::path::Path::new(&dir)).expect("indeks acilamadi");

    // The disk, walked exactly as the service walks it — same rules, same
    // allowances. A rule that differs here invents drift that is not there,
    // which is the one way this measurement can lie.
    let (def_paths, def_dirs, def_files) = scour_source_fs::platform_defaults();
    let opts = ScanOptions {
        hidden: true,
        follow_symlinks: false,
        skip_metadata: true,
        threads: 2,
        exclude_paths: def_paths,
        exclude_dirs: def_dirs,
        exclude_files: def_files,
        allow: vec!["target/release".into(), "target/debug".into()],
        // **Everything Scour writes about itself**, which is what the service
        // denies — not just the index. The web window is a browser and its
        // profile lives under the data directory; leaving it in reported 6,192
        // "missing" files in one Chromium cache directory and none of them were
        // drift. A verification whose rules differ from the service's measures
        // the difference between the two rule sets.
        deny: vec![
            dir.clone(),
            scour_config::data_dir().to_string_lossy().into_owned(),
        ],
        ..Default::default()
    };

    let began = Instant::now();
    let mut disk = Seen {
        keep: 0,
        ..Default::default()
    };
    for root in &roots {
        let src = FsSource::new(SourceId(0), "verify", vec![root.into()]);
        let _ = src.scan(&opts, &mut disk);
    }
    println!("diskte {} yol, {:.2?}", disk.digests.len(), began.elapsed());

    // The index, row by row. `for_each_segment` hands over the live view; a
    // dead row is one a commit has already retired and is not a ghost.
    let mut rows = 0u64;
    let mut ghosts: Vec<String> = Vec::new();
    let mut ghost_count = 0u64;
    let mut in_index: HashSet<u64> = HashSet::new();
    index
        .for_each_segment(&mut |_, seg| {
            for row in 0..seg.rows() {
                if !seg.is_alive(row) {
                    continue;
                }
                let Some(name) = seg.names.get(row) else {
                    continue;
                };
                let path = seg.path(row, name);
                rows += 1;
                let d = path_digest(SourceId(0), &path);
                in_index.insert(d);
                if !disk.digests.contains(&d) {
                    ghost_count += 1;
                    if ghosts.len() < 10 {
                        ghosts.push(path);
                    }
                }
            }
        })
        .expect("segmentler okunamadi");

    // The missing ones by name, because *which* they are decides what the
    // number means: a burst in one directory is churn since the copy, a whole
    // tree is a rule this walk does not share with the service, and a scatter
    // is drift.
    let mut missing_paths: Vec<String> = Vec::new();
    for root in &roots {
        let src = FsSource::new(SourceId(0), "verify", vec![root.into()]);
        let mut pick = Pick {
            want: &in_index,
            out: &mut missing_paths,
        };
        let _ = src.scan(&opts, &mut pick);
    }
    let missing = missing_paths.len();

    println!("indekste {rows} canli satir");
    println!(
        "  hayalet (indekste var, diskte yok) : {ghost_count}  (%{:.4})",
        100.0 * ghost_count as f64 / rows.max(1) as f64
    );
    println!(
        "  eksik   (diskte var, indekste yok) : {missing}  (%{:.4})",
        100.0 * missing as f64 / disk.digests.len().max(1) as f64
    );
    for p in ghosts.iter().take(5) {
        println!("    hayalet: {p}");
    }
    let mut by_dir: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for p in &missing_paths {
        let d = p.rsplit_once('/').map_or("/", |(d, _)| d).to_owned();
        *by_dir.entry(d).or_default() += 1;
    }
    let mut top: Vec<(String, usize)> = by_dir.into_iter().collect();
    top.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
    println!("  eksiklerin toplandigi yerler:");
    for (d, n) in top.iter().take(12) {
        println!("    {n:>6}  {d}");
    }
}

/// The second pass: names the ones the index does not have.
struct Pick<'a> {
    want: &'a HashSet<u64>,
    out: &'a mut Vec<String>,
}

impl EntrySink for Pick<'_> {
    fn push(&mut self, e: Entry) -> Flow {
        if !self.want.contains(&path_digest(SourceId(0), &e.path)) {
            self.out.push(e.path);
        }
        Flow::Continue
    }
}
