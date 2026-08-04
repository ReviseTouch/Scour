//! What not to index.
//!
//! Three kinds of rule, plus one that overrules them:
//!
//! * **Paths** — a prefix. `/proc` skips everything under it.
//! * **Directory names** — anywhere they appear. `node_modules` skips every
//!   copy of it, and there are always many.
//! * **File names** — likewise.
//! * **Allow** — a prefix that wins over all of the above, so one interesting
//!   directory can be rescued from a broad exclusion without unpicking it.
//!
//! Defaults are per-platform and live in [`platform_defaults`]. They are data
//! rather than code so that the settings screen can show and edit them, which
//! is the whole reason a user ever looks at this: something they wanted was
//! missing, and they need to see why.

use scour_core::ScanOptions;

/// Compiled exclusion rules. Built once per scan, then asked per entry.
#[derive(Debug, Default, Clone)]
pub struct Rules {
    paths: Vec<String>,
    dirs: Vec<String>,
    files: Vec<String>,
    allow: Vec<String>,
}

impl Rules {
    pub fn from_options(opts: &ScanOptions) -> Self {
        let norm = |v: &Vec<String>| -> Vec<String> {
            v.iter()
                .map(|s| crate::path::normalise(s).trim_end_matches('/').to_owned())
                .filter(|s| !s.is_empty())
                .collect()
        };
        Self {
            paths: norm(&opts.exclude_paths),
            dirs: opts.exclude_dirs.iter().map(|s| s.to_lowercase()).collect(),
            files: opts
                .exclude_files
                .iter()
                .map(|s| s.to_lowercase())
                .collect(),
            allow: norm(&opts.allow),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty() && self.dirs.is_empty() && self.files.is_empty()
    }

    /// Should this entry be skipped?
    ///
    /// `path` is already `/`-normalised.
    pub fn excludes(&self, path: &str, name: &str, is_dir: bool) -> bool {
        if self.allow.iter().any(|a| under(path, a)) {
            return false;
        }
        if self.paths.iter().any(|p| under(path, p)) {
            return true;
        }
        let lower = name.to_lowercase();
        let list = if is_dir { &self.dirs } else { &self.files };
        list.contains(&lower)
    }

    /// Should this path be skipped, judged from the path alone?
    ///
    /// The same answer as [`Rules::excludes`] without the `is_dir` argument,
    /// and therefore **without a syscall** — every component is checked
    /// against the directory rules and the last one against the file rules,
    /// so a path with `target` anywhere in it is out whether or not anything
    /// still exists to stat.
    ///
    /// This is what a watcher needs. The first version of that filter called
    /// `is_dir()` per event, which is one `stat` for every file a compiler
    /// writes — measured at 74% of a core while a build ran, for events that
    /// were then thrown away.
    pub fn excludes_path(&self, path: &str) -> bool {
        if self.allow.iter().any(|a| under(path, a)) {
            return false;
        }
        if self.paths.iter().any(|p| under(path, p)) {
            return true;
        }
        let mut last = "";
        for part in path.split('/').filter(|p| !p.is_empty()) {
            last = part;
            let lower = part.to_lowercase();
            if self.dirs.contains(&lower) {
                return true;
            }
            // A rule like `.git/objects` is two components; check the tail of
            // the path against it rather than one name at a time.
            if self
                .dirs
                .iter()
                .any(|d| d.contains('/') && under(path, d) || path.ends_with(d.as_str()))
            {
                return true;
            }
        }
        self.files.contains(&last.to_lowercase())
    }

    /// Could anything under this directory still be wanted?
    ///
    /// A directory excluded by prefix may still contain an allowed subtree, and
    /// pruning it there would make the allow rule a lie.
    pub fn may_contain_allowed(&self, path: &str) -> bool {
        self.allow.iter().any(|a| under(a, path))
    }
}

/// Is `path` inside `prefix`, or the prefix itself?
///
/// Compared by path component, not by characters: `/ab` is not inside `/a`.
fn under(path: &str, prefix: &str) -> bool {
    let p = prefix.trim_end_matches('/');
    if p.is_empty() {
        return true;
    }
    path == p || (path.starts_with(p) && path.as_bytes().get(p.len()) == Some(&b'/'))
}

/// Things no file index should hold, per platform.
///
/// Two categories, and they are excluded for different reasons. Virtual
/// filesystems (`/proc`, `/sys`) are not files at all and reading them can
/// block forever. Build output and package caches are real files, but they
/// churn constantly and bury real results under thousands of hashes — the
/// user's own `target/` directory is the single loudest source of noise in a
/// developer's home directory.
pub fn platform_defaults() -> (Vec<String>, Vec<String>, Vec<String>) {
    let mut paths: Vec<String> = Vec::new();
    let mut dirs: Vec<String> = Vec::new();
    let files: Vec<String> = vec![".DS_Store".into(), "Thumbs.db".into(), "desktop.ini".into()];

    #[cfg(target_os = "linux")]
    {
        paths.extend(
            [
                "/proc",
                "/sys",
                "/dev",
                "/run",
                "/tmp",
                "/var/lib/docker",
                "/var/cache",
            ]
            .map(String::from),
        );
        dirs.push(".Trash-1000".into());
    }
    #[cfg(target_os = "macos")]
    {
        paths.extend(["/dev", "/System/Volumes/Data/private", "/private/var/vm"].map(String::from));
        dirs.push(".Spotlight-V100".into());
        dirs.push(".fseventsd".into());
    }
    #[cfg(windows)]
    {
        paths.extend(
            [
                "C:/Windows/WinSxS",
                "C:/Windows/Temp",
                "C:/$Recycle.Bin",
                "C:/System Volume Information",
            ]
            .map(String::from),
        );
    }

    // Everywhere: churn, not content.
    //
    // `target` is the one this list used to describe and not contain, and the
    // omission was expensive twice over. It is **852,437 of 2,986,545 entries**
    // on this machine — 28% of an index, none of it written by anyone — and
    // while a compile is running the watcher turns it into a flood: 3,935
    // changes queued, the service at 67% of a core, and every search behind
    // them. Measured during one `cargo test`.
    //
    // It is a *directory name*, so it is skipped wherever it appears, and
    // `exclude.allow` takes it back for anyone who wants to search a build
    // tree — one line of configuration against a third of the index.
    //
    // `build`, `dist` and `out` are deliberately **not** here. They cost
    // another 249,445 entries and they are plausible names for real work,
    // which `target` beside a `Cargo.toml` is not. Anyone who wants them gone
    // adds them; the default does not guess.
    dirs.extend(
        [
            ".git/objects",
            "target",
            "node_modules",
            "__pycache__",
            ".venv",
            ".mypy_cache",
            ".pytest_cache",
            ".gradle",
            ".ccache",
            ".cargo/registry",
        ]
        .map(String::from),
    );

    (paths, dirs, files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Rules {
        Rules::from_options(&ScanOptions {
            exclude_paths: vec!["/proc".into(), "/home/u/big".into()],
            exclude_dirs: vec!["node_modules".into(), "TARGET".into()],
            exclude_files: vec![".DS_Store".into()],
            allow: vec!["/home/u/big/keep".into()],
            ..Default::default()
        })
    }

    #[test]
    fn prefixes_are_matched_by_component() {
        let r = rules();
        assert!(r.excludes("/proc/1/status", "status", false));
        assert!(r.excludes("/proc", "proc", true));
        // The bug this exists to prevent: /procession is not inside /proc.
        assert!(!r.excludes("/procession/x", "x", false));
    }

    #[test]
    fn names_are_matched_case_insensitively_and_by_type() {
        let r = rules();
        assert!(r.excludes("/a/b/node_modules", "node_modules", true));
        assert!(
            r.excludes("/a/b/Target", "Target", true),
            "rules are case-insensitive"
        );
        // A *file* called node_modules is not the directory rule's business.
        assert!(!r.excludes("/a/b/node_modules", "node_modules", false));
        assert!(r.excludes("/a/.DS_Store", ".DS_Store", false));
    }

    #[test]
    fn an_allow_rule_wins() {
        let r = rules();
        assert!(r.excludes("/home/u/big/junk", "junk", true));
        assert!(!r.excludes("/home/u/big/keep", "keep", true));
        assert!(!r.excludes("/home/u/big/keep/deep/x.txt", "x.txt", false));
    }

    #[test]
    fn an_excluded_directory_on_the_way_to_an_allowed_one_is_not_pruned() {
        // Otherwise the allow rule would be unreachable: the walk would stop at
        // /home/u/big and never see /home/u/big/keep.
        let r = rules();
        assert!(r.may_contain_allowed("/home/u/big"));
        assert!(r.may_contain_allowed("/home/u"));
        assert!(!r.may_contain_allowed("/home/u/other"));
    }

    #[test]
    fn platform_defaults_are_populated_and_sane() {
        let (paths, dirs, files) = platform_defaults();
        assert!(dirs.iter().any(|d| d == "node_modules"));
        // 28% of a developer's index and the loudest thing a compile does.
        assert!(dirs.iter().any(|d| d == "target"));
        // And the ones that are plausible names for real work stay out of it.
        for plausible in ["build", "dist", "out", "src"] {
            assert!(
                !dirs.iter().any(|d| d == plausible),
                "{plausible} is a name people use for their own work"
            );
        }
        assert!(files.iter().any(|f| f == ".DS_Store"));
        // Every default path must be absolute, or it would match nothing.
        assert!(
            paths.iter().all(|p| p.starts_with('/') || p.contains(':')),
            "{paths:?}"
        );
    }
}
