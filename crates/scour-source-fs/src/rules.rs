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
    /// Allow rules naming a sequence rather than a place — `target/release`,
    /// matched wherever it appears.
    ///
    /// **Why a sequence and not a path.** The exclusion that matters most to a
    /// developer is `target`, and it is right: 852,437 of 2,986,545 entries
    /// here, and a flood through the watcher while a build runs. But the
    /// binaries it produces are the very things a person wants to find and
    /// run, and they live in exactly two of its children. A path-shaped allow
    /// means writing one line per project and rewriting it per checkout; a
    /// sequence means `target/release` once, for every project on the disk.
    allow_seqs: Vec<Vec<String>>,
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
            allow: norm(
                &opts
                    .allow
                    .iter()
                    .filter(|a| a.starts_with('/'))
                    .cloned()
                    .collect(),
            ),
            // Anything that is not an absolute path is a sequence of directory
            // names, the same shape the exclusions already accept.
            allow_seqs: opts
                .allow
                .iter()
                .filter(|a| !a.starts_with('/'))
                .map(|a| {
                    a.split('/')
                        .filter(|c| !c.is_empty())
                        .map(str::to_lowercase)
                        .collect()
                })
                .collect(),
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

    /// Is this path taken back by an allow rule?
    ///
    /// **The two shapes mean different amounts, and the numbers are why.** A
    /// path — `/home/u/big/keep` — takes back a *subtree*, which is what a
    /// person naming one place means.
    ///
    /// A sequence — `target/release` — takes back that directory's **own
    /// entries and no deeper**. Measured on this machine: `target/release` is
    /// 11,056 entries in one project and 114,463 in another, nearly all of it
    /// `deps/`, while its top level is **41 entries containing all 5
    /// binaries**. A subtree rule would hand back everything the exclusion
    /// exists to keep out in order to reach a handful of files; this hands
    /// back the handful.
    fn allows(&self, path: &str, is_dir: bool) -> bool {
        if self.allow.iter().any(|a| under(path, a)) {
            return true;
        }
        if self.allow_seqs.is_empty() {
            return false;
        }
        let comps = lower_components(path);
        let n = comps.len();
        self.allow_seqs.iter().any(|rule| {
            let l = rule.len();
            // The named directory itself, so it can be entered and listed.
            if n >= l && comps[n - l..] == rule[..] {
                return true;
            }
            // A file directly inside it. Directories are deliberately left
            // out: `deps` under `release` is where the noise lives, and
            // refusing it here is what lets the walk prune it.
            !is_dir && n > l && comps[n - 1 - l..n - 1] == rule[..]
        })
    }

    /// Is any *ancestor* of this path an excluded directory?
    ///
    /// **Only asked when an allow rule exists.** Ordinarily an excluded
    /// directory is pruned and nothing under it is ever offered, so this
    /// question cannot arise and the walk pays nothing for it. An allow rule
    /// changes that: the walk is let into `target` to reach `target/release`,
    /// and without this everything else in there — every object file, every
    /// fingerprint — would be indexed *because* one child was wanted.
    fn inside_excluded(&self, path: &str) -> bool {
        let comps = lower_components(path);
        // The last component is the entry itself; its own rules were already
        // applied by the caller.
        let ancestors = comps.len().saturating_sub(1);
        if comps.iter().take(ancestors).any(|c| self.dirs.contains(c)) {
            return true;
        }
        self.seq_within(path)
    }

    /// Should this entry be skipped?
    ///
    /// `path` is already `/`-normalised.
    pub fn excludes(&self, path: &str, name: &str, is_dir: bool) -> bool {
        if self.deny.iter().any(|d| under(path, d)) {
            return true;
        }
        if self.allows(path, is_dir) {
            return false;
        }
        if self.paths.iter().any(|p| under(path, p)) {
            return true;
        }
        let lower = name.to_lowercase();
        let by_name = if is_dir {
            self.dirs.contains(&lower) || self.seq_ends_at(path, name)
        } else {
            self.files.contains(&lower)
        };
        if by_name {
            return true;
        }
        // See `inside_excluded`: free unless somebody asked for an exception.
        if self.allow.is_empty() && self.allow_seqs.is_empty() {
            return false;
        }
        self.inside_excluded(path)
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
        // No `is_dir` here, and the generous reading is the safe one: a
        // watcher letting one event through costs a check, and refusing one
        // costs a row that never updates.
        if self.allows(path, false) || self.allows(path, true) {
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
        if self.allow.iter().any(|a| under(a, path)) {
            return true;
        }
        if self.allow_seqs.is_empty() {
            return false;
        }
        // A rule naming `target/release` has to let the walk into `target`,
        // which is to say: this directory ends with some head of the rule.
        let comps = lower_components(path);
        self.allow_seqs.iter().any(|rule| {
            (1..=rule.len().min(comps.len())).any(|n| comps[comps.len() - n..] == rule[..n])
        })
    }
}

