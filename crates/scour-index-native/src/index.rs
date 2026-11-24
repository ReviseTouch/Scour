//! The index, as the rest of Scour sees it.
//!
//! Everything below is a segment: written once, mapped, never edited. An upsert
//! is staged and invisible until [`Index::commit`]; a removal goes into an
//! overlay every search consults at once; a commit turns both into files.

mod commit;
mod doomed;
mod facets;
mod flush;
mod fold;
mod meta;
mod reach;
use commit::*;
use doomed::*;
use facets::*;
use flush::*;
#[cfg(test)]
use fold::FoldGate;
use meta::*;
use reach::*;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use parking_lot::RwLock;
use scour_core::{
    ApplyReport, Change, Entry, Error, Facet, FacetRequest, FacetResponse, Hit, Index, IndexStats,
    MaintReport, Maintenance, Result, ScanRequest, SearchRequest, SearchResponse, SourceId,
};

use crate::build::build;
use crate::columns::Field;
use crate::directory_bytes::{DirectoryBytes, dir_size};
use crate::durable::replace_synced;
use crate::ids::digest;
use crate::lock::DirLock;
use crate::search::{Plan, Segment, Wanted, run_with};
use crate::segment::Live;
use crate::usage::Rollup;

#[derive(Debug, Default)]
struct Inner {
    segments: Vec<Live>,
    /// The in-memory segment list or generation is newer than the manifest.
    /// Without it a retried commit succeeds and a restart loses the segment.
    meta_dirty: bool,
    /// Segments whose in-memory live bitmap is newer than the file on disk. A
    /// failed commit may have applied the removal to RAM already, where killing
    /// a dead row again correctly reports zero.
    dirty_alive: std::collections::HashSet<u64>,
    /// Segment files to erase after the manifest stops naming them, kept until
    /// a manifest publication succeeds.
    pending_erase: std::collections::HashSet<u64>,
    /// Upserts not yet written. Searchable only after a commit.
    staged: Vec<Entry>,
    /// Rows a walk found exactly as they already were, by segment number: a sweep
    /// deletes what the walk did not stamp, so without this a rescan of an
    /// untouched disk empties the index. One bit a row, 275 KB for two million.
    seen: HashMap<u64, Vec<u8>>,
    /// How many rows have been spared since the last time somebody was told.
    /// Counted at flush, so it reaches [`ApplyReport`] one batch late.
    spared: u64,
    /// Every source this index has been handed a row for. **`RemoveSubtree`
    /// carries a path and no source** and the identity table is keyed on both,
    /// so each is tried in turn. Learned rather than stored.
    sources: Vec<SourceId>,
    /// Where each staged path sits, so a second upsert of the same file
    /// replaces the first instead of adding a second row for it.
    staged_at: HashMap<u64, usize>,
    /// Paths whose removal has taken effect for searches but not yet for the
    /// files. A watcher reporting a deletion has a path and nothing else, and a
    /// prefix set holding one path removes exactly it: nothing is under a file.
    hidden_prefixes: scour_core::PrefixSet,
    generation: u64,
    /// A generation that has been handed out and not yet reconciled. Between a
    /// scan taking one and the sweep that ends it, folding across the boundary
    /// would hide rows from the judgement. Callers scan one at a time.
    open: Option<u64>,
    next_segment: u64,
    /// Segments that exist only in memory: published, searchable, and written
    /// down by the next [`NativeIndex::persist`]. The manifest never names them,
    /// so a crash loses their rows and the next walk finds them again.
    unwritten: std::collections::HashSet<u64>,
}

#[derive(Debug)]
pub struct NativeIndex {
    dir: PathBuf,
    /// Physical directory bytes cached between index-controlled mutations.
    disk_bytes: std::sync::Arc<DirectoryBytes>,
    inner: RwLock<Inner>,
    /// Segments in the air, and something to wait on. Separate from `inner`: a
    /// builder thread never takes the index lock, so waiting for a build cannot
    /// deadlock against the lock the build needs.
    building: std::sync::Arc<(parking_lot::Mutex<Building>, parking_lot::Condvar)>,
    /// Builds that failed. Their rows were put back; this is how the next
    /// commit finds out it has something to report.
    build_failed: std::sync::atomic::AtomicUsize,
    /// Folder sizes, derived and cached, under **their own lock**: building the
    /// prefix sums is 90 ms over two million rows, which under `inner` would stop
    /// every search for that long. Taken *after* `inner` on every path.
    sizes: parking_lot::RwLock<crate::sizes::Cache>,
    /// Held by whoever is writing published segments down. A client's flush
    /// and the service's own persist both wrote the same segment, and one
    /// truncated a file the other was opening: "`.alive` is 0 bytes".
    persisting: parking_lot::Mutex<()>,
    /// A deterministic test-only pause after a fold snapshots its inputs.
    #[cfg(test)]
    fold_gate: parking_lot::Mutex<Option<std::sync::Arc<FoldGate>>>,
    /// How many times a published segment has been written to disk.
    #[cfg(test)]
    unwritten_writes: std::sync::atomic::AtomicUsize,
    /// Released when this is dropped, or by the kernel if the process dies.
    /// Held for the lifetime of the index: every writing path goes through it.
    _lock: DirLock,
}

