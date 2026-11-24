//! The sources: which are watched, which walked, and the rules they are walked by.

use super::*;
use scour_core::{Change, Error, Hit, Result, ScanOptions, SourceInfo};
use std::sync::Arc;
use std::sync::atomic::Ordering;

impl Engine {
    pub fn sources(&self) -> Vec<SourceInfo> {
        self.shared.sources.iter().map(|s| s.describe()).collect()
    }

    /// How each source's last whole walk went, by source name. A source not yet
    /// walked in this run is absent.
    pub fn walks(&self) -> std::collections::HashMap<String, scour_core::Walked> {
        self.shared.walks.read().clone()
    }

    /// How each source is kept up to date, by its name: what a settings page
    /// says beside a disk or a folder.
    pub fn keeping(&self) -> std::collections::HashMap<String, Keeping> {
        let watches = self.shared.watches.lock();
        self.shared
            .sources
            .iter()
            .enumerate()
            .map(|(i, src)| {
                let live = watches
                    .iter()
                    .any(|(w, h)| *w == i && h.unwatched().is_empty());
                let changed = self.shared.changed[i].load(Ordering::Relaxed);
                let keeping = Keeping {
                    live,
                    changed: (changed > 0).then_some(changed),
                    walking: self.shared.walking[i].load(Ordering::Relaxed)
                        || self.shared.queued[i].load(Ordering::Relaxed),
                };
                (src.describe().name, keeping)
            })
            .collect()
    }

    /// Start watching every source that can be watched. One that cannot is not an
    /// error: it has no change feed and is reconciled by rescanning instead.
    pub fn start_watching(&self) -> Result<u32> {
        let mut started = 0;
        let scan = self.shared.scan();
        let mut handles = self.shared.watches.lock();
        for (i, src) in self.shared.sources.iter().enumerate() {
            if !src.caps().contains(scour_core::Caps::WATCH) {
                continue;
            }
            match src.watch(&scan, Box::new(Forward(self.changes.clone()))) {
                Ok(h) => {
                    handles.push((i, h));
                    started += 1;
                }
                // One source that will not watch does not stop the others.
                Err(_) => continue,
            }
        }
        self.shared.status.write().watching = started;
        Ok(started)
    }

    /// Subtrees no watcher is covering, gathered from every handle. Empty is the
    /// ordinary answer; when it is not, live updates are partial.
    pub fn unwatched(&self) -> Vec<String> {
        self.shared
            .watches
            .lock()
            .iter()
            .flat_map(|(_, h)| h.unwatched())
            .collect()
    }

    /// Watch everything, then walk it — a walk is a snapshot and a watch is
    /// everything after it, so walking first leaves a window covered by neither.
    pub fn cover_then_walk(&self, walk: bool) -> Result<(u32, Vec<String>)> {
        let started = self.start_watching()?;
        let skipped = self.unwatched();
        if walk {
            // A watch that picked up where the last one stopped has the changes
            // in between already: walking it again is minutes of a disk for
            // nothing. Not for an index that holds nothing yet.
            let cold = self.shared.status.read().cold;
            let caught_up: Vec<usize> = if cold {
                Vec::new()
            } else {
                self.shared
                    .watches
                    .lock()
                    .iter()
                    .filter(|(_, h)| h.caught_up())
                    .map(|(i, _)| *i)
                    .collect()
            };
            let all: Vec<usize> = (0..self.shared.sources.len())
                .filter(|i| !caught_up.contains(i))
                .collect();
            self.queue_all(all)?;
        }
        Ok((started, skipped))
    }

    /// Queue a full walk of every source, or of one subtree. A whole-source walk
    /// already queued is not queued again, and the flag clears when the walk starts.
    pub fn rescan(&self, subtree: Option<String>) -> Result<()> {
        match &subtree {
            Some(path) => {
                let idx = self
                    .owner_of(path)
                    .ok_or_else(|| Error::NotFound { path: path.clone() })?;
                // A source's own root, asked for by name — a refresh button —
                // is the source walked whole: a drive's whole walk can be read
                // from its master file table, a subtree's cannot.
                let roots = self.shared.sources[idx].describe().roots;
                let bare = |p: &str| p.trim_end_matches('/').to_lowercase();
                if roots.len() == 1 && bare(&roots[0]) == bare(path) {
                    return self.queue_whole(idx);
                }
                self.send(Job::Scan {
                    source: idx,
                    subtree,
                })
            }
            None => self.queue_all((0..self.shared.sources.len()).collect()),
        }
    }

