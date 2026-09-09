//! Choosing the implementations: every concrete type in Scour is named here and
//! nowhere else. If a second file needs to say `NativeIndex`, something above it
//! has stopped being written against the trait.

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
    // One engine, so a line rather than a match; a second one is a second arm
    // here and nothing anywhere else.
    let index: Arc<dyn Index> = Arc::new(open_index(&dir)?);

    // The number a source's rows carry is remembered, not counted: a position
    // in the configuration array would rebind every row when sources move.
    let names: Vec<String> = config.sources.iter().map(|s| s.name.clone()).collect();
    let assigned = crate::sources::assign(&dir, &names);
    for id in &assigned.dropped {
        match index.forget(SourceId(*id)) {
            Ok(0) => {}
            Ok(n) => eprintln!("scourd: a source is gone; {n} of its entries went with it"),
            Err(e) => eprintln!("scourd: a removed source's entries are still here: {e}"),
        }
    }

    let sources: Vec<Arc<dyn Source>> = config
        .sources
        .iter()
        .zip(&assigned.ids)
        .map(|(s, id)| {
            Arc::new(
                FsSource::new(SourceId(*id), s.name.clone(), s.roots.clone())
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
            // Never shorter than the burst clock: `commit_interval` is a floor
            // on how often a segment is written at all.
            commit_idle: Duration::from_millis(
                config
                    .service
                    .commit_idle_ms
                    .max(config.service.commit_interval_ms),
            ),
            rebuild_threshold: config.index.rebuild_threshold,
            poll_interval: Duration::from_secs(config.service.poll_interval_secs.max(1)),
            reconcile_interval: Duration::from_secs(config.service.reconcile_interval_secs.max(1)),
            // Raised to a window's fetch run and no further: a window records
            // a whole run of `scour_core::PAGE_ROWS` as loaded, so a lower
            // ceiling leaves rows that never arrive.
            result_limit: config.ui.result_limit.max(scour_core::PAGE_ROWS),
            ..EngineOptions::default()
        },
    ))
}

/// Open the index, rebuilding it from nothing if an older Scour wrote it. The
/// decision is here because only this file knows every source is a local
/// filesystem that can be read again, and a cold index is scanned on start.
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

/// Where the index files go: a subdirectory, so a second engine beside this one
/// does not count its bytes in `bytes_on_disk`.
fn index_dir(config: &Config) -> std::path::PathBuf {
    config.index.dir.join("native")
}

/// The data directory *this* configuration points at: the parent of the index,
/// the same convention `main.rs` uses for the kept settings, so `--config` moves
/// everything a service writes about itself or none of it.
fn data_dir(config: &Config) -> &std::path::Path {
    config
        .index
        .dir
        .parent()
        .unwrap_or(config.index.dir.as_path())
}

/// The configured exclusions, plus the platform's own — merged rather than
/// replaced: adding one directory still leaves `/proc` skipped.
fn scan_options(config: &Config) -> scour_core::ScanOptions {
    // Read here rather than passed in: the walk is configured before `main.rs`
    // opens the settings, and the two must read the same directory.
    let added = scour_settings::Settings::load(&state_dir(config));
    scan_options_with(config, &added)
}

/// Where a window's own settings live, beside this index: [`Config::state_dir`],
/// under the name the rest of this program uses.
pub fn state_dir(config: &Config) -> std::path::PathBuf {
    config.state_dir()
}

/// What `config.toml` itself asks to skip — no built-ins, nothing a window
/// added. Reported rather than used: a panel shows these as their own group,
/// because they can be switched off but not deleted from a hand-edited file.
pub fn config_rules(config: &Config) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
    let c = &config.exclude;
    (
        c.paths.clone(),
        c.dirs.clone(),
        c.files.clone(),
        c.allow.clone(),
    )
}

/// The rules in force, given what a window has written: the built-in set, the
/// hand-written `config.toml`, and what a panel wrote beside the index. Merged
/// first and switched-off ones taken out last, so off means off in both lists.
pub fn scan_options_with(
    config: &Config,
    added: &scour_settings::Settings,
) -> scour_core::ScanOptions {
    let (paths, dirs, files) = platform_defaults();
    let mut o = config.scan_options();
    o.skip_metadata = false;

    o.exclude_paths.extend(added.exclude_paths.iter().cloned());
    o.exclude_dirs.extend(added.exclude_dirs.iter().cloned());
    o.exclude_files.extend(added.exclude_files.iter().cloned());
    o.allow.extend(added.exclude_allow.iter().cloned());
    // Nothing Scour writes about itself is indexed: a commit writes segments,
    // the watcher sees them, indexing them writes more — 62% of a core idle,
    // and 8.25% more per open web window through its browser profile.
    //
    // Denied rather than excluded, since an `allow` is tested first and would
    // win; here rather than in the platform defaults because only this file
    // knows where the index was configured to go.
    merge(
        &mut o.deny,
        vec![
            config.index.dir.to_string_lossy().into_owned(),
            index_dir(config).to_string_lossy().into_owned(),
            data_dir(config).to_string_lossy().into_owned(),
        ],
    );
    merge(&mut o.exclude_paths, paths);
    merge(&mut o.exclude_dirs, dirs);
    merge(&mut o.exclude_files, files);

    // Switched off last, after everything is in, so a rule written in two of
    // the three places goes out of both. `deny` is deliberately not offered:
    // it holds the service's own directories, not an exclusion anyone chose.
    switch_off(&mut o.exclude_paths, "path", added);
    switch_off(&mut o.exclude_dirs, "dir", added);
    switch_off(&mut o.exclude_files, "file", added);
    switch_off(&mut o.allow, "allow", added);
    o
}

