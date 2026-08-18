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

    // **The number a source's rows carry is remembered, not counted.** It used
    // to be the position in the configuration array, so moving two sources
    // around in the file rebound every row one of them had written — and a
    // source deleted from the file left its rows with nothing that would ever
    // walk them again. See `crate::sources`.
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
            // Never shorter than the burst clock, because below it nothing
            // happens sooner — `commit_interval` is a floor on how often a
            // segment is written at all — so a smaller number would read faster
            // than it behaves.
            commit_idle: Duration::from_millis(
                config
                    .service
                    .commit_idle_ms
                    .max(config.service.commit_interval_ms),
            ),
            rebuild_threshold: config.index.rebuild_threshold,
            // Raised to the browser window's fetch run and no further. That
            // page asks for fixed runs of 200 rows and records the whole run as
            // loaded, so a ceiling under it leaves rows that never arrive.
            // Above it the number in the file is the owner's and is taken as
            // written — this was `.max(1_000)`, which silently ignored every
            // value below a thousand including `UiCfg`'s own default of 200, so
            // the setting could not be believed at all.
            result_limit: config.ui.result_limit.max(200),
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

/// The data directory *this* configuration points at.
///
/// The parent of the index, which is the same convention `main.rs` uses to
/// decide where the kept settings go: everything one service writes about
/// itself sits together, so `--config` moves all of it or none of it.
///
/// **It was `scour_config::data_dir()` below**, computed from `ProjectDirs`
/// while the two entries beside it followed the configuration. A service
/// started with `--config` therefore refused to index the real user's data
/// directory — which it does not write, and which is somebody's files as far
/// as it is concerned — while leaving its own unguarded, which is the loop the
/// entry exists to prevent.
fn data_dir(config: &Config) -> &std::path::Path {
    config
        .index
        .dir
        .parent()
        .unwrap_or(config.index.dir.as_path())
}

/// The configured exclusions, plus the platform's own.
///
/// Merged rather than replaced: someone who adds one directory to the list
/// still wants `/proc` skipped, and discovering otherwise means discovering it
/// the hard way.
fn scan_options(config: &Config) -> scour_core::ScanOptions {
    // Read here rather than passed in, because the walk is configured before
    // `main.rs` opens the settings, and the two must not disagree about which
    // directory they are reading from — `data_dir` is what makes `--config`
    // real isolation.
    let added = scour_settings::Settings::load(&state_dir(config));
    scan_options_with(config, &added)
}

/// Where a window's own settings live, beside this index.
pub fn state_dir(config: &Config) -> std::path::PathBuf {
    data_dir(config).join("state")
}

/// What `config.toml` itself asks to skip — no built-ins, nothing a window
/// added.
///
/// Reported rather than used: a panel shows these as their own group, because
/// they can be switched off but not deleted. Removing one means rewriting a
/// hand-edited file, and that file is where the reasoning behind every value in
/// it is written down.
pub fn config_rules(config: &Config) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
    let c = &config.exclude;
    (
        c.paths.clone(),
        c.dirs.clone(),
        c.files.clone(),
        c.allow.clone(),
    )
}

/// The rules in force, given what a window has written.
///
/// **Three sources reach the walk, and they are different kinds of thing.** The
/// built-in set is code. `config.toml` is a file somebody wrote by hand, with
/// comments and measurements in it. And the third is what a panel wrote, which
/// lives beside the index rather than in that file — the same decision, and the
/// same reasoning, as the column widths: rewriting a hand-edited config to
/// record something typed into a checkbox would destroy the part of it that is
/// worth keeping.
///
/// They are merged and then the switched-off ones are taken back out, in that
/// order, because a rule can be written in two places and switching it off has
/// to mean off — not "off in one of the two lists it is in".
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
    // **The same loop, one step out**: everything else Scour writes about
    // itself. The web window is a browser, its profile lives under Scour's
    // data directory, and a browser writes to its cache constantly. Watched,
    // that becomes: the window writes → the index goes dirty → a client is
    // waiting, so the commit clock is one second rather than fifteen → the
    // revision moves → the page wakes and runs its whole query again → the
    // window writes more cache.
    //
    // Measured with a window open and nothing else happening on the machine:
    // the revision moved **fourteen times in twenty seconds** and one
    // connection thread sat at **8.25% of a core**, against 0.37% for the
    // whole service with the window shut — for a profile of 5,110 entries
    // nobody will ever search for. Excluding the index did not catch this,
    // because the profile is not in the index directory.
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

    // **Switched off, last, after everything is in.**
    //
    // A rule can be written in two of the three places — somebody's
    // `config.toml` says `target` and so does the built-in set — and switching
    // it off has to mean off rather than "off in one of the lists it is in".
    // Taking them out at the end is what makes that true whatever the overlap.
    //
    // `deny` is deliberately not offered: it holds the service's own index and
    // data directory, which is not an exclusion a person chose but the feedback
    // loop that cost two cores when an `allow` rule reopened it once already.
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

    /// The rules with nothing switched off, whatever this machine's owner has
    /// done to their own.
    ///
    /// **These used to call `scan_options`, which reads the real settings
    /// file** — so the moment somebody switched off `node_modules` from a
    /// panel, two tests here started failing on their machine and nowhere
    /// else. A test that depends on the state of the desktop it runs on is a
    /// test that will be believed right up until it is not.
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

    /// Whatever list a rule is in, switching it off takes it out of force.
    ///
    /// The three groups are three different kinds of thing — code, a
    /// hand-written file, and what a window wrote — and only the last can be
    /// deleted. The switch is what makes the other two something a person can
    /// still say no to, so it has to reach all three.
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

    /// A rule written in two places is off when it is switched off — not
    /// half-off.
    ///
    /// Subtracting at the end rather than per source is what makes this true:
    /// `target` is in the built-in set *and* in this configuration, and taking
    /// it out of one list would leave the other still excluding it. The switch
    /// would then do nothing, visibly, for the one rule people most want to
    /// turn off.
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

    /// What the service writes about itself cannot be switched back on.
    ///
    /// `deny` is not an exclusion somebody chose; it is the feedback loop that
    /// cost two cores — a commit writes segments, the watcher sees them, the
    /// engine indexes them, and indexing them writes segments. An `allow` rule
    /// reopened it once already, which is why it is a separate list, and the
    /// switches must not be a second way in.
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
        // The index directory is not the only thing this program writes. The
        // web window's browser profile is under the data directory, and a
        // browser's cache churns; watched, it kept the page waking itself in
        // a circle at 8.25% of a core. Nothing under here is searchable in
        // any sense a person means.
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

        // **And it is the configured directory, not the one `ProjectDirs`
        // names.** This entry alone was `scour_config::data_dir()` while the
        // two beside it followed the configuration, so a service started with
        // `--config` guarded a directory it does not write and left the one it
        // does write wide open — the wrong half of the promise, both ways
        // round. Only the default was ever tested, and the default is the one
        // case where the two answers agree.
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
        // A commit writes segments, the watcher sees them, the engine indexes
        // them, and indexing them writes segments. On an idle machine that
        // loop was 62% of a core and two thirds of every filesystem event.
        let c = Config::default();
        let o = plain(&c);
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