    /// Queue whole walks of these sources as one job, so different disks are
    /// walked at once. A source already queued is left where it is.
    fn queue_all(&self, sources: Vec<usize>) -> Result<()> {
        let fresh: Vec<usize> = sources
            .into_iter()
            .filter(|&i| {
                self.shared
                    .queued
                    .get(i)
                    .is_some_and(|q| !q.swap(true, Ordering::AcqRel))
            })
            .collect();
        if fresh.is_empty() {
            return Ok(());
        }
        let sent = self.send(Job::ScanAll(fresh.clone()));
        if sent.is_err() {
            for i in fresh {
                self.shared.queued[i].store(false, Ordering::Release);
            }
        }
        sent
    }

    /// Queue a full walk of the source with this name: one just added.
    pub fn walk_source(&self, name: &str) -> Result<()> {
        let idx = self
            .shared
            .sources
            .iter()
            .position(|s| s.describe().name == name)
            .ok_or_else(|| Error::NotFound { path: name.into() })?;
        self.queue_whole(idx)
    }

    fn queue_whole(&self, i: usize) -> Result<()> {
        if self.shared.queued[i].swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let sent = self.send(Job::Scan {
            source: i,
            subtree: None,
        });
        if sent.is_err() {
            // Nothing will clear it, because nothing will run it.
            self.shared.queued[i].store(false, Ordering::Release);
        }
        sent
    }

    /// Look at these paths again, now: a watcher takes three to eight seconds, too
    /// long for a watched row. Re-reads only; paths outside every source are skipped.
    pub fn recheck(&self, paths: &[String]) -> Result<usize> {
        let sink = Forward(self.changes.clone());
        let mut done = 0;
        // Grouped by owner, because `recheck` is a source's method.
        let mut by_source: std::collections::BTreeMap<usize, Vec<String>> = Default::default();
        for path in paths {
            if let Some(idx) = self.owner_of(path) {
                by_source.entry(idx).or_default().push(path.clone());
            }
        }
        for (idx, group) in by_source {
            done += self.shared.sources[idx].recheck(&group, &sink);
        }
        Ok(done)
    }

    /// Which source owns this path?
    pub(super) fn owner_of(&self, path: &str) -> Option<usize> {
        owner_of(&self.shared, path)
    }

    /// What the walk and the watchers are skipping, right now: built-in, configured
    /// and window-added rules merged. A snapshot, not a view.
    pub fn scan_options(&self) -> Arc<ScanOptions> {
        self.shared.scan()
    }

    /// Skip by these from now on, and tell the watchers, or a rule would sweep a
    /// tree clean and the watcher would put it straight back. Does not scan.
    pub fn set_scan_options(&self, opts: ScanOptions) {
        let fresh = Arc::new(opts);
        *self.shared.scan.write() = Arc::clone(&fresh);
        // Under the lock the worker covers a subtree with: never both in one handle.
        for (_, handle) in self.shared.watches.lock().iter() {
            handle.retune(&fresh);
        }
    }

    /// Throw out everything the rules in force would now skip. No walk: excluding
    /// only removes, and removal is by subtree — 1.3 µs for 378,100 rows.
    pub fn apply_rules(&self) -> Result<u64> {
        let opts = self.shared.scan();
        // One per source. `None` means no exclusions, and its rows are left alone.
        let tests: Vec<Option<Box<dyn Fn(&str, bool) -> bool + Send + Sync>>> = self
            .shared
            .sources
            .iter()
            .map(|s| s.excluder(&opts))
            .collect();
        if tests.iter().all(Option::is_none) {
            return Ok(0);
        }

        let mut doomed: Vec<String> = Vec::new();
        self.shared.index.scan(
            &scour_core::ScanRequest {
                // No `..Default::default()`: a second field appearing must stop this compiling.
                query: scour_query::parse(""),
            },
            &mut |hit: &Hit| {
                // A `Hit` does not carry its source, and a path outside a source's
                // roots fails its root check before any rule is read.
                if tests
                    .iter()
                    .flatten()
                    .any(|excluded| excluded(hit.path.as_str(), hit.is_dir))
                {
                    doomed.push(hit.path.clone());
                }
                true
            },
        )?;

        // Folded to the topmost path of each excluded tree, one removal per tree.
        let doomed = coalesce(doomed);
        if std::env::var_os("SCOUR_TRACE_RULES").is_some() {
            for p in doomed.iter().take(8) {
                scour_core::note!("scourd: rule drops {p}");
            }
        }
        let n = doomed.len() as u64;
        if n > 0 {
            let mut changes = doomed
                .into_iter()
                .map(|path| Change::RemoveSubtree { path });
            self.shared.index.apply(&mut changes)?;
            self.shared.index.commit()?;
            self.shared.touched_now();
        }
        Ok(n)
    }
}
