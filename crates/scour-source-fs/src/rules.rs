//! What not to index.
//!
//! Three kinds of rule — a path prefix, a directory name anywhere, a file name
//! anywhere — plus `allow`, which overrules them. Defaults are data rather than
//! code in [`platform_defaults`], so a settings screen can show and edit them.

use scour_core::ScanOptions;

/// Compiled exclusion rules. Built once per scan, then asked per entry.
/// A rule containing `/` is a component sequence, not a name: `.git/objects`.
#[derive(Debug, Default, Clone)]
pub struct Rules {
    paths: Vec<String>,
    /// Rules naming one directory wherever it appears — a hash lookup per entry.
    dirs: std::collections::HashSet<String>,
    /// Rules naming a sequence — `.git/objects`. Matched at any component
    /// boundary, and everything below the match goes with it.
    dir_seqs: Vec<Vec<String>>,
    files: std::collections::HashSet<String>,
    allow: Vec<String>,
    /// Allow rules naming a sequence rather than a place — `target/release`,
    /// matched wherever it appears, so one line covers every project on the disk
    /// instead of one line per checkout.
    allow_seqs: Vec<Vec<String>>,
    /// Exclusions an allow rule may not overrule.
    /// The index's own directory: indexing what it writes cost 36% and 26% of two cores.
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
            // Anything that is not an absolute path is a sequence of directory names.
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
    /// For the scan, where the entry judged is the directory itself and is pruned.
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
    /// For the watcher, which is handed a path with no walk behind it.
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
    /// A path takes back a subtree; a sequence takes back that directory's own
    /// entries and no deeper — `target/release` is 41 entries holding all 5 binaries.
    fn allows(&self, path: &str, is_dir: bool) -> bool {
        if self.allow.iter().any(|a| under(path, a)) {
            return true;
        }
        if self.allow_seqs.is_empty() {
            return false;
        }
        // Only the tail can match, and it is read from the end: folding whole
        // paths cost 0.68 µs an entry against 0.03 for the other three kinds.
        let keep = self.allow_seqs.iter().map(Vec::len).max().unwrap_or(0) + 1;
        let mut buf = [""; MAX_TAIL];
        if keep > MAX_TAIL {
            // A rule deeper than anyone writes. Correctness over speed.
            let comps = lower_components(path);
            let n = comps.len();
            return self.allow_seqs.iter().any(|rule| {
                let l = rule.len();
                (n >= l && comps[n - l..] == rule[..])
                    || (!is_dir && n > l && comps[n - 1 - l..n - 1] == rule[..])
            });
        }
        // `tail[0]` is the last component, so a rule is compared reversed.
        let n = tail_of(path, keep, &mut buf);
        let tail = &buf[..n];
        self.allow_seqs.iter().any(|rule| {
            let l = rule.len();
            // The named directory itself, so it can be entered and listed.
            let matches = |from: usize| {
                rule.iter()
                    .rev()
                    .zip(tail[from..].iter())
                    .all(|(r, c)| eq_folded(c, r))
            };
            if n >= l && matches(0) {
                return true;
            }
            // A file directly inside it. Directories are left out on purpose:
            // `deps` under `release` is the noise, and refusing it lets the walk prune.
            !is_dir && n > l && matches(1)
        })
    }

    /// Is any *ancestor* of this path an excluded directory?
    /// Only asked when an allow rule exists: the walk is let into `target` to reach
    /// `target/release`, and everything else in there must still go.
    fn inside_excluded(&self, path: &str) -> bool {
        // One reused buffer, not one `String` a component; the list is a set.
        let mut fold = String::with_capacity(32);
        let mut it = path.split('/').filter(|c| !c.is_empty()).peekable();
        while let Some(c) = it.next() {
            // The last component is the entry itself, already judged by the caller.
            if it.peek().is_none() {
                break;
            }
            fold.clear();
            fold.extend(c.chars().flat_map(char::to_lowercase));
            if self.dirs.contains(&fold) {
                return true;
            }
        }
        self.seq_within(path)
    }

    /// Should this entry be skipped? `path` is already `/`-normalised.
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
    /// The same answer as [`Rules::excludes`] **without a syscall**, which is what
    /// a watcher needs: an `is_dir()` per event cost 74% of a core during a build.
    pub fn excludes_path(&self, path: &str) -> bool {
        if self.deny.iter().any(|d| under(path, d)) {
            return true;
        }
        // No `is_dir`: letting one event through costs a check, refusing one a stale row.
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
    /// A directory excluded by prefix may still contain an allowed subtree.
    pub fn may_contain_allowed(&self, path: &str) -> bool {
        if self.allow.iter().any(|a| under(a, path)) {
            return true;
        }
        if self.allow_seqs.is_empty() {
            return false;
        }
        // `target/release` has to let the walk into `target`: this path ends
        // with some head of the rule.
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

/// How deep a tail this can read without allocating. Longer than any sequence
/// rule anybody writes; a rule past it falls back to folding the path.
const MAX_TAIL: usize = 8;

/// The last components of a path, newest first, borrowed and unfolded.
/// [`eq_folded`] compares folded in place, so `rsplit` touches only the bytes it
/// returns; `tail[0]` is the last component, so rules are compared reversed.
fn tail_of<'a>(path: &'a str, keep: usize, buf: &mut [&'a str; MAX_TAIL]) -> usize {
    let mut n = 0;
    for c in path.rsplit('/').filter(|c| !c.is_empty()).take(keep) {
        buf[n] = c;
        n += 1;
    }
    n
}

/// Is `component` this rule component, ignoring case?
/// Rules are folded when built, so only one side folds, and into the comparison
/// rather than into a `String`. No length shortcut: `İ` folds to two characters.
fn eq_folded(component: &str, folded: &str) -> bool {
    component
        .chars()
        .flat_map(char::to_lowercase)
        .eq(folded.chars())
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
/// Two reasons: a virtual filesystem is not files and can block for ever, and
/// build output churns and buries real results.
pub fn platform_defaults() -> (Vec<String>, Vec<String>, Vec<String>) {
    // Android adds nothing to these, and an untouched `mut` is a warning.
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
        // The trash on a removable volume carries the uid of whoever emptied it there.
        // SAFETY: `getuid` reads a field of the process and cannot fail.
        let uid = unsafe { libc::getuid() };
        dirs.push(format!(".Trash-{uid}"));
    }
    #[cfg(target_os = "macos")]
    {
        paths.extend(["/dev", "/System/Volumes/Data/private", "/private/var/vm"].map(String::from));
        dirs.push(".Spotlight-V100".into());
        dirs.push(".fseventsd".into());
        dirs.push("Library/Caches".into());
    }
    #[cfg(windows)]
    {
        paths.extend(["C:/Windows/WinSxS", "C:/Windows/Temp"].map(String::from));
        // On every drive, not only C:, now that every fixed drive is a source.
        dirs.extend(["$Recycle.Bin", "System Volume Information"].map(String::from));
    }

    // Everywhere: churn, not content. `target` is 852,437 of 2,986,545 entries
    // here, and one `cargo test` queued 3,935 changes at 67% of a core. `build`,
    // `dist` and `out` stay out of it: those are names people use for real work.
    // `.cache` is what XDG defines as regenerable: 337,565 rows here, a third of
    // them browser and thumbnail churn. The home trash is a sequence, not a name.
    dirs.extend(
        [
            ".git/objects",
            "target",
            "node_modules",
            ".cache",
            ".local/share/Trash",
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
mod tests;