fn lower_components(path: &str) -> Vec<String> {
    path.split('/')
        .filter(|c| !c.is_empty())
        .map(str::to_lowercase)
        .collect()
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

    /// `target` excluded the way the defaults exclude it, plus the allow
    /// rules under test.
    fn allowing(allow: &[&str]) -> Rules {
        Rules::from_options(&ScanOptions {
            exclude_dirs: vec!["target".into()],
            allow: allow.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        })
    }

    #[test]
    fn a_sequence_takes_back_the_binaries_without_the_build() {
        let r = allowing(&["target/release"]);
        // The walk has to be let into `target` to reach `release`.
        assert!(
            r.excludes("/p/target", "target", true),
            "still excluded itself"
        );
        assert!(r.may_contain_allowed("/p/target"), "but walked into");
        // What was asked for comes back…
        assert!(!r.excludes("/p/target/release", "release", true));
        assert!(!r.excludes("/p/target/release/app", "app", false));
        // …and its siblings do not come with it, which is the whole point.
        assert!(r.excludes("/p/target/debug/app", "app", false));
        assert!(
            r.excludes("/p/target/release/deps", "deps", true),
            "nor its depths"
        );
        assert!(r.excludes("/p/target/.rustc_info.json", ".rustc_info.json", false));
        // Every project, not one: the rule names a shape, not a place.
        assert!(!r.excludes("/other/deep/target/release/bin", "bin", false));
    }

    #[test]
    fn a_path_allow_takes_back_one_place_only() {
        let r = allowing(&["/p/target/release"]);
        assert!(r.may_contain_allowed("/p/target"), "on the way");
        assert!(!r.excludes("/p/target/release/app", "app", false));
        assert!(r.excludes("/p/target/debug/app", "app", false));
        assert!(
            r.excludes("/q/target/release/app", "app", false),
            "another project"
        );
    }

    /// The rule as it is actually used: the platform's own exclusions, plus
    /// what the shipped configuration suggests.
    #[test]
    fn the_defaults_plus_one_line_bring_the_binaries_back() {
        let (paths, dirs, files) = platform_defaults();
        let r = Rules::from_options(&ScanOptions {
            exclude_paths: paths,
            exclude_dirs: dirs,
            exclude_files: files,
            allow: vec!["target/release".into(), "target/debug".into()],
            ..Default::default()
        });
        let bin = "/home/u/proj/target/release/app";
        assert!(r.may_contain_allowed("/home/u/proj/target"), "walked into");
        assert!(!r.excludes(bin, "app", false), "the binary is kept");
        assert!(!r.excludes_path(bin), "and the watcher agrees");
        assert!(
            r.excludes("/home/u/proj/target/release/deps", "deps", true),
            "and not the intermediate tree beside it"
        );
        assert!(r.excludes(
            "/home/u/proj/target/release/deps/lib.rlib",
            "lib.rlib",
            false
        ));
        assert!(r.excludes(
            "/home/u/proj/target/.rustc_info.json",
            ".rustc_info.json",
            false
        ));
    }

    #[test]
    fn the_watcher_agrees_without_being_told_what_is_a_directory() {
        let r = allowing(&["target/release"]);
        assert!(!r.excludes_path("/p/target/release/app"));
        assert!(r.excludes_path("/p/target/debug/app"));
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
