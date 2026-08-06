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
///
/// Directory rules are compiled into **component sequences**, and that is the
/// whole of a bug this file carried for as long as it has existed: the
/// defaults contain `.git/objects` and `.cargo/registry`, and both sides
/// compared a rule containing a `/` against a single file name, which can
/// never contain one. The two highest-churn directories on a developer's disk
/// were named in the defaults, shown in the settings, and indexed anyway.
#[derive(Debug, Default, Clone)]
pub struct Rules {
    paths: Vec<String>,
    /// Rules naming one directory, wherever it appears. The common case, and
    /// the one that has to stay a hash lookup on the entry's own name.
    dirs: Vec<String>,
    /// Rules naming a sequence — `.git/objects`. Matched at any component
    /// boundary, and everything below the match goes with it.
    dir_seqs: Vec<Vec<String>>,
    files: Vec<String>,
    allow: Vec<String>,
    /// Exclusions an allow rule may not overrule.
    ///
    /// The index's own directory is the whole reason this exists: a user's
    /// `allow` that happens to cover it turns the service into a thing that
    /// indexes what it writes while writing it — measured, before it was
    /// excluded, at 36% and 26% of two cores feeding each other.
    deny: Vec<String>,
}

impl Rules {
    pub fn from_options(opts: &ScanOptions) -> Self {
        let norm = |v: &Vec<String>| -> Vec<String> {
            v.iter()
                .map(|s| crate::path::normalise(s).trim_end_matches('/').to_owned())
                .filter(|s| !s.is_empty())
                .collect()
        };
        let lower: Vec<String> = opts.exclude_dirs.iter().map(|s| s.to_lowercase()).collect();
        Self {
            paths: norm(&opts.exclude_paths),
            dirs: lower.iter().filter(|d| !d.contains('/')).cloned().collect(),
            dir_seqs: lower
                .iter()
                .filter(|d| d.contains('/'))
                .map(|d| {
                    d.split('/')
                        .filter(|c| !c.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .collect(),
            files: opts
                .exclude_files
                .iter()
                .map(|s| s.to_lowercase())
                .collect(),
            allow: norm(&opts.allow),
            deny: norm(&opts.deny),
        }
    }

    /// Does a directory-sequence rule match, ending at this path?
    ///
    /// For the scan, where the entry being judged is the directory itself: the
    /// walker prunes it, so its children never arrive to be asked about.
    fn seq_ends_at(&self, path: &str, name: &str) -> bool {
        let lower = name.to_lowercase();
        let comps: Vec<&str> = path.split('/').filter(|c| !c.is_empty()).collect();
        self.dir_seqs.iter().any(|rule| {
            rule.last().is_some_and(|last| *last == lower)
                && comps.len() >= rule.len()
                && comps[comps.len() - rule.len()..]
                    .iter()
                    .zip(rule)
                    .all(|(c, r)| c.to_lowercase() == *r)
        })
    }

    /// Does a directory-sequence rule match anywhere in this path?
    ///
    /// For the watcher, which is handed a path with no walk behind it and has
    /// to decide about descendants on its own.
    fn seq_within(&self, path: &str) -> bool {
        if self.dir_seqs.is_empty() {
            return false;
        }
        let comps: Vec<String> = path
            .split('/')
            .filter(|c| !c.is_empty())
            .map(str::to_lowercase)
            .collect();
        self.dir_seqs.iter().any(|rule| {
            comps.len() >= rule.len() && comps.windows(rule.len()).any(|w| w == rule.as_slice())
        })
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
            && self.dirs.is_empty()
            && self.dir_seqs.is_empty()
            && self.files.is_empty()
            && self.deny.is_empty()
    }

    /// Should this entry be skipped?
    ///
    /// `path` is already `/`-normalised.
    pub fn excludes(&self, path: &str, name: &str, is_dir: bool) -> bool {
        if self.deny.iter().any(|d| under(path, d)) {
            return true;
        }
        if self.allow.iter().any(|a| under(path, a)) {
            return false;
        }
        if self.paths.iter().any(|p| under(path, p)) {
            return true;
        }
        let lower = name.to_lowercase();
        if is_dir {
            return self.dirs.contains(&lower) || self.seq_ends_at(path, name);
        }
        self.files.contains(&lower)
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
        if self.deny.iter().any(|d| under(path, d)) {
            return true;
        }
        if self.allow.iter().any(|a| under(path, a)) {
            return false;
        }
        if self.paths.iter().any(|p| under(path, p)) {
            return true;
        }
        let mut last = "";
        for part in path.split('/').filter(|p| !p.is_empty()) {
            last = part;
            if self.dirs.contains(&part.to_lowercase()) {
                return true;
            }
        }
        self.seq_within(path) || self.files.contains(&last.to_lowercase())
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
    // `mut` on a platform that adds nothing to them is an unused-mut warning,
    // and Android is that platform: it has no `/proc` to exclude by absolute
    // path because an app cannot walk outside its own directory anyway.
    #[allow(unused_mut)]
    let mut paths: Vec<String> = Vec::new();
    #[allow(unused_mut)]
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
    fn a_rule_naming_two_components_excludes_the_tree_under_it() {
        // `.git/objects` and `.cargo/registry` are in the defaults, are shown
        // in the settings, and were indexed anyway: the scan compared a rule
        // containing a `/` against a file name, which never contains one, and
        // the watcher's version matched the directory itself but nothing below
        // it. The two busiest directories on a developer's disk.
        let rules = Rules::from_options(&ScanOptions {
            exclude_dirs: vec![".git/objects".into(), "node_modules".into()],
            ..Default::default()
        });

        // The scan is asked about the directory, and prunes it.
        assert!(rules.excludes("/p/.git/objects", "objects", true));
        // The watcher is asked about anything, with no walk behind it.
        assert!(rules.excludes_path("/p/.git/objects/aa/3f2b1c"));
        assert!(rules.excludes_path("/p/.git/objects"));

        // What must not be caught: the same last component under a different
        // parent, and the parent itself.
        assert!(!rules.excludes("/p/build/objects", "objects", true));
        assert!(!rules.excludes_path("/p/build/objects/aa"));
        assert!(!rules.excludes_path("/p/.git/config"));
        // A misleading suffix is not a component boundary.
        assert!(!rules.excludes_path("/p/not.git/objects-old/x"));

        // And the one-component rules still work the cheap way.
        assert!(rules.excludes("/p/node_modules", "node_modules", true));
        assert!(rules.excludes_path("/p/node_modules/react/index.js"));
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
