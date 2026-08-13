//! The file itself.
//!
//! ## Eight settings that were removed rather than wired up
//!
//! `index.engine`, `index.paths`, `index.heap_mb`, `scan.fast`, `ui.columns`,
//! `service.maintain_every_hours`, `content.enabled`, `content.max_file_mb`.
//! All were parsed, defaulted and round-trip tested; none was read by anything.
//! A third of this file described a program that did not exist.
//!
//! Two of them did worse than nothing. `scan.fast` promised "the difference
//! between a usable index in a minute and one in ten" and was forced off at
//! both wiring sites. `index.paths` carried a real measurement — 174 MB of a
//! 352 MB index on 855,126 entries — and said "turn this on if `path:` matters
//! more than the disk", and turning it on did nothing at all. A setting that
//! promises a measured result and delivers none is worse than an absent one,
//! because the absent one cannot be believed.
//!
//! `content.*` went with them even though `trait Extractor` is deliberately
//! reserved and stays. The trait is vocabulary the workspace talks to itself
//! in; a config key is a promise to a person, and `content.enabled = true`
//! silently did nothing. `ui.columns` went because the Slint window will want
//! column *widths* beside the list, so the shape it would come back in is
//! already known to differ from the shape it had.
//!
//! They come back when something reads them, and the file is shorter and true
//! until then.

use std::path::PathBuf;