impl NativeIndex {
    /// The segments as they stand, for a test of another module.
    #[cfg(test)]
    pub(crate) fn with_segments<R>(&self, f: impl FnOnce(&[Live]) -> R) -> R {
        f(&self.inner.read().segments)
    }

    /// Open the index in `dir`, creating an empty one if there is none.
    pub fn open_or_create(dir: &Path) -> Result<NativeIndex> {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(&e, &dir.to_string_lossy()))?;
        // Before anything is read: a second writer here does not merely lose an
        // update, it calls `File::create` on a file the first one has mmapped.
        let lock = DirLock::acquire(dir)?;
        // **Only a missing manifest means a new index.** Any other error read as
        // `Meta::default()` says the index holds nothing, and `sweep_orphans`
        // then erases every segment on disk.
        let meta: Meta = match std::fs::read_to_string(dir.join(META_FILE)) {
            Ok(s) => serde_json::from_str(&s).map_err(|e| Error::IndexCorrupt {
                detail: format!("{META_FILE}: {e}"),
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Meta::default(),
            Err(e) => return Err(Error::io(&e, &dir.join(META_FILE).to_string_lossy())),
        };
        if meta.format != FORMAT {
            // Not damaged — written by another version, and nothing is lost:
            // every row came from the filesystem. `discard` is how to act on it.
            return Err(Error::IndexOutdated {
                found: meta.format,
                expected: FORMAT,
            });
        }
        let mut segments = Vec::with_capacity(meta.segments.len());
        for s in &meta.segments {
            segments.push(Live::open(dir, s.number, s.generation)?);
        }
        sweep_orphans(dir, &meta);
        let disk_bytes = std::sync::Arc::new(DirectoryBytes::new(dir));
        Ok(NativeIndex {
            dir: dir.to_owned(),
            disk_bytes,
            inner: RwLock::new(Inner {
                segments,
                generation: meta.generation,
                next_segment: meta.next_segment,
                ..Inner::default()
            }),
            building: std::sync::Arc::new((
                parking_lot::Mutex::new(Building::default()),
                parking_lot::Condvar::new(),
            )),
            build_failed: std::sync::atomic::AtomicUsize::new(0),
            sizes: parking_lot::RwLock::new(crate::sizes::Cache::default()),
            persisting: parking_lot::Mutex::new(()),
            #[cfg(test)]
            fold_gate: parking_lot::Mutex::new(None),
            #[cfg(test)]
            unwritten_writes: std::sync::atomic::AtomicUsize::new(0),
            _lock: lock,
        })
    }

    /// Throw away an index so the next `open_or_create` starts empty, for
    /// [`Error::IndexOutdated`] and nothing else. Removes the manifest and the
    /// segment files only: the directory comes from configuration.
    pub fn discard(dir: &Path) -> Result<()> {
        let entries = match std::fs::read_dir(dir) {
            Ok(e) => e,
            // Nothing to throw away is the state being asked for.
            Err(_) => return Ok(()),
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("seg-") || name == META_FILE {
                let p = entry.path();
                std::fs::remove_file(&p).map_err(|e| Error::io(&e, &p.to_string_lossy()))?;
            }
        }
        Ok(())
    }

    fn save_meta(&self, inner: &mut Inner) -> Result<()> {
        let meta = Meta {
            format: FORMAT,
            generation: inner.generation,
            next_segment: inner.next_segment,
            segments: inner
                .segments
                .iter()
                .filter(|s| !inner.unwritten.contains(&s.number))
                .map(|s| SegRef {
                    number: s.number,
                    generation: s.generation,
                })
                .collect(),
        };
        let json = serde_json::to_string_pretty(&meta).map_err(|e| Error::Io {
            detail: e.to_string(),
        })?;
        // Written beside itself and renamed over: `std::fs::write` truncates
        // first, and a zero-length manifest is an index that must be rebuilt.
        let p = self.dir.join(META_FILE);
        let saved = self
            .disk_bytes
            .changing(|| replace_synced(&p, json.as_bytes()));
        if let Err(error) = saved {
            inner.meta_dirty = true;
            return Err(error);
        }
        inner.meta_dirty = false;
        let erase = std::mem::take(&mut inner.pending_erase);
        self.disk_bytes.changing(|| {
            for number in erase {
                Live::erase(&self.dir, number);
            }
        });
        Ok(())
    }

    /// Hand each segment to `f`. For diagnostics that need to see inside.
    pub fn for_each_segment(&self, f: &mut dyn FnMut(usize, &Segment<'_>)) -> Result<()> {
        let inner = self.inner.read();
        for (i, live) in inner.segments.iter().enumerate() {
            f(i, &live.view()?);
        }
        Ok(())
    }

    /// Run `f` for every live row that the query accepts, across all segments;
    /// stops when `f` returns `false`. **Narrowed the same way a search is**, and
    /// `accepts` is handed the folded name rather than the spelled one.
    fn for_each_match(
        &self,
        inner: &Inner,
        query: &scour_core::Ast,
        mut f: impl FnMut(&mut Spelled, &Segment<'_>, usize) -> bool,
    ) -> Result<()> {
        for live in &inner.segments {
            let seg = live.view()?;
            // A segment's matches come in row order, so its names are read on
            // from the last one rather than from their block's start.
            let mut names = Spelled::default();
            let plan = Plan::compile(query, &seg)?;
            // The pending paths become directory-number ranges once per segment;
            // rebuilding a spelled path per match meant millions of allocations.
            let doomed = Doomed::new(&seg, &inner.hidden_prefixes);
            let go = crate::search::walk_matches(&seg, &plan, |row| {
                if !doomed.is_empty() && doomed.takes(&seg, row, seg.dir_id(row)) {
                    return true;
                }
                f(&mut names, &seg, row)
            });
            if !go {
                break;
            }
        }
        Ok(())
    }
}

impl Index for NativeIndex {
    fn apply(&self, changes: &mut dyn Iterator<Item = Change>) -> Result<ApplyReport> {
        let began = Instant::now();
        let mut inner = self.inner.write();
        let mut report = ApplyReport::default();
        for c in changes {
            match c {
                Change::Upsert(e) => {
                    if !inner.sources.contains(&e.id.source) {
                        inner.sources.push(e.id.source);
                    }
                    // A file that was removed and has come back must stop being
                    // hidden. Behind the emptiness test because asking a set
                    // about a path it does not hold still costs hashing it.
                    if !inner.hidden_prefixes.is_empty() {
                        inner.hidden_prefixes.forget(&e.path);
                    }
                    let d = digest(e.id.source, &e.path);
                    // `get`, not `[]`: the cost of being sure is nothing, and of
                    // being wrong a panic inside a write lock in a service.
                    match inner.staged_at.get(&d).copied() {
                        Some(i) if inner.staged.get(i).is_some_and(|s| s.path == e.path) => {
                            inner.staged[i] = e
                        }
                        _ => {
                            let at = inner.staged.len();
                            inner.staged_at.insert(d, at);
                            inner.staged.push(e);
                        }
                    }
                    report.upserted += 1;
                    if inner.staged.len() >= MAX_STAGED {
                        // The one place a build may happen elsewhere: a buffer
                        // overflowing during a walk, with nothing waiting on it.
                        self.flush_maybe_elsewhere(&mut inner, true)?;
                    }
                }
                Change::RemoveSubtree { path } => {
                    inner.hidden_prefixes.extend([path]);
                    report.subtrees_removed += 1;
                }
                // Walking the subtree again is the engine's job; what comes
                // back arrives here as ordinary upserts and a sweep.
                Change::Rescan { .. } => {}
            }
        }
        // Moved out of `upserted` rather than added beside it: a spared entry was
        // counted as an upsert on the way in and `seen()` adds the two.
        // Saturating because the spared batch is not always the staged one.
        let spared = std::mem::take(&mut inner.spared);
        report.upserted = report.upserted.saturating_sub(spared);
        report.unchanged = spared;
        if std::env::var_os("SCOUR_SPARE_TRACE").is_some() {
            let n = report.seen().max(1);
            scour_core::note!(
                "scourd: apply {n} kayit, {:.0?} ({:.2} us/kayit, sparing dahil)",
                began.elapsed(),
                began.elapsed().as_secs_f64() * 1e6 / n as f64,
            );
        }
        Ok(report)
    }

    fn begin_generation(&self) -> Result<u64> {
        // Rows still being written would be stamped with the generation they
        // were staged under and judged by the one that starts here.
        self.settle()?;
        let mut inner = self.inner.write();
        // Whatever was open is finished: callers scan one at a time. **Closed
        // before the flush**, or the flush spares rows into marks the `clear`
        // below throws away, leaving them unwritten *and* unstamped.
        inner.open = None;
        inner.seen.clear();
        // Flush second, so no segment ever spans two generations: that is what
        // lets a generation be one number a segment rather than a column a row.
        // Into memory, as a publish: a subtree walk starts dozens of times a
        // minute during a build, and each one wrote and synced a segment here
        // with the lock held.
        self.publish_locked(&mut inner)?;
        inner.generation += 1;
        let g = inner.generation;
        inner.open = Some(g);
        // Not saved now: every manifest records the generation it was written
        // in, so none can name a segment stamped above the one it records.
        inner.meta_dirty = true;
        Ok(g)
    }

    /// [`Index::forget`] for every source at once: each live row is killed
    /// through the same bitmaps, so a segment left with none goes with its files
    /// at the next manifest, and staged rows that never landed go too.
    fn clear(&self) -> Result<u64> {
        self.settle()?;
        let mut inner = self.inner.write();
        self.flush(&mut inner)?;
        let mut gone = 0u64;
        let mut touched = Vec::new();
        for live in inner.segments.iter_mut() {
            let mut any = false;
            for row in 0..live.rows() {
                if live.kill(row) {
                    gone += 1;
                    any = true;
                }
            }
            if any {
                touched.push(live.number);
            }
        }
        inner.hidden_prefixes = scour_core::PrefixSet::default();
        if gone > 0 {
            inner.dirty_alive.extend(touched);
            self.write_dirty_alive(&mut inner)?;
            self.forget_empty(&mut inner);
            self.save_meta(&mut inner)?;
        }
        Ok(gone)
    }

    fn forget(&self, source: SourceId) -> Result<u64> {
        self.settle()?;
        let mut inner = self.inner.write();
        self.flush(&mut inner)?;
        let mut gone = 0u64;
        let mut touched = vec![false; inner.segments.len()];
        for (i, live) in inner.segments.iter_mut().enumerate() {
            let victims: Vec<usize> = {
                let seg = live.view()?;
                (0..live.rows())
                    .filter(|&row| live.is_alive(row) && seg.source_of(row) == source)
                    .collect()
            };
            for row in victims {
                if live.kill(row) {
                    gone += 1;
                    touched[i] = true;
                }
            }
        }
        if gone > 0 {
            let dirty = inner
                .segments
                .iter()
                .enumerate()
                .filter_map(|(i, live)| touched[i].then_some(live.number))
                .collect::<Vec<_>>();
            inner.dirty_alive.extend(dirty);
            self.write_dirty_alive(&mut inner)?;
            self.forget_empty(&mut inner);
            self.save_meta(&mut inner)?;
        }
        Ok(gone)
    }

    fn sweep(
        &self,
        source: SourceId,
        under: &[String],
        generation: u64,
        spare: &scour_core::PrefixSet,
    ) -> Result<u64> {
        self.sweep_many(&[(source, under.to_vec())], generation, spare)
    }

    /// Every source of one pass at once: the notes of unchanged rows belong to
    /// the pass, so a second `sweep` after the first would find none and delete
    /// what the first source's notes did not cover.
    fn sweep_many(
        &self,
        parts: &[(SourceId, Vec<String>)],
        generation: u64,
        spare: &scour_core::PrefixSet,
    ) -> Result<u64> {
        self.sweep_paths(parts, generation, spare)
    }

    /// Close a generation that ended without a sweep — a cancelled walk, an
    /// unreadable root.
    fn abandon_generation(&self, generation: u64) -> Result<()> {
        let mut inner = self.inner.write();
        close_generation(&mut inner, generation);
        // The marks are worthless without the sweep that would have read them,
        // and a marked segment cannot be folded, which stops compaction.
        inner.seen.clear();
        // Recorded by the next persist, as a sweep's ending is.
        Ok(())
    }

    fn commit(&self) -> Result<()> {
        self.commit_now()
    }

    fn fresh(&self, generation: u64) -> u64 {
        // A pass writes nothing for a row it found unchanged, so the rows stamped
        // with its generation are exactly the ones it had to write.
        let inner = self.inner.read();
        inner
            .segments
            .iter()
            .filter(|s| s.generation == generation)
            .map(Live::live_rows)
            .sum()
    }

    fn release_memory(&self) {
        for live in &self.inner.read().segments {
            live.release_pages();
        }
        // And the folder totals, 20 MB on this machine once anything was
        // ordered by size: rebuilt in a third of a second when next asked,
        // which an idle service can afford and a small machine cannot the
        // other way round.
        self.sizes.write().clear();
        // And what a query's buffers left in glibc's arenas, freed but kept.
        trim_allocator();
    }

    fn publish(&self) -> Result<()> {
        // A large batch goes to disk as a commit would: memory holds a second's
        // trickle, not a walk.
        if self.inner.read().staged.len() > PUBLISH_MAX_STAGED {
            return self.commit();
        }
        self.publish_now()?;
        let held: usize = {
            let inner = self.inner.read();
            inner
                .segments
                .iter()
                .filter(|s| inner.unwritten.contains(&s.number))
                .map(Live::rows)
                .sum()
        };
        if held > UNWRITTEN_MAX_ROWS {
            self.persist_unwritten()?;
        }
        Ok(())
    }

    fn search(&self, req: &SearchRequest) -> Result<SearchResponse> {
        self.search_segments(req)
    }

    /// Every matching row, built one at a time and handed over. **The read lock is
    /// held throughout**, so a scan delays the watcher's next commit; releasing it
    /// between segments would let a commit renumber rows underneath the walk. Rows
    /// arrive in the index's own order, not a sorted one.
    fn scan(&self, req: &ScanRequest, f: &mut dyn FnMut(&Hit) -> bool) -> Result<u64> {
        let inner = self.inner.read();
        let mut rows = 0u64;
        self.for_each_match(&inner, &req.query, |names, seg, row| {
            let Some(name) = names.of(seg, row) else {
                // A row whose name cannot be read is not a row anybody can be
                // shown. Skipped rather than aborting the export.
                return true;
            };
            rows += 1;
            f(&seg.hit(row, name))
        })?;
        Ok(rows)
    }

    /// What each of these folders weighs and how many files it holds, from prefix
    /// sums over directory numbers — bytes **on disk**, hard links counted once,
    /// over what this index holds. See `sizes.rs`. Both locks, in this order every
    /// time: `inner` for reading, the cache for writing.
    fn subtree_sizes(&self, paths: &[String]) -> Result<Vec<Option<(u64, u64)>>> {
        let inner = self.inner.read();
        let mut cache = self.sizes.write();
        Ok(cache
            .subtrees(&inner.segments, paths)
            .into_iter()
            .map(Some)
            .collect())
    }

    /// Every question about the matching set, from **one** walk of it. A count, a
    /// breakdown by kind and a distribution by age were three walks of the same
    /// rows behind one keystroke: 100 to 200 ms on 2.1 M entries.
    fn facets(&self, req: &FacetRequest) -> Result<FacetResponse> {
        self.count_facets(req)
    }

    fn usage(&self, req: &scour_core::UsageRequest) -> Result<scour_core::UsageResponse> {
        let started = Instant::now();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let inner = self.inner.read();
        if let Some(mut children) = crate::usage::Children::new(req, now) {
            for live in &inner.segments {
                children.add_segment(&live.view()?)?;
            }
            return children.finish(started);
        }
        if let Some(mut everything) = crate::usage::Everything::new(req, now) {
            let mut whole = true;
            for live in &inner.segments {
                whole &= everything.add_segment(&live.view()?)?;
            }
            if whole {
                return everything.finish(started);
            }
        }
        let mut rollup = Rollup::new(req, now);
        for live in &inner.segments {
            rollup.add_segment(&live.view()?)?;
        }
        rollup.finish(started)
    }

    fn stats(&self) -> Result<IndexStats> {
        let inner = self.inner.read();
        let mut entries = 0u64;
        let mut dirs = 0u64;
        let mut largest = 0u64;
        // Read, not walked: the engine calls this once a second, and counting
        // directories row by row was 2.1 M rows a second under the read lock.
        for live in &inner.segments {
            let live_rows = live.live_rows();
            entries += live_rows;
            largest = largest.max(live_rows);
            dirs += live.dirs() as u64;
        }
        Ok(IndexStats {
            entries,
            dirs,
            bytes_on_disk: self.disk_bytes.get(),
            segments: inner.segments.len() as u32,
            // Everything outside the largest segment: what a search pays for
            // twice, once per segment that cannot stop early.
            unsorted_entries: entries - largest,
            pending_removals: inner.hidden_prefixes.len() as u64,
            // No extractor is registered yet. The column is reserved, not used.
            has_content: false,
            dir: self.dir.display().to_string(),
            written_at: std::fs::metadata(self.dir.join(META_FILE))
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs() as i64),
        })
    }

    fn maintain(&self, level: Maintenance) -> Result<MaintReport> {
        self.maintain_now(level)
    }
}

#[cfg(test)]
mod tests;