fn merge(into: &mut Vec<String>, extra: Vec<String>) {
    for e in extra {
        if !into.iter().any(|x| x.eq_ignore_ascii_case(&e)) {
            into.push(e);
        }
    }
}

fn switch_off(list: &mut Vec<String>, kind: &str, added: &scour_settings::Settings) {
    list.retain(|v| !added.rule_off(kind, v));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rules with nothing switched off: never `scan_options`, which reads
    /// the real settings file and makes a test depend on the desktop it runs on.
    fn plain(config: &Config) -> scour_core::ScanOptions {
        scan_options_with(config, &scour_settings::Settings::default())
    }

    #[test]
    fn platform_exclusions_are_added_to_the_configured_ones() {
        let mut c = Config::default();
        c.exclude.dirs = vec!["my-own".into()];
        let o = plain(&c);
        assert!(
            o.exclude_dirs.iter().any(|d| d == "my-own"),
            "the user's rule survives"
        );
        assert!(
            o.exclude_dirs.iter().any(|d| d == "node_modules"),
            "the platform's does too"
        );
    }

    /// Whatever list a rule is in, switching it off takes it out of force: only
    /// what a window wrote can be deleted, so the switch has to reach all three.
    #[test]
    fn a_rule_switched_off_is_not_in_force_whichever_list_it_is_in() {
        let mut c = Config::default();
        c.exclude.dirs = vec!["from-the-file".into()];
        let added = scour_settings::Settings {
            exclude_dirs: vec!["from-a-window".into()],
            exclude_off: vec![
                "dir:node_modules".into(), // built in
                "dir:from-the-file".into(),
                "dir:from-a-window".into(),
            ],
            ..Default::default()
        };
        let o = scan_options_with(&c, &added);
        for gone in ["node_modules", "from-the-file", "from-a-window"] {
            assert!(
                !o.exclude_dirs.iter().any(|d| d == gone),
                "{gone} is switched off and still being skipped: {:?}",
                o.exclude_dirs
            );
        }
        // And the ones nobody touched are untouched.
        assert!(o.exclude_dirs.iter().any(|d| d == "__pycache__"));
    }

    /// A rule written in two places is off, not half-off: subtracting at the
    /// end rather than per source is what makes that true.
    #[test]
    fn switching_off_a_rule_written_twice_switches_off_both_copies() {
        let mut c = Config::default();
        c.exclude.dirs = vec!["target".into()];
        let added = scour_settings::Settings {
            exclude_off: vec!["dir:target".into()],
            ..Default::default()
        };
        let o = scan_options_with(&c, &added);
        assert!(
            !o.exclude_dirs
                .iter()
                .any(|d| d.eq_ignore_ascii_case("target")),
            "one copy of the rule survived the switch: {:?}",
            o.exclude_dirs
        );
    }

    /// What the service writes about itself cannot be switched back on: `deny`
    /// is a separate list because an `allow` rule reopened the loop once.
    #[test]
    fn the_switches_cannot_reopen_the_index_to_itself() {
        let c = Config::default();
        let dir = index_dir(&c).to_string_lossy().into_owned();
        let added = scour_settings::Settings {
            // Every spelling of it somebody might reach for.
            exclude_off: vec![
                format!("path:{dir}"),
                format!("dir:{dir}"),
                format!("deny:{dir}"),
            ],
            ..Default::default()
        };
        let o = scan_options_with(&c, &added);
        let rules = scour_source_fs::Rules::from_options(&o);
        assert!(
            rules.excludes_path(&format!("{dir}/seg-00000001.cols")),
            "a switch reopened the index to itself: {:?}",
            o.deny
        );
    }

    #[test]
    fn scour_never_indexes_what_it_writes_about_itself() {
        // The web window's browser profile is under the data directory, and
        // watched it kept the page waking itself at 8.25% of a core.
        let c = Config::default();
        let o = plain(&c);
        let rules = scour_source_fs::Rules::from_options(&o);
        let data = scour_config::data_dir();
        assert!(
            rules.excludes_path(&format!(
                "{}/app/Default/Cache/Cache_Data/x_0",
                data.display()
            )),
            "the window's own browser cache is indexed: {:?}",
            o.deny
        );

        // The configured directory, not the one `ProjectDirs` names: under
        // `--config` the two disagree, and the default is where they do not.
        let mut elsewhere = Config::default();
        elsewhere.index.dir = "/srv/scour-elsewhere/index".into();
        let o = plain(&elsewhere);
        let rules = scour_source_fs::Rules::from_options(&o);
        assert!(
            rules.excludes_path("/srv/scour-elsewhere/app/Default/Cache/Cache_Data/x_0"),
            "this service's own browser cache is indexed: {:?}",
            o.deny
        );
        assert!(
            !o.deny.iter().any(|p| std::path::Path::new(p) == data),
            "a service living elsewhere still refuses the real data directory: {:?}",
            o.deny
        );
    }

    #[test]
    fn the_index_never_indexes_itself() {
        // The commit/watch/index loop: 62% of a core on an idle machine, and
        // two thirds of every filesystem event.
        let c = Config::default();
        let o = plain(&c);
        let dir = index_dir(&c).to_string_lossy().into_owned();
        assert!(
            o.deny.iter().any(|p| dir.starts_with(p.as_str())),
            "the index directory {dir} is not excluded: {:?}",
            o.deny
        );
        // And an allow rule cannot take it back; that is what `deny` is for.
        let mut c2 = Config::default();
        c2.exclude.allow = vec!["/".into()];
        let o2 = plain(&c2);
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
        let o = plain(&c);
        assert_eq!(
            o.exclude_dirs
                .iter()
                .filter(|d| d.eq_ignore_ascii_case("node_modules"))
                .count(),
            1
        );
    }
}
