//! The file itself. Every key here is read by something: a setting that is
//! parsed and then ignored is worse than an absent one, because the absent one
//! cannot be believed.

use std::path::PathBuf;

use scour_core::{Error, Result, ScanOptions, SourceKind};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub index: IndexCfg,
    /// Where entries come from. An array and not a list of roots, because a
    /// source carries its own kind, capabilities and eventually credentials.
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
    /// Rescan every source when the service starts. On by default and required
    /// until a source advertises `Caps::JOURNAL`: nothing watches a filesystem
    /// while the service is stopped, so changes between runs have no other way in.
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
    /// How long changes accumulate before a commit. A commit costs tens of
    /// milliseconds, so a new file appears after about this long; removals are
    /// hidden immediately regardless.
    pub commit_interval_ms: u64,
    /// How long changes may wait while nobody is looking; a window registers as a
    /// watcher and gets the fast clock. The largest piece of an idle service's
    /// cost, and linear in commits: 0.09% of a core at 5 s, 0.022% at 60 s.
    pub commit_idle_ms: u64,
    /// Recheck a source without a change feed or a readable pulse.
    pub poll_interval_secs: u64,
    /// Reconcile even a quiet, watched source to recover silent event loss.
    /// Expensive walks rest for at least twenty times their previous duration.
    pub reconcile_interval_secs: u64,
}

impl Default for ServiceCfg {
    fn default() -> Self {
        Self {
            socket: String::new(),
            commit_interval_ms: 1_000,
            commit_idle_ms: 15_000,
            poll_interval_secs: 60,
            reconcile_interval_secs: 1_800,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiCfg {
    /// The most rows one page may hold, whatever a caller asks for: a ceiling on
    /// the service, not a preference of any one window. `scourd` raises anything
    /// below [`scour_core::PAGE_ROWS`], the fixed run a paging window fetches in.
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
    /// Load the file, or write a default one and return that. Writing on first run
    /// is what makes the exclusion lists visible enough to edit.
    pub fn load_or_default() -> (Config, Option<Error>) {
        let path = crate::paths::config_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(c) => (c.with_defaults_filled(), None),
                // Fatal, because falling back to defaults falls back to a source
                // list of one entry: the engine would `forget` every other source
                // and drop its rows while staying up.
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
    /// the platform's list of things no index should hold.
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
            // Filled by the wiring, the only place that knows where the index went.
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

    /// Where everything a person chose from inside a face is kept. Beside the
    /// index rather than in the hand-written `config.toml`, so `--config` isolates
    /// both; one copy, because all four faces have to agree on it.
    pub fn state_dir(&self) -> std::path::PathBuf {
        self.index
            .dir
            .parent()
            .unwrap_or(self.index.dir.as_path())
            .join("state")
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
        // still valid and nothing has to migrate.
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
        // A key that silently does nothing is discovered months later.
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