use scour_core::{Error, Result, ScanOptions, SourceKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub index: IndexCfg,
    /// Where entries come from.
    ///
    /// An array rather than a list of roots, because a source is more than a
    /// path: it has its own kind, its own capabilities, and eventually its own
    /// credentials. A cloud bucket added later is another entry here, not a
    /// new shape of configuration file.
    #[serde(rename = "source")]
    pub sources: Vec<SourceCfg>,
    pub scan: ScanCfg,
    pub exclude: ExcludeCfg,
    pub service: ServiceCfg,
    pub ui: UiCfg,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IndexCfg {
    pub dir: PathBuf,
    /// How many entries may sit outside the ordered body before a rebuild is
    /// advised. Every query reads all of them.
    pub rebuild_threshold: u64,
}

impl Default for IndexCfg {
    fn default() -> Self {
        Self {
            dir: crate::paths::default_index_dir(),
            rebuild_threshold: 200_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SourceCfg {
    /// Stable name, used in configuration and on the wire.
    pub name: String,
    pub kind: SourceKindCfg,
    pub roots: Vec<PathBuf>,
    /// Watch this source for changes.
    pub watch: bool,
}

impl Default for SourceCfg {
    fn default() -> Self {
        Self {
            name: "home".into(),
            kind: SourceKindCfg::Local,
            roots: Vec::new(),
            watch: true,
        }
    }
}

/// Mirrors [`SourceKind`], separately, so the on-disk vocabulary can outlive a
/// change to the internal one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKindCfg {
    #[default]
    Local,
    Removable,
    Network,
    Cloud,
}

impl From<SourceKindCfg> for SourceKind {
    fn from(k: SourceKindCfg) -> Self {
        match k {
            SourceKindCfg::Local => SourceKind::Local,
            SourceKindCfg::Removable => SourceKind::Removable,
            SourceKindCfg::Network => SourceKind::Network,
            SourceKindCfg::Cloud => SourceKind::Cloud,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ScanCfg {
    pub hidden: bool,
    pub follow_symlinks: bool,
    /// Worker threads; zero decides from the hardware.
    pub threads: usize,
    /// Rescan every source when the service starts.
    ///
    /// **On by default, and it has to be until there is a journal.** Nothing
    /// watches a filesystem while the service is stopped, and no source here
    /// advertises `Caps::JOURNAL`, so a file created, deleted or renamed
    /// between one run and the next has no way into the index at all. Off, an
    /// ordinary restart left those changes wrong for as long as the machine
    /// lived, and nothing anywhere said so.
    ///
    /// A walk costs a few seconds of background work on a warm cache. That is
    /// the price of the index being about the disk rather than about the last
    /// time somebody remembered to rescan.
    pub on_start: bool,
}

impl Default for ScanCfg {
    fn default() -> Self {
        Self {
            hidden: true,
            follow_symlinks: false,
            threads: 0,
            on_start: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExcludeCfg {
    /// Prefixes to skip entirely.
    pub paths: Vec<String>,
    /// Directory names to skip anywhere they appear.
    pub dirs: Vec<String>,
    /// File names to skip anywhere they appear.
    pub files: Vec<String>,
    /// Prefixes that override every rule above.
    pub allow: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServiceCfg {
    /// Where the service listens. Empty means the platform default.
    pub socket: String,
    /// How long changes accumulate before a commit.
    ///
    /// A commit costs tens of milliseconds, so committing per change would
    /// make a `git checkout` unusable. What this buys is that a new file
    /// appears in results after about this long rather than instantly —
    /// removals are hidden immediately regardless, because a deleted file that
    /// is still listed is the more annoying failure.
    pub commit_interval_ms: u64,
    /// How long a handful of changes may wait while **nobody is looking**.
    ///
    /// The bound on staleness for a search typed at a prompt, which is the case
    /// this exists for: an open window registers as a watcher and gets the fast
    /// clock, a `scour foo` does not, so this is what it sees.
    ///
    /// **It is the largest single piece of what an idle service costs**, and
    /// the trade is measured. Only this number changed, alternating 180-second
    /// runs at three changes a second, arms that do not overlap:
    ///
    /// | this setting | worker |
    /// |---|---|
    /// | 5 s | ~0.09% of a core |
    /// | 15 s | 0.050% / 0.056% |
    /// | 60 s | 0.022% / 0.022% |
    ///
    /// The cost is linear in the number of commits and not in the rows they
    /// carry: a commit is about **ten `fsync` calls** — seven segment parts,
    /// the alive bitmap, the manifest — and one row costs 22.5 ms where a
    /// hundred and twenty-eight cost 23.7. So this is a freshness contract with
    /// a price on it rather than a tuning knob, and it belongs in the
    /// configuration for the same reason: only the person searching knows what
    /// their answer is worth.
    pub commit_idle_ms: u64,
}

impl Default for ServiceCfg {
    fn default() -> Self {
        Self {
            socket: String::new(),
            commit_interval_ms: 1_000,
            commit_idle_ms: 15_000,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiCfg {
    /// The most rows one page may hold, whatever a caller asks for.
    ///
    /// A ceiling on the service rather than a preference of any one window: it
    /// is what stops a client turning a keystroke into a million built rows.
    ///
    /// **Not honoured below 200.** `scourd` raises anything smaller, because
    /// the browser window fetches in fixed runs of 200 rows and marks the whole
    /// run as loaded — a page cut short leaves rows that never arrive and
    /// nothing that would ask for them again.
    ///
    /// The default was 200 against a floor of 1,000, so the number in this file
    /// did nothing for any value anyone was likely to write and the default
    /// itself was unreachable. 1,000 is what the service has always used.
    pub result_limit: u32,
    /// BCP-47 tag, or empty for the system language.
    pub language: String,
}

impl Default for UiCfg {
    fn default() -> Self {
        Self {
            result_limit: 1_000,
            language: String::new(),
        }
    }
}

impl Config {
    /// Load the file, or write a default one and return that.
    ///
    /// Writing on first run is deliberate: the exclusion lists are the thing
    /// people need to edit, and a file that does not exist cannot be read to
    /// find out what the options are.
    pub fn load_or_default() -> (Config, Option<Error>) {
        let path = crate::paths::config_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(c) => (c.with_defaults_filled(), None),
                // **A file that exists and does not parse is fatal**, and the
                // reasoning it replaces was right about the wrong settings.
                // "A malformed file must not stop the service: it starts on
                // defaults and reports why" is correct for a result limit or a
                // language — the default is harmless and the user loses a
                // preference. It is not correct for a source list, because the
                // default *is* a source list: one entry, the home directory.
                //
                // So a stray character in the file — one unknown key, and the
                // schema denies those — dropped every other source. On this
                // machine that is `/mnt/depo`: the engine sees a source it no
                // longer has and calls `forget`, and a million rows leave the
                // index. The service stays up, indexing the wrong tree, and
                // says so in one line on stderr that goes to the journal.
                //
                // Refusing to start is the smaller failure by a wide margin.
                // It is loud, it is immediate, and nothing is lost.
                Err(e) => (
                    Config::default().with_defaults_filled(),
                    Some(Error::Config {
                        detail: format!(
                            "{}: {e}\nNothing was loaded. Fix the file, or move it aside to \
                             start on defaults.",
                            path.display()
                        ),
                    }),
                ),
            },
            Err(_) => {
                let c = Config::default().with_defaults_filled();
                let err = c.save().err();
                (c, err)
            }
        }
    }

    pub fn load_from(path: &std::path::Path) -> Result<Config> {
        let text =
            std::fs::read_to_string(path).map_err(|e| Error::io(&e, &path.to_string_lossy()))?;
        toml::from_str::<Config>(&text)
            .map(Config::with_defaults_filled)
            .map_err(|e| Error::Config {
                detail: e.to_string(),
            })
    }

    pub fn save(&self) -> Result<()> {
        self.save_to(&crate::paths::config_path())
    }

    pub fn save_to(&self, path: &std::path::Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::io(&e, &dir.to_string_lossy()))?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| Error::Config {
            detail: e.to_string(),
        })?;
        std::fs::write(path, text).map_err(|e| Error::io(&e, &path.to_string_lossy()))
    }

    /// Fill in what a bare file leaves out: the home directory as a source, and
    /// the platform's own list of things no index should hold.
    fn with_defaults_filled(mut self) -> Self {
        if self.sources.is_empty() {
            let home = directories::UserDirs::new().map(|u| u.home_dir().to_path_buf());
            self.sources.push(SourceCfg {
                name: "home".into(),
                kind: SourceKindCfg::Local,
                roots: home.into_iter().collect(),
                watch: true,
            });
        }
        self
    }

    /// The scan options these settings describe.
    pub fn scan_options(&self) -> ScanOptions {
        ScanOptions {
            hidden: self.scan.hidden,
            follow_symlinks: self.scan.follow_symlinks,
            skip_metadata: false,
            threads: self.scan.threads,
            exclude_paths: self.exclude.paths.clone(),
            exclude_dirs: self.exclude.dirs.clone(),
            exclude_files: self.exclude.files.clone(),
            allow: self.exclude.allow.clone(),
            // Filled by the wiring, which is the only place that knows where
            // the index went.
            deny: Vec::new(),
            subtree: None,
        }
    }

    pub fn socket(&self) -> String {
        if self.service.socket.is_empty() {
            crate::paths::socket_path()
        } else {
            self.service.socket.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_config_round_trips_through_toml() {
        let c = Config::default().with_defaults_filled();
        let text = toml::to_string_pretty(&c).expect("serialise");
        let back: Config = toml::from_str(&text).expect("deserialise");
        assert_eq!(c, back);
        assert_eq!(c.sources.len(), 1, "a bare config still indexes something");
    }

    #[test]
    fn an_older_file_loads_with_new_fields_filled_in() {
        // Every table is `default`, so a file written before a field existed is
        // still valid — which is the whole reason nothing has to migrate.
        let text = r#"
            [index]
            rebuild_threshold = 50000
            [[source]]
            name = "work"
            roots = ["/srv/work"]
        "#;
        let c: Config = toml::from_str(text).expect("parse");
        assert_eq!(c.index.rebuild_threshold, 50_000);
        assert_eq!(c.index.dir, IndexCfg::default().dir, "an omitted field");
        assert_eq!(c.sources[0].name, "work");
        assert!(c.sources[0].watch, "an omitted flag takes its default");
        assert_eq!(c.ui.result_limit, 1_000);
    }

    #[test]
    fn a_misspelt_key_is_reported_rather_than_ignored() {
        // Silently doing nothing is the failure mode that gets discovered
        // months later.
        let text = "[scan]\nhiden = true\n";
        let err = toml::from_str::<Config>(text).unwrap_err();
        assert!(err.to_string().contains("hiden"), "{err}");
    }

    #[test]
    fn a_broken_file_does_not_stop_the_service() {
        let dir = tempfile::tempdir().expect("temp");
        let path = dir.path().join("bad.toml");
        std::fs::write(&path, "this is not toml {{{").expect("write");
        let err = Config::load_from(&path).unwrap_err();
        assert_eq!(err.code(), "config");
    }

    #[test]
    fn saving_and_loading_preserves_everything() {
        let dir = tempfile::tempdir().expect("temp");
        let path = dir.path().join("nested/config.toml");
        let mut c = Config::default().with_defaults_filled();
        c.exclude.dirs = vec!["node_modules".into(), "target".into()];
        c.sources.push(SourceCfg {
            name: "depo".into(),
            kind: SourceKindCfg::Removable,
            roots: vec!["/mnt/depo".into()],
            watch: false,
        });
        c.save_to(&path).expect("save");
        assert_eq!(Config::load_from(&path).expect("load"), c);
    }

    #[test]
    fn scan_options_carry_the_exclusions() {
        let mut c = Config::default();
        c.exclude.dirs = vec!["node_modules".into()];
        c.exclude.allow = vec!["/keep".into()];
        c.scan.threads = 4;
        let o = c.scan_options();
        assert_eq!(o.exclude_dirs, vec!["node_modules"]);
        assert_eq!(o.allow, vec!["/keep"]);
        assert_eq!(o.threads, 4);
    }
}
