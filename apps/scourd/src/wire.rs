//! Choosing the implementations.
//!
//! Every concrete type in Scour is named here and nowhere else. That is the
//! architecture's one rule, made visible: if a second file ever needs to say
//! `TantivyIndex`, something above has stopped being written against the
//! trait.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use scour_config::{Config, EngineCfg};
use scour_core::{Index, Source, SourceId};
use scour_engine::{Engine, EngineOptions};
use scour_index_native::NativeIndex;
use scour_index_tantivy::{IndexOptions, TantivyIndex};
use scour_source_fs::{FsSource, platform_defaults};

pub fn build(config: &Config) -> Result<Engine> {
    let dir = index_dir(config);
    let index: Arc<dyn Index> = match config.index.engine {
        EngineCfg::Native => Arc::new(
            NativeIndex::open_or_create(&dir)
                .with_context(|| format!("opening the index at {}", dir.display()))?,
        ),
        EngineCfg::Tantivy => Arc::new(
            TantivyIndex::open_or_create(
                &dir,
                IndexOptions {
                    index_paths: config.index.paths,
                    index_content: config.content.enabled,
                    writer_heap_mb: config.index.heap_mb,
                    rebuild_threshold: config.index.rebuild_threshold,
                },
            )
            .with_context(|| format!("opening the index at {}", dir.display()))?,
        ),
    };

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

/// Where this engine's files go.
///
/// A subdirectory each, rather than the configured directory itself. The two
/// formats have nothing in common, so sharing a directory would mean switching
/// engines left the other one's files lying beside the new ones, counted by
/// `bytes_on_disk` and never read. It also lets both exist at once, which is
/// what makes the comparison a matter of one line in a file.
fn index_dir(config: &Config) -> std::path::PathBuf {
    match config.index.engine {
        EngineCfg::Native => config.index.dir.join("native"),
        EngineCfg::Tantivy => config.index.dir.join("tantivy"),
    }
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
