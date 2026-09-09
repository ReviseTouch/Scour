//! Does a walk of several roots at once reach all of them?
//!
//!   cargo run --release -p scour-source-fs --example manyroots -- /usr /etc /opt /var
//!
//! One line per root per run: an unstable walk is a number that moves between runs.

use std::collections::BTreeMap;

use scour_core::{Entry, EntrySink, Flow, ScanOptions, Source, SourceId};
use scour_source_fs::FsSource;

struct PerRoot {
    roots: Vec<String>,
    counts: BTreeMap<String, u64>,
}

impl EntrySink for PerRoot {
    fn push(&mut self, entry: Entry) -> Flow {
        if let Some(root) = self
            .roots
            .iter()
            .find(|r| entry.path == **r || entry.path.starts_with(&format!("{r}/")))
        {
            *self.counts.entry(root.clone()).or_default() += 1;
        }
        Flow::Continue
    }
}

fn main() {
    let roots: Vec<String> = std::env::args().skip(1).collect();
    let roots = if roots.is_empty() {
        vec!["/usr".into(), "/etc".into(), "/opt".into(), "/var".into()]
    } else {
        roots
    };
    let src = FsSource::new(
        SourceId(2),
        "sistem",
        roots.iter().map(std::path::PathBuf::from).collect(),
    );
    let opts = ScanOptions {
        hidden: true,
        follow_symlinks: false,
        threads: 0,
        ..Default::default()
    };
    for run in 1..=3 {
        let mut sink = PerRoot {
            roots: roots.clone(),
            counts: BTreeMap::new(),
        };
        let report = src.scan(&opts, &mut sink).expect("walk");
        let seen: Vec<String> = roots
            .iter()
            .map(|r| format!("{r}={}", sink.counts.get(r).copied().unwrap_or(0)))
            .collect();
        println!(
            "run {run}: {}  ·  entries {} dirs {} excluded {} unreadable {} cancelled {} took {} ms",
            seen.join(" "),
            report.entries,
            report.dirs,
            report.excluded,
            report.unreadable,
            report.cancelled,
            report.took_ms,
        );
        println!(
            "         vouched {:?}  blind {}",
            report.vouched,
            report.blind.len()
        );
    }
}
