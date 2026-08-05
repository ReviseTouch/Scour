//! The file itself.

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
    pub content: ContentCfg,
    pub service: ServiceCfg,
    pub ui: UiCfg,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct IndexCfg {
    pub dir: PathBuf,
    /// Which implementation answers searches.
    ///
    /// The two do not share a file format and do not read each other's
    /// directories, so changing this means the first scan runs again. They are
    /// kept side by side because the comparison is the only honest way to know
    /// which one to ship — see `docs/MEASUREMENTS.md`.
    pub engine: EngineCfg,
    /// Index full paths as trigrams, so `path:` is a term rather than a
    /// filter applied to every candidate.
    ///
    /// **Off by default, and measured**: on 855,126 entries it cost 174 MB of
    /// a 352 MB index — 45%. `under:` and `parent:` are unaffected and remain
    /// the fast way to scope a search to a folder, because those are ancestor
    /// tokens rather than trigrams. Turn this on if `path:` matters more than
    /// the disk.
    pub paths: bool,
    /// Writer memory in megabytes. The peak while indexing follows it.
    pub heap_mb: usize,
    /// How many entries may sit outside the ordered body before a rebuild is
    /// advised. Every query reads all of them.
    pub rebuild_threshold: u64,
}

/// The implementations of `Index` a build knows about.
///
/// Named rather than numbered so the file says what it means, and so a build
/// without one of them can report an unknown engine instead of a wrong one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EngineCfg {
    /// The index written for this workload: no inverted index, rows in date
    /// order, 44.5 bytes an entry.
    #[default]
    Native,
}

impl Default for IndexCfg {
    fn default() -> Self {
        Self {
            dir: crate::paths::default_index_dir(),
            engine: EngineCfg::default(),
            paths: false,
            // Only ever used for a bulk scan or a rebuild, and handed back
            // afterwards. These documents are a path, a name and ten numbers —
            // there is no body text — so the steady state runs on far less.
            heap_mb: 128,
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
    /// Skip the per-entry `stat` on the first pass and fill the rest in
    /// afterwards. The difference between a usable index in a minute and one
    /// in ten.
    pub fast: bool,
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
            fast: true,
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

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContentCfg {
    /// Index the contents of documents, not just their names.
    ///
    /// Nothing extracts content yet. The setting exists because turning it on
    /// changes what is on disk, and the index has to be built knowing.
    pub enabled: bool,
    /// Files larger than this are indexed by name only.
    pub max_file_mb: u64,
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
    /// Run a maintenance pass this many hours after starting, and then at that
    /// interval. Zero disables it.
    pub maintain_every_hours: u64,
}

impl Default for ServiceCfg {
    fn default() -> Self {
        Self {
            socket: String::new(),
            commit_interval_ms: 1_000,
            maintain_every_hours: 24,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiCfg {
    pub result_limit: u32,
    /// BCP-47 tag, or empty for the system language.
    pub language: String,
    pub columns: Vec<String>,
}

impl Default for UiCfg {
    fn default() -> Self {
        Self {
            result_limit: 200,
            language: String::new(),
            columns: ["name", "path", "size", "mtime"].map(String::from).to_vec(),
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
                // A malformed file must not stop the service: it starts on
                // defaults and reports why, which is far better than refusing
                // to run because of one stray character.
                Err(e) => (
                    Config::default().with_defaults_filled(),
                    Some(Error::Config {
                        detail: e.to_string(),
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
            paths = false
            [[source]]
            name = "work"
            roots = ["/srv/work"]
        "#;
        let c: Config = toml::from_str(text).expect("parse");
        assert!(!c.index.paths);
        assert_eq!(c.index.heap_mb, IndexCfg::default().heap_mb);
        assert_eq!(c.sources[0].name, "work");
        assert!(c.sources[0].watch, "an omitted flag takes its default");
        assert_eq!(c.ui.result_limit, 200);
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
