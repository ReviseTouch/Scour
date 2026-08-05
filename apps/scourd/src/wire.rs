//! Choosing the implementations.
//!
//! Every concrete type in Scour is named here and nowhere else. That is the
//! architecture's one rule, made visible: if a second file ever needs to say
//! `NativeIndex`, something above has stopped being written against the
//! trait.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use scour_config::Config;
use scour_core::{Index, Source, SourceId};
use scour_engine::{Engine, EngineOptions};
use scour_index_native::NativeIndex;
use scour_source_fs::{FsSource, platform_defaults};

pub fn build(config: &Config) -> Result<Engine> {
    let dir = index_dir(config);
    // One engine, so this is a single line rather than a match. The `Index`
    // trait is what keeps it that way: adding a second one is a second arm
    // here and nothing anywhere else.
    let index: Arc<dyn Index> = Arc::new(open_index(&dir)?);

    let sources: Vec<Arc<dyn Source>> = config
        .sources
        .iter()
        .enumerate()
        .map(|(i, s)| {
            Arc::new(
                FsSource::new(SourceId(i as u32), s.name.clone(), s.roots.clone())
                    .with_kind(s.kind.into())
                    .with_watch(s.watch),
            ) as Arc<dyn Source>
        })
        .collect();

    Ok(Engine::new(
        sources,
        index,
        EngineOptions {
            scan: scan_options(config),
            commit_interval: Duration::from_millis(config.service.commit_interval_ms.max(50)),
            rebuild_threshold: config.index.rebuild_threshold,
            result_limit: config.ui.result_limit.max(1_000),
            ..EngineOptions::default()
        },
    ))
}

/// Open the index, rebuilding it from nothing if it was written by an older
/// version of Scour.
///
/// The decision belongs here rather than in the index, because it rests on
/// something only this file knows: every source configured here is a local
/// filesystem that can be read again. A migration would have to be written
/// once per format change and would produce, at best, exactly what a rescan
/// produces. A cold index is scanned on start without being asked, so throwing
/// it away is the whole of the repair.
fn open_index(dir: &std::path::Path) -> Result<NativeIndex> {
    match NativeIndex::open_or_create(dir) {
        Err(scour_core::Error::IndexOutdated { found, expected }) => {
            eprintln!(
                "scourd: the index at {} is format {found} and this build writes {expected}; \
                 building it again",
                dir.display()
            );
            NativeIndex::discard(dir)
                .with_context(|| format!("clearing the old index at {}", dir.display()))?;
            NativeIndex::open_or_create(dir)
                .with_context(|| format!("opening the index at {}", dir.display()))
        }
        other => other.with_context(|| format!("opening the index at {}", dir.display())),
    }
}

/// Where the index files go.
///
/// A subdirectory rather than the configured directory itself, so that a
/// second engine could exist beside this one without either counting the
/// other's bytes in `bytes_on_disk`.
fn index_dir(config: &Config) -> std::path::PathBuf {
    config.index.dir.join("native")
}

/// The configured exclusions, plus the platform's own.
///
/// Merged rather than replaced: someone who adds one directory to the list
/// still wants `/proc` skipped, and discovering otherwise means discovering it
/// the hard way.
fn scan_options(config: &Config) -> scour_core::ScanOptions {
    let (paths, dirs, files) = platform_defaults();
    let mut o = config.scan_options();
    o.skip_metadata = false;
    // **The index does not index itself**, and the reason is not tidiness.
    //
    // A commit writes segment files; the watcher sees them; the engine turns
    // them into entries; committing those writes more segment files. Measured
    // on an idle machine: 171 of the 247 files changed in the last minute were
    // in the index directory, the worker thread at 36% of a core and the
    // inotify thread at 26%, feeding each other with nothing else happening.
    //
    // It belongs here rather than in the platform defaults because only this
    // file knows where the index went — it is configuration, and a user who
    // moves it must not have to know to exclude it.
    // **Denied rather than excluded**, which is the difference between a rule
    // and a rule a user can switch off by accident: an `allow` covering the
    // index directory used to win, because allow is tested first and returns
    // before the exclusions are read.
    merge(
        &mut o.deny,
        vec![
            config.index.dir.to_string_lossy().into_owned(),
            index_dir(config).to_string_lossy().into_owned(),
        ],
    );
    merge(&mut o.exclude_paths, paths);
    merge(&mut o.exclude_dirs, dirs);
    merge(&mut o.exclude_files, files);
    o
}

fn merge(into: &mut Vec<String>, extra: Vec<String>) {
    for e in extra {
        if !into.iter().any(|x| x.eq_ignore_ascii_case(&e)) {
            into.push(e);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_exclusions_are_added_to_the_configured_ones() {
        let mut c = Config::default();
        c.exclude.dirs = vec!["my-own".into()];
        let o = scan_options(&c);
        assert!(
            o.exclude_dirs.iter().any(|d| d == "my-own"),
            "the user's rule survives"
        );
        assert!(
            o.exclude_dirs.iter().any(|d| d == "node_modules"),
            "the platform's does too"
        );
    }

    #[test]
    fn the_index_never_indexes_itself() {
        // A commit writes segments, the watcher sees them, the engine indexes
        // them, and indexing them writes segments. On an idle machine that
        // loop was 62% of a core and two thirds of every filesystem event.
        let c = Config::default();
        let o = scan_options(&c);
        let dir = index_dir(&c).to_string_lossy().into_owned();
        assert!(
            o.deny.iter().any(|p| dir.starts_with(p.as_str())),
            "the index directory {dir} is not excluded: {:?}",
            o.deny
        );
        // And an allow rule cannot take it back. This is what the separate
        // list is for: the feedback loop it prevents cost two cores.
        let mut c2 = Config::default();
        c2.exclude.allow = vec!["/".into()];
        let o2 = scan_options(&c2);
        let rules = scour_source_fs::Rules::from_options(&o2);
        assert!(
            rules.excludes_path(&format!("{dir}/seg-00000001.cols")),
            "an allow rule reopened the index to itself"
        );
    }

    #[test]
    fn a_rule_is_not_duplicated_when_it_is_already_there() {
        let mut c = Config::default();
        c.exclude.dirs = vec!["NODE_MODULES".into()];
        let o = scan_options(&c);
        assert_eq!(
            o.exclude_dirs
                .iter()
                .filter(|d| d.eq_ignore_ascii_case("node_modules"))
                .count(),
            1
        );
    }
}
