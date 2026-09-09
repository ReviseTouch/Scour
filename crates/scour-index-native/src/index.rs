//! The index, as the rest of Scour sees it.
//!
//! Everything below is a segment: written once, mapped, never edited. An upsert
//! is staged and invisible until [`Index::commit`]; a removal goes into an
//! overlay every search consults at once; a commit turns both into files.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use parking_lot::RwLock;
use scour_core::{
    ApplyReport, Change, Entry, Error, Facet, FacetBy, FacetRequest, FacetResponse, Hit, Index,
    IndexStats, Kind, MaintReport, Maintenance, Result, ScanRequest, SearchRequest, SearchResponse,
    SourceId,
};
use serde::{Deserialize, Serialize};

use crate::build::{build, build_sorted};
use crate::columns::Field;
use crate::directory_bytes::{DirectoryBytes, dir_size};
use crate::durable::replace_synced;
use crate::ids::digest;
use crate::lock::DirLock;
use crate::rank::{LiveRank, first_at_or_below};
use crate::search::{Plan, Segment, Wanted, run_with};
use crate::segment::Live;
use crate::usage::Rollup;

/// How many entries may wait in memory before a segment is written.
/// 100k is ~25 MB of `Entry` and leaves eleven segments after a million: a
/// search at 5.15 ms against 1.02 until a 2.7 s compaction.
const MAX_STAGED: usize = 100_000;

/// How many rows a facet count will look at before it answers approximately.
/// A facet is a sidebar, not a result. Nobody waits for it.
const FACET_SCAN_CAP: usize = 200_000;

/// …except a distribution, which has to see everything. Rows are stored newest
/// first, so a cap takes a prefix rather than a sample, and a capped histogram
/// of ages says "today" whatever was asked.
const AGE_SCAN_CAP: usize = usize::MAX;

const META_FILE: &str = "native-index.json";
/// Bumped when the files change shape **or when a stored value changes what it
/// means**. 2 added the trigram filter; 3 the per-block minimum and maximum; 4 a
/// distance byte a directory; 5 the file kinds, where nothing changed shape at
/// all and every older row still decodes, into the wrong answer; 6 the folded
/// name arena, without which a search has nothing to walk; 7 took the block from
/// 128 rows to 32, and every offset in every file is relative to it; 8 made a
/// row's identity its path — three identity columns went and the lookup table is
/// keyed on the path; 9 added the link count, without which a disk-usage report
/// counts a hard-linked file once per name.
const FORMAT: u32 = 9;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
struct SegRef {
    number: u64,
    generation: u64,
}

#[derive(Debug, Serialize, Deserialize)]
struct Meta {
    format: u32,
    generation: u64,
    next_segment: u64,
    segments: Vec<SegRef>,
}

impl Default for Meta {
    fn default() -> Self {
        Meta {
            format: FORMAT,
            generation: 0,
            next_segment: 1,
            segments: Vec::new(),
        }
    }
}

/// What a flush left for the caller to write once it has let go of the index.
/// **It holds the only copy of those rows**, so every path that can fail after
/// this point hands it to [`NativeIndex::restore`] rather than dropping it.
#[derive(Debug, Default)]
struct Pending {
    /// The new segment's number, generation and rows.
    staged: Option<(u64, u64, Vec<Entry>)>,
    /// Live bits to replace, by segment number.
    alive: Vec<(u64, Vec<u8>)>,
    /// The removals this flush was going to make durable.
    prefixes: scour_core::PrefixSet,
}

impl Pending {
    fn write_alive(&self, dir: &Path, disk_bytes: &DirectoryBytes) -> Result<()> {
        disk_bytes.changing(|| {
            for (number, bits) in &self.alive {
                Live::write_alive(dir, *number, bits)?;
            }
            Ok(())
        })
    }
}

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
}

/// A segment being built on another thread, as invisible as staged rows. It
/// carries the set of paths it holds: a removal arriving while it is in the air
/// has no row to kill yet.
#[derive(Debug, Default)]
struct Flight {
    number: u64,
    /// The path digests this segment will hold.
    paths: std::collections::HashSet<u64>,
    /// What to kill in it the moment it lands, in `kill_paths` order.
    doomed: Vec<(u32, SourceId, String)>,
    /// Subtrees removed while it was in the air. Applied on landing for the
    /// same reason: the rows were not there to be killed.
    gone: scour_core::PrefixSet,
}

/// A build that has finished.
#[derive(Debug)]
enum Landed {
    /// The files are written and synced; the segment needs a place in the list.
    Built { number: u64, live: Live },
    /// The rows could not be written. They come back rather than being lost —
    /// see [`Pending`].
    Failed {
        number: u64,
        rows: Vec<Entry>,
        detail: String,
    },
}

#[derive(Debug, Default)]
struct Building {
    /// Handed to a thread and not yet finished.
    flights: Vec<Flight>,
    /// Finished, waiting for someone holding the write lock to install them.
    landed: Vec<Landed>,
}

/// How many segments may be built at once. Each holds its rows and its output
/// bytes — about 35 MB for a full segment — so this is a memory bound; past it
/// the caller builds the flush itself.
const MAX_BUILDING: usize = 4;

/// How much larger a segment has to be than everything below it before it stops
/// being folded with them — `K` in size-tiered arithmetic, see [`head_of`]. Two
/// would allow 24 segments at 5 M rows instead of 13, and a search pays per
/// segment: 1.11 ms for one against 5.20 for eleven at 1.08 M entries.
const TIER_RATIO: u64 = 4;

/// Split segments into size tiers, smallest first: `(number, rows)` walked by
/// ascending size, a new tier at the first member larger than [`TIER_RATIO`]
/// times everything below it. Ties break on the number, not the manifest order.
fn size_tiers(sizes: impl IntoIterator<Item = (u64, u64)>) -> Vec<Vec<u64>> {
    let mut sizes: Vec<(u64, u64)> = sizes.into_iter().collect();
    sizes.sort_by_key(|&(number, rows)| (rows, number));
    let mut tiers: Vec<Vec<u64>> = Vec::new();
    let mut below = 0u64;
    for (number, rows) in sizes {
        match tiers.last_mut() {
            Some(tier) if rows <= TIER_RATIO.saturating_mul(below) => {
                tier.push(number);
                below += rows;
            }
            _ => {
                tiers.push(vec![number]);
                below = rows;
            }
        }
    }
    tiers
}

/// One candidate segment, as [`head_of`] sees it: a number and two counts.
#[derive(Clone, Copy, Debug)]
struct Member {
    number: u64,
    rows: u64,
    dead: u64,
}

/// Which of a group of segments to fold together next, or nothing. **The head is
/// the smallest size tier, not the whole of the rest**, so the tail absorbs a
/// trickle every ~113k rows of churn rather than once a minute, and the count
/// stays under `ceil(log4(rows)) + 1`: 13 at 5 M rows.
fn head_of(members: &[Member]) -> Option<Vec<u64>> {
    let biggest = members.iter().max_by_key(|m| m.rows)?;
    // A quarter of it dead is the point at which rewriting the largest segment
    // gives back more than it costs, and everything riding along is then cheap.
    if biggest.dead * 4 > biggest.rows {
        return Some(members.iter().map(|m| m.number).collect());
    }
    let rest: Vec<&Member> = members
        .iter()
        .filter(|m| m.number != biggest.number)
        .collect();
    let tiers = size_tiers(rest.iter().map(|m| (m.number, m.rows)));
    tiers.into_iter().find(|tier| tier.len() >= 2).or_else(|| {
        // Every tier a single segment: nothing to merge, but a segment a quarter
        // dead still pays for its own rewrite. `fold` accepts a group of one.
        rest.into_iter()
            .find(|m| m.dead * 4 > m.rows)
            .map(|m| vec![m.number])
    })
}

#[cfg(test)]
#[derive(Debug)]
struct FoldGate {
    snapshot_ready: std::sync::Barrier,
    resume: std::sync::Barrier,
}

#[cfg(test)]
impl FoldGate {
    fn new() -> FoldGate {
        FoldGate {
            snapshot_ready: std::sync::Barrier::new(2),
            resume: std::sync::Barrier::new(2),
        }
    }
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
    /// A deterministic test-only pause after a fold snapshots its inputs.
    #[cfg(test)]
    fold_gate: parking_lot::Mutex<Option<std::sync::Arc<FoldGate>>>,
    /// Released when this is dropped, or by the kernel if the process dies.
    /// Held for the lifetime of the index: every writing path goes through it.
    _lock: DirLock,
}

impl NativeIndex {
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
            #[cfg(test)]
            fold_gate: parking_lot::Mutex::new(None),
            _lock: lock,
        })
    }

    #[cfg(test)]
    fn gate_next_fold(&self, gate: std::sync::Arc<FoldGate>) {
        *self.fold_gate.lock() = Some(gate);
    }

    #[cfg(test)]
    fn pause_fold_before_publish(&self) {
        let gate = self.fold_gate.lock().take();
        if let Some(gate) = gate {
            gate.snapshot_ready.wait();
            gate.resume.wait();
        }
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

    /// Turn everything staged and hidden into files, lock held throughout, for
    /// callers whose next line needs the rows in. Removals are applied *before*
    /// the new segment: one of them is the old row of every re-upserted entry.
    fn flush(&self, inner: &mut Inner) -> Result<()> {
        self.flush_maybe_elsewhere(inner, false)
    }

    fn flush_maybe_elsewhere(&self, inner: &mut Inner, elsewhere: bool) -> Result<()> {
        // `elsewhere` says this flush happens because a buffer filled up, not
        // because a caller needs the rows in.
        let mut pending = self.flush_prepare(inner, elsewhere)?;
        if let Err(e) = pending.write_alive(&self.dir, &self.disk_bytes) {
            Self::restore(inner, pending);
            return Err(e);
        }
        let Some((number, generation, staged)) = pending.staged.take() else {
            return Ok(());
        };

        // **Somewhere else, if anybody else is free.** A hundred thousand rows
        // are ~150 ms of one thread and segments are independent. Past
        // `MAX_BUILDING` the caller builds it itself, which is the back pressure.
        let mine = !elsewhere || {
            let held = self.building.0.lock();
            held.flights.len() >= MAX_BUILDING
        };
        if mine {
            let bytes = build(&staged);
            #[cfg(feature = "memory-trace")]
            if std::env::var_os("SCOUR_BUILD_TRACE").is_some() {
                eprintln!(
                    "scourd: builder {number}: entries {:.1} MiB; segment buffers {:.1} MiB (worker)",
                    entry_storage(&staged) as f64 / 1_048_576.0,
                    segment_storage(&bytes) as f64 / 1_048_576.0,
                );
            }
            let written = self
                .disk_bytes
                .changing(|| Live::write(&self.dir, number, generation, &bytes));
            drop(bytes);
            let live = match written {
                Ok(live) => live,
                Err(e) => {
                    pending.staged = Some((number, generation, staged));
                    Self::restore(inner, pending);
                    trim_allocator();
                    return Err(e);
                }
            };
            drop(staged);
            // The builder buffers are the process's largest anonymous allocation,
            // and glibc keeps freed pages until this arena is trimmed.
            trim_builder_allocator();
            inner.segments.push(live);
            inner.segments.sort_by_key(|s| s.number);
            inner.meta_dirty = true;
            // Past here the rows are on disk: the segment is named by the next
            // successful save, or swept as an orphan, so nothing is put back.
            return self.save_meta(inner);
        }

        let paths = staged
            .iter()
            .map(|e| digest(e.id.source, &e.path))
            .collect::<std::collections::HashSet<_>>();
        #[cfg(feature = "memory-trace")]
        if std::env::var_os("SCOUR_BUILD_TRACE").is_some() {
            eprintln!(
                "scourd: flight {number}: {} rows = {:.1} MiB; path set {} slots",
                staged.len(),
                entry_storage(&staged) as f64 / 1_048_576.0,
                paths.capacity(),
            );
        }
        self.building.0.lock().flights.push(Flight {
            number,
            paths,
            doomed: Vec::new(),
            gone: scour_core::PrefixSet::default(),
        });
        let dir = self.dir.clone();
        let disk_bytes = std::sync::Arc::clone(&self.disk_bytes);
        let building = std::sync::Arc::clone(&self.building);
        let started = std::thread::Builder::new()
            .name("scour-build".into())
            .spawn(move || {
                let bytes = build(&staged);
                #[cfg(feature = "memory-trace")]
                if std::env::var_os("SCOUR_BUILD_TRACE").is_some() {
                    eprintln!(
                        "scourd: builder {number}: entries {:.1} MiB; segment buffers {:.1} MiB",
                        entry_storage(&staged) as f64 / 1_048_576.0,
                        segment_storage(&bytes) as f64 / 1_048_576.0,
                    );
                }
                let written = disk_bytes.changing(|| Live::write(&dir, number, generation, &bytes));
                drop(bytes);
                let done = match written {
                    Ok(live) => {
                        drop(staged);
                        // Trimming while this short-lived thread still owns its
                        // arena releases the pages a later trim leaves mapped.
                        trim_builder_allocator();
                        Landed::Built { number, live }
                    }
                    Err(e) => Landed::Failed {
                        number,
                        rows: staged,
                        detail: e.to_string(),
                    },
                };
                let mut held = building.0.lock();
                held.landed.push(done);
                building.1.notify_all();
            });
        if let Err(e) = started {
            // No thread to be had. Build it here rather than lose the rows.
            self.building
                .0
                .lock()
                .flights
                .retain(|f| f.number != number);
            return Err(Error::Io {
                detail: format!("no thread for a segment build: {e}"),
            });
        }
        Ok(())
    }

    /// Remove paths that name a file, touching no other row; returns the prefixes
    /// that still need the scan. A watcher reports every removal as a subtree and
    /// almost all are one file, where scanning was 10.5 ms at half a million rows
    /// and 64.9 ms at four million against a binary search on the identity table.
    fn kill_leaves<'p>(
        live: &mut Live,
        prefixes: &'p scour_core::PrefixSet,
        sources: &[SourceId],
    ) -> Result<(u64, scour_core::PrefixSet)> {
        let mut keep: Vec<String> = Vec::new();
        let mut wanted: Vec<(u32, SourceId, &'p str)> = Vec::new();
        {
            let seg = live.view()?;
            for p in prefixes.iter() {
                let trimmed = p.trim_end_matches('/');
                // No source yet means no way to key a lookup, so the scan has to
                // answer it. Dropping it would lose the removal outright.
                if sources.is_empty() || trimmed.is_empty() || !seg.dirs.subtree(p).is_empty() {
                    keep.push(p.to_owned());
                    continue;
                }
                for &source in sources {
                    wanted.push((crate::ids::IdMap::key_of(source, trimmed), source, trimmed));
                }
            }
        }
        let rest = scour_core::PrefixSet::new(keep);
        if wanted.is_empty() {
            return Ok((0, rest));
        }
        wanted.sort_unstable_by_key(|(k, _, _)| *k);
        let gone = live.kill_paths(&wanted)?;
        Ok((gone, rest))
    }

    fn kill_under(live: &mut Live, prefixes: &scour_core::PrefixSet) -> Result<u64> {
        let victims: Vec<usize> = {
            let seg = live.view()?;
            let doomed = Doomed::new(&seg, prefixes);
            if doomed.is_empty() {
                Vec::new()
            } else {
                let rows = live.rows();
                let mut out = Vec::new();
                for block in 0..rows.div_ceil(crate::columns::BLOCK) {
                    // A block whose directory numbers all fall outside every
                    // prefix holds nothing this removal is about.
                    if let Some((lo, hi)) = seg.cols.block_range(Field::DirId, block)
                        && lo >= 0
                        && !doomed.touches(lo as u32, hi as u32)
                    {
                        continue;
                    }
                    let from = block * crate::columns::BLOCK;
                    let to = (from + crate::columns::BLOCK).min(rows);
                    out.extend((from..to).filter(|&row| {
                        live.is_alive(row) && doomed.takes(&seg, row, seg.dir_id(row))
                    }));
                }
                out
            }
        };
        let mut gone = 0;
        for row in victims {
            if live.kill(row) {
                gone += 1;
            }
        }
        Ok(gone)
    }

    /// Put every finished build in the list; the caller holds the write lock.
    /// A segment sitting in `landed` exists on disk and answers no query.
    fn collect(&self, inner: &mut Inner) -> Result<()> {
        let done: Vec<Landed> = {
            let mut held = self.building.0.lock();
            std::mem::take(&mut held.landed)
        };
        if done.is_empty() {
            if inner.meta_dirty {
                self.save_meta(inner)?;
            }
            return Ok(());
        }
        let mut added = false;
        let mut failure = None;
        let mut done = done.into_iter();
        while let Some(one) = done.next() {
            match one {
                Landed::Built { number, mut live } => {
                    // Whatever was removed while this was in the air. The rows
                    // did not exist to be killed then and do now.
                    let (doomed, gone) = {
                        let held = self.building.0.lock();
                        held.flights
                            .iter()
                            .find(|flight| flight.number == number)
                            .map(|flight| (flight.doomed.clone(), flight.gone.clone()))
                            .unwrap_or_default()
                    };
                    let installed = (|| -> Result<()> {
                        if !doomed.is_empty() {
                            let mut wanted: Vec<(u32, SourceId, &str)> = doomed
                                .iter()
                                .map(|(key, source, path)| (*key, *source, path.as_str()))
                                .collect();
                            wanted.sort_unstable_by_key(|(key, _, _)| *key);
                            live.kill_paths(&wanted)?;
                        }
                        if !gone.is_empty() {
                            Self::kill_under(&mut live, &gone)?;
                        }
                        // On a retry both calls report zero, so the presence of
                        // the late removals — not today's hit count — is what
                        // says the bitmap needs its durable replacement.
                        if !doomed.is_empty() || !gone.is_empty() {
                            let (number, bits) = live.alive_snapshot();
                            self.disk_bytes
                                .changing(|| Live::write_alive(&self.dir, number, &bits))?;
                        }
                        Ok(())
                    })();
                    if let Err(error) = installed {
                        let mut held = self.building.0.lock();
                        held.landed.push(Landed::Built { number, live });
                        held.landed.extend(done);
                        if added {
                            inner.segments.sort_by_key(|segment| segment.number);
                        }
                        self.building.1.notify_all();
                        return Err(error);
                    }
                    self.building
                        .0
                        .lock()
                        .flights
                        .retain(|flight| flight.number != number);
                    inner.next_segment = inner.next_segment.max(number + 1);
                    inner.segments.push(live);
                    inner.meta_dirty = true;
                    added = true;
                }
                Landed::Failed {
                    number,
                    rows,
                    detail,
                } => {
                    self.building
                        .0
                        .lock()
                        .flights
                        .retain(|f| f.number != number);
                    self.build_failed
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    Self::restore(
                        inner,
                        Pending {
                            staged: Some((number, inner.generation, rows)),
                            ..Pending::default()
                        },
                    );
                    failure = Some(detail);
                }
            }
        }
        if added {
            inner.segments.sort_by_key(|s| s.number);
            let saved = self.save_meta(inner);
            self.building.1.notify_all();
            saved?;
        } else {
            self.building.1.notify_all();
        }
        match failure {
            Some(detail) => Err(Error::Io { detail }),
            None => Ok(()),
        }
    }

    /// Wait until nothing is being built, then put the results in the list.
    /// **Every operation that reasons about what the index contains does this
    /// first**: a segment in the air is rows a sweep or fold would silently miss.
    fn settle(&self) -> Result<()> {
        {
            let mut held = self.building.0.lock();
            while !held.flights.is_empty() && held.landed.len() < held.flights.len() {
                self.building.1.wait(&mut held);
            }
        }
        self.collect(&mut self.inner.write())
    }

    /// Put back what a failed publication was carrying. The staged rows go in
    /// **behind** whatever arrived meanwhile and only where that has not already
    /// replaced them: a path appears once.
    fn restore(inner: &mut Inner, pending: Pending) {
        for (number, _) in &pending.alive {
            if inner.segments.iter().any(|live| live.number == *number) {
                inner.dirty_alive.insert(*number);
            }
        }
        let prefixes = std::mem::take(&mut inner.hidden_prefixes);
        inner.hidden_prefixes = pending.prefixes;
        inner.hidden_prefixes.extend(prefixes.into_paths());
        // An upsert that arrived while a commit was writing is newer than the
        // removal being restored.
        for entry in &inner.staged {
            inner.hidden_prefixes.forget(&entry.path);
        }
        let Some((_, _, staged)) = pending.staged else {
            return;
        };
        let newer = std::mem::take(&mut inner.staged);
        inner.staged = staged;
        inner.staged_at.clear();
        for (i, e) in inner.staged.iter().enumerate() {
            inner.staged_at.insert(digest(e.id.source, &e.path), i);
        }
        for e in newer {
            let d = digest(e.id.source, &e.path);
            match inner.staged_at.get(&d).copied() {
                Some(i) if inner.staged[i].path == e.path => inner.staged[i] = e,
                _ => {
                    inner.staged_at.insert(d, inner.staged.len());
                    inner.staged.push(e);
                }
            }
        }
    }

    /// Lift every bitmap that is newer than its file out for one durable write.
    /// Taking the stamps matters: a newer removal can dirty the same segment
    /// while these snapshots are in flight, and its stamp must survive.
    fn take_dirty_alive(inner: &mut Inner) -> Vec<(u64, Vec<u8>)> {
        let dirty = std::mem::take(&mut inner.dirty_alive);
        inner
            .segments
            .iter()
            .filter(|live| dirty.contains(&live.number))
            .map(Live::alive_snapshot)
            .collect()
    }

    /// Persist dirty bitmaps while the caller holds the write lock. A failed
    /// replacement leaves the stamps behind, so the next invocation retries even
    /// though killing an already-dead row reports zero.
    fn write_dirty_alive(&self, inner: &mut Inner) -> Result<()> {
        let alive = Self::take_dirty_alive(inner);
        if alive.is_empty() {
            return Ok(());
        }
        let numbers: Vec<u64> = alive.iter().map(|(number, _)| *number).collect();
        if let Err(error) = (Pending {
            alive,
            ..Pending::default()
        })
        .write_alive(&self.dir, &self.disk_bytes)
        {
            for number in numbers {
                if inner.segments.iter().any(|live| live.number == number) {
                    inner.dirty_alive.insert(number);
                }
            }
            return Err(error);
        }
        Ok(())
    }

    /// Lift the staged entries out, leaving the index consistent without them.
    /// They were never searchable, so the segment can be built with no lock.
    fn take_staged(inner: &mut Inner) -> Option<(u64, u64, Vec<Entry>)> {
        if inner.staged.is_empty() {
            return None;
        }
        let number = inner.next_segment;
        inner.next_segment += 1;
        inner.meta_dirty = true;
        let staged = std::mem::take(&mut inner.staged);
        inner.staged.shrink_to_fit();
        inner.staged_at.clear();
        inner.staged_at.shrink_to_fit();
        Some((number, inner.generation, staged))
    }

    /// Everything a flush does **except** building and writing the segment, so a
    /// caller that can let go of the lock does. The order inside matters: the
    /// identities to kill are read *from* the staged entries.
    fn flush_prepare(&self, inner: &mut Inner, discretionary: bool) -> Result<Pending> {
        // Anything that finished building belongs in the list first: a row that
        // has just landed is a row this flush may have to replace.
        self.collect(inner)?;
        if inner.staged.is_empty()
            && inner.hidden_prefixes.is_empty()
            && inner.dirty_alive.is_empty()
        {
            if inner.meta_dirty {
                self.save_meta(inner)?;
            }
            return Ok(Pending::default());
        }
        let mut touched = vec![false; inner.segments.len()];

        // Subtrees, one pass over each segment for every prefix at once, by
        // **directory number** rather than by path: the directory table answers
        // it as a range check, where paths meant 2.1 M constructions per delete.
        if !inner.hidden_prefixes.is_empty() {
            let prefixes = std::mem::take(&mut inner.hidden_prefixes);
            let sources = inner.sources.clone();
            for (i, live) in inner.segments.iter_mut().enumerate() {
                // Files first, by identity, and only what is left over — a
                // real directory — pays for a walk.
                let (leaves, rest) = Self::kill_leaves(live, &prefixes, &sources)?;
                let mut gone = leaves;
                if !rest.is_empty() {
                    gone += Self::kill_under(live, &rest)?;
                }
                if gone > 0 {
                    touched[i] = true;
                }
            }
            // A segment still being built has none of these rows yet. It carries
            // the prefixes and applies them the moment it lands.
            {
                let mut held = self.building.0.lock();
                for f in held.flights.iter_mut() {
                    f.gone.extend(prefixes.iter().map(str::to_owned));
                }
            }
            inner.staged.retain(|e| !prefixes.covers(&e.path));
            inner.hidden_prefixes = prefixes;
        }

        // The old row of everything being re-upserted, **by path**: keying on an
        // inode left 267 rows at one path. Sorted once and merged in one pass a
        // segment, where a binary search per path per segment spent most of a
        // hundred seconds on ten million entries.
        spare_unchanged(inner)?;
        {
            let Inner {
                staged, segments, ..
            } = &mut *inner;
            let mut wanted: Vec<(u32, SourceId, &str)> = staged
                .iter()
                .map(|e| {
                    (
                        crate::ids::IdMap::key_of(e.id.source, &e.path),
                        e.id.source,
                        e.path.as_str(),
                    )
                })
                .collect();
            wanted.sort_unstable_by_key(|(h, _, _)| *h);
            for (i, live) in segments.iter_mut().enumerate() {
                if live.kill_paths(&wanted)? > 0 {
                    touched[i] = true;
                }
            }
            // The same rows, in segments that do not exist yet: a path
            // re-indexed while an earlier segment holding it is still being
            // written would have two rows the moment that segment landed.
            let mut held = self.building.0.lock();
            if !held.flights.is_empty() {
                for f in held.flights.iter_mut() {
                    for (key, source, path) in &wanted {
                        if f.paths.contains(&digest(*source, path)) {
                            f.doomed.push((*key, *source, (*path).to_owned()));
                        }
                    }
                }
            }
        }
        let t_kill = Instant::now();

        // **A buffer that emptied itself has nothing to flush.** On a rescan
        // almost every entry leaves through `spare_unchanged` above, and writing
        // the rest turned an untouched disk into nine segments of two rows each.
        let pending = if discretionary && inner.staged.len() < MAX_STAGED {
            None
        } else {
            Self::take_staged(inner)
        };
        let _ = t_kill;
        // Copied, not written. The write is an `fsync` a segment and happens
        // once a second; doing it here held the index for 33 to 56 ms.
        for (i, live) in inner.segments.iter().enumerate() {
            if touched.get(i).copied().unwrap_or(false) {
                inner.dirty_alive.insert(live.number);
            }
        }
        let mut alive = Self::take_dirty_alive(inner);
        let prefixes = std::mem::take(&mut inner.hidden_prefixes);
        // The manifest stops naming these segments *before* their files go. The
        // other way round leaves a window in which it names files that are gone,
        // and the index will not open again.
        let gone = self.forget_empty(inner);
        if let Err(e) = self.save_meta(inner) {
            Self::restore(
                inner,
                Pending {
                    staged: pending,
                    alive,
                    prefixes,
                },
            );
            return Err(e);
        }
        // **A segment swept empty is both touched and gone**, and the caller
        // writes the snapshot after releasing the lock, which puts its file back:
        // 182 orphan segments on the live index against 55 in the manifest.
        alive.retain(|(n, _)| !gone.contains(n));
        Ok(Pending {
            staged: pending,
            alive,
            prefixes,
        })
    }

    /// Erase segments nothing is left alive in: an emptied segment left in the
    /// list is walked end to end by every query, 1,204,270 rows for nothing.
    fn forget_empty(&self, inner: &mut Inner) -> Vec<u64> {
        let mut gone = Vec::new();
        inner.segments.retain(|s| {
            if s.rows() > 0 && s.live_rows() == 0 {
                gone.push(s.number);
                false
            } else {
                true
            }
        });
        if !gone.is_empty() {
            inner.meta_dirty = true;
            inner.pending_erase.extend(gone.iter().copied());
        }
        gone
    }

    /// Fold these segments into one, dropping rows that are no longer live —
    /// **without holding the index against a search**. A segment is written once
    /// and never edited, so the build runs under the **read** lock; held
    /// throughout, a query issued during a rebuild waited **22,984 ms**.
    fn fold(&self, which_numbers: &[u64]) -> Result<bool> {
        // **Not while a walk is running, and only for the segments being folded**:
        // marks are keyed on the number a row's segment had, and a fold renumbers
        // what it consumes. **`false`, not `Ok(())`** — a refusal that reads as
        // success leaves the compaction loop spinning at 99.7% of a core.
        let marked = {
            let inner = self.inner.read();
            which_numbers.iter().any(|n| inner.seen.contains_key(n))
        };
        if marked {
            return Ok(false);
        }
        // **The number is claimed under the write lock, before anything is built.**
        // Reading `next_segment` under the read lock reserves nothing: a
        // concurrent commit writes its files over the ones this fold has mapped.
        let number = {
            let mut inner = self.inner.write();
            let n = inner.next_segment;
            inner.next_segment += 1;
            inner.meta_dirty = true;
            n
        };
        let (generation, snapshot, bytes) = {
            let inner = self.inner.read();
            let segs: Vec<&Live> = inner
                .segments
                .iter()
                .filter(|s| which_numbers.contains(&s.number))
                .collect();
            // Nothing here worth rewriting. `false` for the same reason as
            // above: the caller loops until a fold stops changing anything.
            if segs.len() < 2 && segs.iter().all(|s| s.dead_rows() == 0) {
                return Ok(false);
            }
            // The merged segment can carry only one stamp, so the group must
            // share one or be known entirely current — see [`Inner::open`].
            let generation = segs.iter().map(|s| s.generation).max().unwrap_or(0);
            // Immutable bytes cannot change while the read lock is released, but
            // a live bitmap can: publishing needs the same death stamps.
            let snapshot: Vec<(u64, u64)> = segs
                .iter()
                .map(|segment| (segment.number, segment.deaths()))
                .collect();
            let views: Vec<Segment<'_>> = segs.iter().map(|s| s.view()).collect::<Result<_>>()?;
            let bytes =
                build_sorted(&mut |emit: &mut dyn FnMut(&Entry)| merge_rows(&segs, &views, emit));
            (generation, snapshot, bytes)
        };

        #[cfg(test)]
        self.pause_fold_before_publish();

        // The segment is written before the lock is taken: it is a new file
        // that nothing names yet, so nobody can be reading it.
        let mut folded = if bytes.names.is_empty() || bytes.alive.is_empty() {
            None
        } else {
            Some(
                self.disk_bytes
                    .changing(|| Live::write(&self.dir, number, generation, &bytes))?,
            )
        };
        drop(bytes);

        let mut inner = self.inner.write();
        let current = snapshot.len() == which_numbers.len()
            && snapshot.iter().all(|&(number, deaths)| {
                inner
                    .segments
                    .iter()
                    .find(|segment| segment.number == number)
                    .is_some_and(|segment| segment.deaths() == deaths)
            });
        if !current {
            // A commit changed an input after the snapshot: publishing would
            // bring a removed or replaced row back. Drop the mappings first.
            drop(inner);
            drop(folded.take());
            self.disk_bytes.changing(|| Live::erase(&self.dir, number));
            trim_allocator();
            return Ok(false);
        }
        // Between the read and the write another commit may have appended a
        // segment. Take a number that is still free and fold only what is there.
        inner.next_segment = inner.next_segment.max(number + 1);
        let old: Vec<u64> = inner
            .segments
            .iter()
            .filter(|s| which_numbers.contains(&s.number))
            .map(|s| s.number)
            .collect();
        inner
            .segments
            .retain(|s| !which_numbers.contains(&s.number));
        inner.segments.extend(folded);
        inner.meta_dirty = true;
        inner.pending_erase.extend(old.iter().copied());
        // Newest last is what the search loop and `merge_rows` both assume; a
        // fold has to leave the order it found.
        inner.segments.sort_by_key(|s| s.number);
        self.save_meta(&mut inner)?;
        drop(inner);
        trim_allocator();
        Ok(true)
    }

    /// Segments grouped by generation, **by number rather than by position**: a
    /// fold releases the lock while it builds, and eight segments can become nine
    /// underneath it.
    fn groups(inner: &Inner) -> Vec<Vec<u64>> {
        // **The stamp only constrains a fold while a scan is open**: outside one
        // there is no sweep coming and the next generation starts above them all.
        // Grouping by it always left 241 segments across 205 generations.
        if inner.open.is_none() {
            return vec![inner.segments.iter().map(|s| s.number).collect()];
        }
        let mut by_gen: HashMap<u64, Vec<u64>> = HashMap::new();
        for s in &inner.segments {
            by_gen.entry(s.generation).or_default().push(s.number);
        }
        let mut out: Vec<Vec<u64>> = by_gen.into_values().collect();
        out.sort_by_key(|g| std::cmp::Reverse(g.len()));
        out
    }

    /// The next group of segments worth folding, or nothing, answered in numbers
    /// so the caller can let go of the index before it builds. See [`head_of`].
    fn next_head(&self) -> Option<Vec<u64>> {
        let inner = self.inner.read();
        let group = Self::groups(&inner).into_iter().find(|g| g.len() >= 3)?;
        let members: Vec<Member> = group
            .iter()
            .map(|&number| {
                let (rows, dead) = inner
                    .segments
                    .iter()
                    .find(|s| s.number == number)
                    .map_or((0, 0), |s| (s.rows() as u64, s.dead_rows()));
                Member { number, rows, dead }
            })
            .collect();
        head_of(&members)
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
        mut f: impl FnMut(&Segment<'_>, usize) -> bool,
    ) -> Result<()> {
        for live in &inner.segments {
            let seg = live.view()?;
            let plan = Plan::compile(query, &seg)?;
            // The pending paths become directory-number ranges once per segment;
            // rebuilding a spelled path per match meant millions of allocations.
            let doomed = Doomed::new(&seg, &inner.hidden_prefixes);
            let go = crate::search::walk_matches(&seg, &plan, |row| {
                if !doomed.is_empty() && doomed.takes(&seg, row, seg.dir_id(row)) {
                    return true;
                }
                f(&seg, row)
            });
            if !go {
                break;
            }
        }
        Ok(())
    }
}

/// Which rows of one segment a batch of subtree removals takes. Asking every row
/// about every path measured **2.24 s** for four thousand paths over a million
/// rows with the write lock held, so they become two sorted lookups a segment:
/// the directory numbers below them, and the ones named by parent and name.
struct Doomed<'a> {
    /// Half-open ranges of directory numbers, disjoint and sorted.
    inside: Vec<(u32, u32)>,
    /// `(parent number, name)`, sorted by parent.
    named: Vec<(u32, &'a str)>,
}

impl<'a> Doomed<'a> {
    fn new(seg: &Segment<'_>, prefixes: &'a scour_core::PrefixSet) -> Doomed<'a> {
        let mut inside: Vec<(u32, u32)> = Vec::new();
        let mut named: Vec<(u32, &'a str)> = Vec::new();
        for p in prefixes.iter() {
            let scope = seg.dirs.subtree(p);
            if let Some(own) = scope.own {
                inside.push((own, own + 1));
            }
            if !scope.below.is_empty() {
                inside.push((scope.below.start, scope.below.end));
            }
            if let Some((parent, name)) = p.trim_end_matches('/').rsplit_once('/') {
                let parent = if parent.is_empty() { "/" } else { parent };
                if let Some(pd) = seg.dirs.exact(parent) {
                    named.push((pd, name));
                }
            }
        }
        inside.sort_unstable();
        let mut merged: Vec<(u32, u32)> = Vec::with_capacity(inside.len());
        for (start, end) in inside {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        named.sort_unstable();
        Doomed {
            inside: merged,
            named,
        }
    }

    fn is_empty(&self) -> bool {
        self.inside.is_empty() && self.named.is_empty()
    }

    fn takes(&self, seg: &Segment<'_>, row: usize, dir: u32) -> bool {
        let at = self.inside.partition_point(|&(_, end)| end <= dir);
        if self.inside.get(at).is_some_and(|&(start, _)| start <= dir) {
            return true;
        }
        if self.named.is_empty() {
            return false;
        }
        // The name is read only for rows sitting directly in a directory that
        // something was removed from, which is a handful even during a delete.
        let at = self.named.partition_point(|&(parent, _)| parent < dir);
        if self.named.get(at).is_none_or(|&(parent, _)| parent != dir) {
            return false;
        }
        let Some(name) = seg.names.get(row) else {
            return false;
        };
        self.named[at..]
            .iter()
            .take_while(|&&(parent, _)| parent == dir)
            .any(|&(_, n)| n == name)
    }

    /// Could any row in a block of directory numbers `lo..=hi` be taken? **The
    /// zone map, used for a removal the way a search uses it**: scanning every row
    /// to delete one file was 4.58 ms over 250,000 rows and 64.14 ms over four
    /// million, against 535 µs for an upsert.
    fn touches(&self, lo: u32, hi: u32) -> bool {
        if self.inside.iter().any(|&(s, e)| s <= hi && e > lo) {
            return true;
        }
        let at = self.named.partition_point(|&(parent, _)| parent < lo);
        self.named.get(at).is_some_and(|&(parent, _)| parent <= hi)
    }
}

/// Take out of the batch every entry the index already holds, exactly as it is.
/// Asked here rather than in [`Index::apply`], which sees one entry at a time: a
/// probe per entry per segment measured slower than writing the rows. The row is
/// marked in [`Inner::seen`] so the sweep knows the walk saw it.
fn spare_unchanged(inner: &mut Inner) -> Result<u64> {
    let began = Instant::now();
    // Only inside a generation: outside one nothing needs the mark, and the
    // marks are cleared when a generation opens.
    if inner.open.is_none() || inner.staged.is_empty() || inner.segments.is_empty() {
        return Ok(0);
    }
    let mut drop_at = vec![false; inner.staged.len()];
    let mut marks: HashMap<u64, Vec<u8>> = HashMap::new();
    let mut spared = 0u64;
    {
        let Inner {
            staged, segments, ..
        } = &mut *inner;
        let mut keys: Vec<(u32, u32)> = staged
            .iter()
            .enumerate()
            .map(|(i, e)| (crate::ids::IdMap::key_of(e.id.source, &e.path), i as u32))
            .collect();
        keys.sort_unstable();
        let mut decided = vec![false; staged.len()];
        let mut hits: Vec<(u32, usize)> = Vec::new();
        for live in segments.iter() {
            hits.clear();
            live.spare_paths(&keys, staged, &mut decided, &mut hits)?;
            if hits.is_empty() {
                continue;
            }
            let bits = marks
                .entry(live.number)
                .or_insert_with(|| vec![0u8; live.rows().div_ceil(8)]);
            for &(at, row) in &hits {
                drop_at[at as usize] = true;
                if let Some(b) = bits.get_mut(row / 8) {
                    *b |= 1 << (row % 8);
                }
            }
            spared += hits.len() as u64;
        }
    }
    // Both numbers, because a ratio that falls is either the disk changing or
    // this deciding wrongly, and the segment count says which.
    if std::env::var_os("SCOUR_SPARE_TRACE").is_some() {
        let n = drop_at.len().max(1);
        scour_core::note!(
            "scourd: spare {spared} / {} across {} segments, {:.0?} ({:.2} us/kayit)",
            drop_at.len(),
            inner.segments.len(),
            began.elapsed(),
            began.elapsed().as_secs_f64() * 1e6 / n as f64,
        );
    }
    if spared == 0 {
        return Ok(0);
    }
    // Onto whatever earlier batches of the same generation already marked.
    for (number, bits) in marks {
        let held = inner
            .seen
            .entry(number)
            .or_insert_with(|| vec![0u8; bits.len()]);
        if held.len() < bits.len() {
            held.resize(bits.len(), 0);
        }
        for (h, b) in held.iter_mut().zip(bits) {
            *h |= b;
        }
    }
    let mut i = 0usize;
    inner.staged.retain(|_| {
        let keep = !drop_at[i];
        i += 1;
        keep
    });
    // Rebuilt rather than adjusted: the positions all moved, and a stale one
    // would have a later upsert overwrite an unrelated entry.
    inner.staged_at.clear();
    for (i, e) in inner.staged.iter().enumerate() {
        inner.staged_at.insert(digest(e.id.source, &e.path), i);
    }
    inner.spared += spared;
    Ok(spared)
}

/// A row's path, as bytes, without a string being made of it. The join, not the
/// two parts: `("/a", "c")` is less than `("/a-x", "b")` as a pair, and `/a-x/b`
/// is less than `/a/c` as a path.
fn joined_path<'a>(dir: &'a str, name: &'a str) -> impl Iterator<Item = u8> + 'a {
    let (head, slash) = match dir {
        "" => ("", false),
        "/" => ("/", false),
        d => (d, true),
    };
    head.bytes()
        .chain(slash.then_some(b'/'))
        .chain(name.bytes())
}

/// A row that might be on the page, ordered but not built: four numbers and a
/// key, against a `Hit`'s reconstructed path and eleven fields. A deep page holds
/// a hundred thousand of these on the way to sixty rows.
struct Candidate {
    key: crate::search::SortValue,
    /// What breaks a tie on the key: newest first, as `sort_hits` does it.
    mtime: i64,
    seg: u32,
    row: u32,
}

/// One segment's position in a merge.
struct Cursor {
    entry: Entry,
    seg: usize,
    row: usize,
}

// Ordered so that a max-heap yields the *first* row of the merged order:
// newest first, ties broken by path, which is the order `build` produces.
impl Ord for Cursor {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.entry
            .meta
            .mtime
            .cmp(&other.entry.meta.mtime)
            .then_with(|| other.entry.path.cmp(&self.entry.path))
    }
}
impl PartialOrd for Cursor {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl PartialEq for Cursor {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}
impl Eq for Cursor {}

/// Below this offset a page is walked to: the walk is a few milliseconds near
/// the top of a result, where the reach costs a rank table to be built.
const REACH_FROM: usize = 2_000;

/// How far the merge will step through a group of rows that share a second
/// before giving up and letting the walk do it. There is no rank *within* a tie,
/// and the bound keeps the reach from ever being the slower path.
const TIE_STEPS: usize = 100_000;

/// Whether to keep every page on the walk, for measuring the reach against it.
/// `SCOUR_NO_REACH=1`, read once.
fn walk_only() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("SCOUR_NO_REACH").is_some())
}

/// A page **reached** rather than walked to. The walk's cost is everything above
/// the page — 105 ms and 2,080,974 rows visited for the two hundred at offset two
/// million — where the start is a binary search plus a rank over a bitmap. `None`
/// for any shape this cannot answer, and the caller then walks.
#[allow(clippy::too_many_arguments)]
fn reach(
    segments: &[Live],
    views: &[Segment<'_>],
    offset: usize,
    limit: usize,
    cap: usize,
    started: Instant,
) -> Option<SearchResponse> {
    // Live rows before any given row, per segment. Not kept: a rank one deletion
    // stale returns a page from the wrong place.
    let ranks: Vec<LiveRank> = segments
        .iter()
        .zip(views)
        .map(|(live, seg)| LiveRank::build(seg.alive, live.rows()))
        .collect();
    let counted: usize = ranks
        .iter()
        .zip(views)
        .map(|(rank, seg)| rank.total(seg.alive))
        .sum();
    let empty = |visited: u64| SearchResponse {
        hits: Vec::new(),
        total: (counted as u64).min(cap as u64),
        capped: counted >= cap,
        took_us: started.elapsed().as_micros() as u64,
        fast_path: true,
        rows_visited: visited,
        rows_built: 0,
        misread: Vec::new(),
    };
    if offset >= counted {
        return Some(empty(0));
    }

    // How many live rows are newer than `t`. Two reads a segment: where the
    // column crosses `t`, and how many rows before that are live.
    let newer = |t: i64| -> usize {
        segments
            .iter()
            .zip(views)
            .zip(&ranks)
            .map(|((live, seg), rank)| {
                let at = first_at_or_below(live.rows(), t, |row| seg.num_of(Field::Mtime, row));
                rank.upto(seg.alive, at)
            })
            .sum()
    };

    // The date of the row at `offset`: the smallest `t` with no more than
    // `offset` rows above it, by bisection. The bracket is the newest date in
    // **any** segment and the oldest in any, not the newest they share.
    let mut low = i64::MAX;
    let mut high = i64::MIN;
    for (live, seg) in segments.iter().zip(views) {
        if live.rows() > 0 {
            high = high.max(seg.num_of(Field::Mtime, 0));
            low = low.min(seg.num_of(Field::Mtime, live.rows() - 1));
        }
    }
    if low > high {
        return Some(empty(0));
    }
    while low < high {
        let mid = low + (high - low) / 2;
        if newer(mid) <= offset {
            high = mid;
        } else {
            low = mid + 1;
        }
    }
    let when = high;
    let above = newer(when);
    // Rows of that same date that still precede the page. There is no rank
    // inside a tie, so these are stepped through — see [`TIE_STEPS`].
    let mut skip = offset.checked_sub(above)?;
    if skip > TIE_STEPS {
        return None;
    }

    // Where each segment's part of the page begins: its first row not newer
    // than `when`.
    let mut at: Vec<usize> = segments
        .iter()
        .zip(views)
        .map(|(live, seg)| {
            first_at_or_below(live.rows(), when, |row| seg.num_of(Field::Mtime, row))
        })
        .collect();
    for (i, cursor) in at.iter_mut().enumerate() {
        *cursor = next_live(views, segments, i, *cursor);
    }

    // The merge holds one row a segment, ordered as `sort_hits` orders it:
    // newest first, ties broken by paths compared as if joined.
    let mut dirs: HashMap<(usize, u32), String> = HashMap::new();
    let mut hits: Vec<Hit> = Vec::with_capacity(limit);
    let mut visited = 0u64;
    while hits.len() < limit {
        let mut best: Option<usize> = None;
        for i in 0..views.len() {
            if at[i] >= segments[i].rows() {
                continue;
            }
            let Some(b) = best else {
                best = Some(i);
                continue;
            };
            if first_of(views, &mut dirs, (i, at[i]), (b, at[b])) {
                best = Some(i);
            }
        }
        let Some(i) = best else { break };
        let row = at[i];
        visited += 1;
        if skip > 0 {
            skip -= 1;
        } else {
            let seg = &views[i];
            if let Some(name) = seg.names.get(row) {
                hits.push(seg.hit(row, name));
            }
        }
        at[i] = next_live(views, segments, i, row + 1);
    }

    Some(SearchResponse {
        rows_built: hits.len() as u64,
        hits,
        rows_visited: visited,
        ..empty(0)
    })
}

/// The next live row at or after `from`, or one past the end.
fn next_live(views: &[Segment<'_>], segments: &[Live], i: usize, from: usize) -> usize {
    let rows = segments[i].rows();
    (from..rows)
        .find(|&row| views[i].is_alive(row))
        .unwrap_or(rows)
}

/// Whether `a` comes before `b` in the merged stored order.
fn first_of(
    views: &[Segment<'_>],
    dirs: &mut HashMap<(usize, u32), String>,
    a: (usize, usize),
    b: (usize, usize),
) -> bool {
    let mtime = |(seg, row): (usize, usize)| views[seg].num_of(Field::Mtime, row);
    match mtime(a).cmp(&mtime(b)) {
        std::cmp::Ordering::Greater => return true,
        std::cmp::Ordering::Less => return false,
        std::cmp::Ordering::Equal => {}
    }
    // The same date. Inside one segment the row number is the path order, so
    // there is nothing to compare.
    if a.0 == b.0 {
        return a.1 < b.1;
    }
    for (seg, row) in [a, b] {
        let key = (seg, views[seg].dir_id(row));
        dirs.entry(key)
            .or_insert_with(|| views[seg].dirs.get(key.1).unwrap_or_default());
    }
    let joined = |(seg, row): (usize, usize)| {
        let dir = dirs[&(seg, views[seg].dir_id(row))].as_str();
        let name = views[seg].names.get(row).unwrap_or_default();
        joined_path(dir, name)
    };
    joined(a).lt(joined(b))
}

/// Emit every live row of every segment, in the merged stored order. Each
/// segment is already in it, so the heap holds one row a segment.
fn merge_rows(segs: &[&Live], views: &[Segment<'_>], emit: &mut dyn FnMut(&Entry)) {
    use std::collections::BinaryHeap;

    let next = |i: usize, from: usize| -> Option<(Entry, usize)> {
        (from..segs[i].rows())
            .find(|&r| segs[i].is_alive(r))
            .and_then(|r| views[i].entry(r).map(|e| (e, r)))
    };

    let mut heap: BinaryHeap<Cursor> = BinaryHeap::with_capacity(segs.len());
    for i in 0..segs.len() {
        if let Some((entry, row)) = next(i, 0) {
            heap.push(Cursor { entry, seg: i, row });
        }
    }
    while let Some(c) = heap.pop() {
        emit(&c.entry);
        if let Some((entry, row)) = next(c.seg, c.row + 1) {
            heap.push(Cursor {
                entry,
                seg: c.seg,
                row,
            });
        }
    }
}

/// Give freed memory back to the operating system. A fold builds a whole segment
/// in memory and drops it, and glibc keeps the freed arena in its own pools
/// rather than returning it. A no-op off Linux/glibc.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn trim_allocator() {
    unsafe extern "C" {
        fn malloc_trim(pad: usize) -> i32;
    }
    unsafe {
        malloc_trim(0);
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn trim_allocator() {}

fn trim_builder_allocator() {
    #[cfg(feature = "memory-trace")]
    if std::env::var_os("SCOUR_NO_BUILDER_TRIM").is_some() {
        return;
    }
    trim_allocator();
}

#[cfg(feature = "memory-trace")]
fn entry_storage(entries: &Vec<Entry>) -> usize {
    entries.capacity() * std::mem::size_of::<Entry>()
        + entries
            .iter()
            .map(|entry| entry.path.capacity())
            .sum::<usize>()
}

#[cfg(feature = "memory-trace")]
fn segment_storage(bytes: &crate::build::SegmentBytes) -> usize {
    bytes.names.capacity()
        + bytes.fnames.capacity()
        + bytes.cols.capacity()
        + bytes.dirs.capacity()
        + bytes.ids.capacity()
        + bytes.tri_dict.capacity()
        + bytes.tri_post.capacity()
        + bytes.alive.capacity()
        + bytes.porder.capacity()
        + bytes.norder.capacity()
        + bytes.eorder.capacity()
}

#[cfg(feature = "memory-trace")]
#[derive(Clone, Copy)]
struct CommitStamp {
    wall: Instant,
    cpu: std::time::Duration,
}

#[cfg(feature = "memory-trace")]
impl CommitStamp {
    fn now() -> CommitStamp {
        CommitStamp {
            wall: Instant::now(),
            cpu: thread_cpu(),
        }
    }

    fn since(self, earlier: CommitStamp) -> (f64, f64) {
        (
            self.wall.duration_since(earlier.wall).as_secs_f64() * 1_000.0,
            self.cpu.saturating_sub(earlier.cpu).as_secs_f64() * 1_000.0,
        )
    }
}

#[cfg(all(feature = "memory-trace", target_os = "linux"))]
fn thread_cpu() -> std::time::Duration {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) } == 0 {
        std::time::Duration::new(time.tv_sec as u64, time.tv_nsec as u32)
    } else {
        std::time::Duration::ZERO
    }
}

#[cfg(all(feature = "memory-trace", not(target_os = "linux")))]
fn thread_cpu() -> std::time::Duration {
    std::time::Duration::ZERO
}

#[cfg(feature = "memory-trace")]
fn trace_commit(
    rows: usize,
    start: CommitStamp,
    settled: CommitStamp,
    prepared: CommitStamp,
    alive: CommitStamp,
    built: CommitStamp,
    written: CommitStamp,
    published: CommitStamp,
) {
    if std::env::var_os("SCOUR_COMMIT_TRACE").is_none() {
        return;
    }
    let phase = |a: CommitStamp, b: CommitStamp| {
        let (wall, cpu) = b.since(a);
        format!("{wall:.2}/{cpu:.2}")
    };
    eprintln!(
        "scourd: commit {rows} rows, wall/CPU ms: settle {} prepare {} alive {} build {} files {} manifest {} total {}",
        phase(start, settled),
        phase(settled, prepared),
        phase(prepared, alive),
        phase(alive, built),
        phase(built, written),
        phase(written, published),
        phase(start, published),
    );
}

/// Remove segment files the manifest does not name. A crash between the first of
/// a segment's files and the last leaves a partial set nothing will open: **182
/// orphan segments against 55 real ones**, counted by `bytes_on_disk`. Safe by
/// construction — the manifest is written before any file is unlinked.
fn sweep_orphans(dir: &Path, meta: &Meta) {
    let named: std::collections::HashSet<u64> = meta.segments.iter().map(|s| s.number).collect();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(number) = name
            .strip_prefix("seg-")
            .and_then(|r| r.split('.').next())
            .and_then(|n| n.parse::<u64>().ok())
        else {
            continue;
        };
        // Only what is already behind the manifest's next number: a segment
        // being written right now is not worth racing.
        if !named.contains(&number) && number < meta.next_segment {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Close a scan's compaction cohort before ordinary changes resume, or every
/// later watcher commit keeps the scan's stamp and compaction sees body and
/// trickle as one group: four one-row commits rewrote a 100,000-row segment,
/// **477 ms of CPU**, once a minute on an idle machine.
fn close_generation(inner: &mut Inner, generation: u64) {
    if inner.open == Some(generation) {
        inner.open = None;
        inner.generation = inner.generation.saturating_add(1);
        inner.meta_dirty = true;
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
        self.flush(&mut inner)?;
        inner.generation += 1;
        inner.meta_dirty = true;
        let g = inner.generation;
        inner.open = Some(g);
        self.save_meta(&mut inner)?;
        Ok(g)
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
        let began = Instant::now();
        // A segment in the air holds rows this generation stamped; sweeping
        // before it lands judges them by a walk that never saw them.
        self.settle()?;
        let mut inner = self.inner.write();
        self.flush(&mut inner)?;
        close_generation(&mut inner, generation);
        let mut gone = 0u64;
        // **Taken once, for every root of the walk.** These marks belong to the
        // pass rather than to any one root: taken per root, the live index
        // deleted three of its four roots on alternate walks.
        let inner_seen = std::mem::take(&mut inner.seen);
        let mut touched = vec![false; inner.segments.len()];
        for (i, live) in inner.segments.iter_mut().enumerate() {
            if live.generation >= generation {
                continue;
            }
            let victims: Vec<usize> = {
                let seg = live.view()?;
                // One root that is the whole tree makes every other root
                // redundant, which is what `whole` has always meant.
                let whole = under.iter().any(|p| p.is_empty() || p == "/") || under.is_empty();
                let scopes: Vec<_> = under.iter().map(|p| seg.dirs.subtree(p)).collect();
                // The swept directory's **own** row is not under itself: it lives
                // in its parent and carries the parent's number, so the range
                // check walks past it and the name answers for those few rows.
                let owns: Vec<(u32, &str)> = if whole {
                    Vec::new()
                } else {
                    under
                        .iter()
                        .filter_map(|p| {
                            let p = p.trim_end_matches('/');
                            let (parent, name) = p.rsplit_once('/')?;
                            let parent = if parent.is_empty() { "/" } else { parent };
                            Some((seg.dirs.exact(parent)?, name))
                        })
                        .collect()
                };
                // **No path is built per row, and that is the whole cost of this
                // loop.** A row's directory number *is* its parent, so a
                // descendant is in `scope.below` and a child is `scope.own`.
                if !whole && scopes.iter().all(|s| s.is_empty()) && owns.is_empty() {
                    Vec::new()
                } else {
                    // Rows the walk found unchanged are stamped here rather than
                    // by being rewritten. See `Inner::seen`.
                    let seen = inner_seen.get(&live.number);
                    let spared = |row: usize| {
                        seen.is_some_and(|bits: &Vec<u8>| {
                            bits.get(row / 8).is_some_and(|b| b & (1 << (row % 8)) != 0)
                        })
                    };
                    (0..live.rows())
                        .filter(|&row| {
                            if !live.is_alive(row) {
                                return false;
                            }
                            // The walk saw it and it had not changed, so it
                            // was not rewritten. That is a stamp.
                            if spared(row) {
                                return false;
                            }
                            // **Another source's rows are not this walk's to
                            // judge.** A sweep speaks about one source, and where
                            // roots overlap it was applied to both.
                            if seg.source_of(row) != source {
                                return false;
                            }
                            // Somewhere the walk could not look. Its rows are
                            // not evidence of anything, so they stay.
                            if !spare.is_empty()
                                && spare
                                    .covers(&seg.path(row, seg.names.get(row).unwrap_or_default()))
                            {
                                return false;
                            }
                            if whole {
                                return true;
                            }
                            let d = seg.dir_id(row);
                            scopes.iter().any(|s| s.contains(d))
                                || owns
                                    .iter()
                                    .any(|(pd, name)| *pd == d && seg.names.get(row) == Some(*name))
                        })
                        .collect()
                }
            };
            for row in victims {
                if live.kill(row) {
                    gone += 1;
                    touched[i] = true;
                }
            }
        }
        let dirty = inner
            .segments
            .iter()
            .enumerate()
            .filter_map(|(i, live)| touched[i].then_some(live.number))
            .collect::<Vec<_>>();
        inner.dirty_alive.extend(dirty);
        if let Err(error) = self.write_dirty_alive(&mut inner) {
            inner.seen = inner_seen;
            return Err(error);
        }
        // Same order as `flush`: forget, record, then unlink.
        self.forget_empty(&mut inner);
        if let Err(error) = self.save_meta(&mut inner) {
            inner.seen = inner_seen;
            return Err(error);
        }
        if std::env::var_os("SCOUR_SPARE_TRACE").is_some() {
            scour_core::note!("scourd: sweep {gone} satir sildi, {:.0?}", began.elapsed());
        }
        Ok(gone)
    }

    /// Close a generation that ended without a sweep — a cancelled walk, an
    /// unreadable root.
    fn abandon_generation(&self, generation: u64) -> Result<()> {
        let mut inner = self.inner.write();
        close_generation(&mut inner, generation);
        // The marks are worthless without the sweep that would have read them,
        // and a marked segment cannot be folded, which stops compaction.
        inner.seen.clear();
        self.save_meta(&mut inner)
    }

    fn commit(&self) -> Result<()> {
        #[cfg(feature = "memory-trace")]
        let trace_start = CommitStamp::now();
        // The bookkeeping half — kills, subtree removals, the manifest — must be
        // inside the lock because it edits rows other threads read.
        let held = Instant::now();
        // Everything that was being built is on disk and in the list before this
        // returns: the engine announces a revision on the strength of it.
        self.settle()?;
        #[cfg(feature = "memory-trace")]
        let trace_settled = CommitStamp::now();
        let mut pending = self.flush_prepare(&mut self.inner.write(), false)?;
        #[cfg(feature = "memory-trace")]
        let trace_prepared = CommitStamp::now();
        // How long a search could have been waiting, printed rather than guessed
        // at.
        if std::env::var_os("SCOUR_LOCK_TRACE").is_some() && held.elapsed().as_millis() > 20 {
            eprintln!(
                "scourd: commit held the index for {:.0?} ({} staged)",
                held.elapsed(),
                pending.staged.as_ref().map_or(0, |(_, _, v)| v.len())
            );
        }
        // Both of the expensive halves, now that the lock is gone. **Every
        // failure from here hands the rows back**: they are out of the staging
        // buffer and this is the only copy.
        if let Err(e) = pending.write_alive(&self.dir, &self.disk_bytes) {
            Self::restore(&mut self.inner.write(), pending);
            return Err(e);
        }
        #[cfg(feature = "memory-trace")]
        let trace_alive = CommitStamp::now();
        let Some((number, generation, staged)) = pending.staged.take() else {
            #[cfg(feature = "memory-trace")]
            trace_commit(
                0,
                trace_start,
                trace_settled,
                trace_prepared,
                trace_alive,
                trace_alive,
                trace_alive,
                trace_alive,
            );
            return Ok(());
        };
        #[cfg(feature = "memory-trace")]
        let trace_rows = staged.len();
        let bytes = build(&staged);
        #[cfg(feature = "memory-trace")]
        let trace_built = CommitStamp::now();
        let live = match self
            .disk_bytes
            .changing(|| Live::write(&self.dir, number, generation, &bytes))
        {
            Ok(live) => live,
            Err(e) => {
                pending.staged = Some((number, generation, staged));
                Self::restore(&mut self.inner.write(), pending);
                return Err(e);
            }
        };
        #[cfg(feature = "memory-trace")]
        let trace_written = CommitStamp::now();
        drop(staged);
        drop(bytes);

        let mut inner = self.inner.write();
        inner.next_segment = inner.next_segment.max(number + 1);
        inner.segments.push(live);
        inner.segments.sort_by_key(|s| s.number);
        inner.meta_dirty = true;
        let saved = self.save_meta(&mut inner);
        #[cfg(feature = "memory-trace")]
        trace_commit(
            trace_rows,
            trace_start,
            trace_settled,
            trace_prepared,
            trace_alive,
            trace_built,
            trace_written,
            CommitStamp::now(),
        );
        saved
    }

    fn search(&self, req: &SearchRequest) -> Result<SearchResponse> {
        let started = Instant::now();
        let inner = self.inner.read();
        let offset = req.page.offset as usize;
        let limit = req.page.limit as usize;
        let need = offset + limit;
        let cap = req.page.count_cap.max(1) as usize;

        // **Candidates, not rows.** Building `offset + limit` rows a segment
        // reconstructed 401,438 front-coded paths to return sixty at offset
        // 100,000, and took 1.43 s.
        let mut pool: Vec<Candidate> = Vec::new();
        // Opened before the walk so the comparator can hold them while the pool
        // is still filling, which is what lets the pool be trimmed as it grows.
        let views: Vec<Segment<'_>> = inner
            .segments
            .iter()
            .map(|live| live.view())
            .collect::<Result<Vec<_>>>()?;

        // **A page reached instead of walked to**, for the one shape that allows
        // it: the stored order, nothing filtering, nothing concealed, deep enough
        // to be worth it. Everything else falls through to the walk unchanged.
        let stored_order = matches!(
            req.sort,
            scour_core::SortKey::Modified | scour_core::SortKey::Relevance
        ) && req.descending;
        if offset >= REACH_FROM
            && !walk_only()
            && stored_order
            && req.query.groups.is_empty()
            && inner.hidden_prefixes.is_empty()
            && let Some(found) = reach(&inner.segments, &views, offset, limit, cap, started)
        {
            return Ok(found);
        }

        let mut counted = 0u64;
        let mut budget = cap;
        let mut visited = 0u64;
        // Paths reconstructed, page and discarded prefix alike — see
        // `SearchResponse::rows_built`.
        let mut rows = 0u64;
        // The order the merge imposes, and it is `sort_hits`'s, moved to where the
        // rows have not been built yet: text keys resolve out of the arenas where
        // their sixteen bytes agree, and the last tie is the path, compared *as if
        // joined* with a directory decoded once for every row in it.
        let exact = crate::search::key_is_exact(req.sort);
        let desc = req.descending;
        let mut dirs: HashMap<(u32, u32), String> = HashMap::new();
        let mut cmp = |a: &Candidate, b: &Candidate| {
            let mut o = a.key.cmp(&b.key);
            // `exact` says the key is an abbreviation; the sort says what it
            // abbreviates. A second inexact key added without a case here orders
            // its whole tie group by file name.
            if o.is_eq() && !exact {
                o = match req.sort {
                    scour_core::SortKey::Name => {
                        let folded = |c: &Candidate| {
                            views
                                .get(c.seg as usize)
                                .and_then(|s| s.folded.get(c.row as usize))
                                .unwrap_or_default()
                        };
                        folded(a).cmp(folded(b))
                    }
                    scour_core::SortKey::Ext => {
                        match (views.get(a.seg as usize), views.get(b.seg as usize)) {
                            (Some(a_seg), Some(b_seg)) => crate::search::compare_extensions(
                                a_seg,
                                a.row as usize,
                                b_seg,
                                b.row as usize,
                            ),
                            _ => std::cmp::Ordering::Equal,
                        }
                    }
                    _ => std::cmp::Ordering::Equal,
                };
            }
            let o = if desc { o.reverse() } else { o };
            o.then_with(|| b.mtime.cmp(&a.mtime)).then_with(|| {
                // **Inside one segment the row number is the path order.** Rows
                // are stored newest-first with the path breaking that, and the
                // dates have just tied.
                if a.seg == b.seg {
                    return a.row.cmp(&b.row);
                }
                for c in [a, b] {
                    let Some(seg) = views.get(c.seg as usize) else {
                        continue;
                    };
                    let at = (c.seg, seg.dir_id(c.row as usize));
                    dirs.entry(at)
                        .or_insert_with(|| seg.dirs.get(at.1).unwrap_or_default());
                }
                let joined = |c: &Candidate| {
                    let seg = views.get(c.seg as usize);
                    let dir = seg
                        .map(|s| dirs[&(c.seg, s.dir_id(c.row as usize))].as_str())
                        .unwrap_or_default();
                    let name = seg
                        .and_then(|s| s.names.get(c.row as usize))
                        .unwrap_or_default();
                    joined_path(dir, name)
                };
                joined(a).cmp(joined(b))
            })
        };

        // Held for the whole loop, and only when the order depends on it.
        // A read guard, because nothing here builds: see the note at the call.
        let folders = (req.sort == scour_core::SortKey::Size).then(|| self.sizes.read());
        for (which, live) in inner.segments.iter().enumerate() {
            rows += live.rows() as u64;
            let seg = &views[which];
            let plan = Plan::compile(&req.query, seg)?;
            let doomed = Doomed::new(seg, &inner.hidden_prefixes);
            let conceals = !doomed.is_empty();
            let mut veto = |seg: &Segment<'_>, row: usize, _name: &[u8]| {
                doomed.takes(seg, row, seg.dir_id(row))
            };
            // **A query with no conditions matches every live row**, and the
            // segment already keeps that number. Walking for the total took
            // **1.233 s** across 2,094,185 rows against 5.5 ms for the page.
            let matches_all = !conceals && plan.is_empty();
            if matches_all && need == 0 {
                // A count and nothing else. There is no page to build, so
                // there is nothing left to walk for.
                let live_rows = live.live_rows();
                counted += live_rows;
                budget = budget.saturating_sub(live_rows as usize);
                continue;
            }
            let found = run_with(
                seg,
                &plan,
                Wanted {
                    sort: req.sort,
                    descending: req.descending,
                    // Nothing is skipped per segment: the row a global offset
                    // skips may live in any of them.
                    offset: 0,
                    limit: need,
                    // What is left of the *global* count, not a fresh copy: the
                    // whole cap per segment measured thirteen times the cost of
                    // one segment on a fragmented index.
                    count_cap: if matches_all { need } else { budget },
                    rank_only: true,
                },
                conceals.then_some(&mut veto as &mut dyn FnMut(&Segment<'_>, usize, &[u8]) -> bool),
                // Only `sort:size` reads this and it never *builds* it: a cold
                // cache means a folder sorts by its own column, and building here
                // would put ninety milliseconds inside a keystroke.
                folders
                    .as_ref()
                    .and_then(|c| c.rows_of(live.number))
                    .unwrap_or(&[]),
            );
            let total = if matches_all {
                live.live_rows()
            } else {
                found.total
            };
            counted += total;
            budget = budget.saturating_sub(total as usize);
            visited += found.rows_visited;
            pool.extend(found.ranked.into_iter().map(|r| Candidate {
                key: r.key,
                mtime: r.mtime,
                seg: which as u32,
                row: r.row,
            }));
        }

        // Selection, not a sort. Two partitions put the window where it belongs
        // without ordering the hundred thousand rows in front of it.
        let end = need.min(pool.len());
        if pool.len() > end && end > 0 {
            pool.select_nth_unstable_by(end - 1, &mut cmp);
            pool.truncate(end);
        }
        let start = offset.min(pool.len());
        if start > 0 && start < pool.len() {
            pool.select_nth_unstable_by(start - 1, &mut cmp);
        }
        let window = &mut pool[start..];
        window.sort_unstable_by(&mut cmp);

        let built = window.len() as u64;
        let hits: Vec<Hit> = window
            .iter()
            .take(limit)
            .filter_map(|c| {
                let seg = views.get(c.seg as usize)?;
                let row = c.row as usize;
                seg.names.get(row).map(|n| seg.hit(row, n))
            })
            .collect();
        Ok(SearchResponse {
            hits,
            total: counted.min(cap as u64),
            capped: counted >= cap as u64,
            took_us: started.elapsed().as_micros() as u64,
            // Did the walk get away with looking at less than everything? Not
            // "did every segment stop": a segment holding fewer matches than a
            // page runs to its own end and reports no early exit.
            fast_path: rows > 0 && visited < rows,
            rows_visited: visited,
            rows_built: built,
            // An index is handed a parsed query and never sees the text. The
            // engine stamps this on the way out.
            misread: Vec::new(),
        })
    }

    /// Every matching row, built one at a time and handed over. **The read lock is
    /// held throughout**, so a scan delays the watcher's next commit; releasing it
    /// between segments would let a commit renumber rows underneath the walk. Rows
    /// arrive in the index's own order, not a sorted one.
    fn scan(&self, req: &ScanRequest, f: &mut dyn FnMut(&Hit) -> bool) -> Result<u64> {
        let inner = self.inner.read();
        let mut rows = 0u64;
        self.for_each_match(&inner, &req.query, |seg, row| {
            let Some(name) = seg.names.get(row) else {
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
        let started = Instant::now();
        let inner = self.inner.read();
        let mut counts: Vec<HashMap<String, u64>> = vec![HashMap::new(); req.by.len()];
        let mut seen = 0usize;
        // `now` once, not per row: a walk of two hundred thousand rows that
        // asks the clock each time is asking it two hundred thousand times.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        // The most demanding question decides, because they share the walk: a
        // distribution cannot be sampled, so it makes a top-ten exact too.
        let cap = if req.by.iter().any(|b| matches!(b, FacetBy::Age { .. })) {
            AGE_SCAN_CAP
        } else {
            FACET_SCAN_CAP
        };
        let parents: Vec<String> = req
            .by
            .iter()
            .map(|b| match b {
                FacetBy::Dir { path, .. } => path.trim_end_matches('/').to_owned(),
                _ => String::new(),
            })
            .collect();
        // Read once per row however many questions want it, and not at all when
        // none does — the common case, since kind and age are both columns.
        let wants_name = req
            .by
            .iter()
            .any(|b| matches!(b, FacetBy::Ext { .. } | FacetBy::Dir { .. }));

        self.for_each_match(&inner, &req.query, |seg, row| {
            let name = wants_name.then(|| seg.names.get(row).unwrap_or_default());
            for (i, by) in req.by.iter().enumerate() {
                match by {
                    FacetBy::Kind => {
                        let k =
                            Kind::from_u8(seg.num_of(Field::Kind, row) as u8).unwrap_or(Kind::File);
                        // The token, not the label: a rail turns a facet into a
                        // `kind:` term, and a label can be translated.
                        *counts[i].entry(k.token().to_owned()).or_default() += 1;
                    }
                    FacetBy::Ext { .. } => {
                        let ext = scour_core::ext_of(name.unwrap_or_default());
                        if !ext.is_empty() {
                            *counts[i].entry(ext).or_default() += 1;
                        }
                    }
                    FacetBy::Dir { .. } => {
                        let path = seg.path(row, name.unwrap_or_default());
                        if let Some(rest) = path
                            .strip_prefix(parents[i].as_str())
                            .and_then(|r| r.strip_prefix('/'))
                        {
                            let child = rest.split('/').next().unwrap_or(rest);
                            *counts[i].entry(child.to_owned()).or_default() += 1;
                        }
                    }
                    FacetBy::Age { edges } => {
                        let days = (now - seg.num_of(Field::Mtime, row)).max(0) / 86_400;
                        // Ascending, so the first band it fits is its own.
                        // Linear because there are a couple of dozen.
                        let key = edges
                            .iter()
                            .find(|&&e| days <= e as i64)
                            .map(|e| e.to_string())
                            .unwrap_or_else(|| "older".to_owned());
                        *counts[i].entry(key).or_default() += 1;
                    }
                }
            }
            seen += 1;
            seen < cap
        })?;

        let groups: Vec<scour_core::FacetGroup> = req
            .by
            .iter()
            .zip(counts)
            .map(|(by, map)| {
                let top = match by {
                    FacetBy::Kind => 16,
                    FacetBy::Ext { top } | FacetBy::Dir { top, .. } => (*top).max(1) as usize,
                    // Every band, or the chart has holes in it.
                    FacetBy::Age { edges } => edges.len() + 1,
                };
                let mut facets: Vec<Facet> = map
                    .into_iter()
                    .map(|(key, count)| Facet { key, count })
                    .collect();
                facets.sort_unstable_by(|a, b| b.count.cmp(&a.count).then(a.key.cmp(&b.key)));
                facets.truncate(top);
                scour_core::FacetGroup {
                    by: by.clone(),
                    facets,
                }
            })
            .collect();

        Ok(FacetResponse {
            // The first group, flat, so a caller that asked one question does
            // not have to unwrap a list to read its answer.
            facets: groups.first().map(|g| g.facets.clone()).unwrap_or_default(),
            by: groups.first().map(|g| g.by.clone()).unwrap_or_default(),
            total: seen as u64,
            groups,
            capped: seen >= cap,
            took_us: started.elapsed().as_micros() as u64,
            // The index is handed a parsed query and never sees the text.
            // Stamped by the engine on the way out.
            misread: Vec::new(),
        })
    }

    fn usage(&self, req: &scour_core::UsageRequest) -> Result<scour_core::UsageResponse> {
        let started = Instant::now();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let inner = self.inner.read();
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
        })
    }

    fn maintain(&self, level: Maintenance) -> Result<MaintReport> {
        let started = Instant::now();
        // A fold rewrites segments and a rebuild reads every row of every one
        // of them. Both have to see the ones still being written.
        self.settle()?;
        let before = dir_size(&self.dir);
        match level {
            Maintenance::Flush => self.flush(&mut self.inner.write())?,
            Maintenance::Idle => {
                // The segments are mapped, so what they cost is page cache the
                // kernel reclaims. All this can return is the staging buffer.
                let mut inner = self.inner.write();
                self.flush(&mut inner)?;
                inner.staged.shrink_to_fit();
                inner.staged_at.shrink_to_fit();
            }
            // Fold the head; leave the body alone. A search pays for the *number*
            // of segments: at 1,083,334 entries one answers `"rapor"` in 1.11 ms
            // and eleven in 5.20. The largest is spared unless a quarter is dead.
            Maintenance::Compact => {
                self.flush(&mut self.inner.write())?;
                // The lock is taken to *choose* and released to *build*. Each
                // round re-reads the list, and a group of eleven becomes two,
                // which no longer qualifies — so this terminates.
                while let Some(head) = self.next_head() {
                    // A refusal ends the round rather than repeating it. See
                    // `fold`.
                    if !self.fold(&head)? {
                        break;
                    }
                }
            }
            // Everything becomes one segment, generations included: outside a scan
            // there is nothing left for a stamp to decide. **The work is decided
            // once, before the first fold** — a loop that re-read the list ran for
            // over ten minutes on 2.1 M entries, a commit landing during each.
            Maintenance::Rebuild => {
                self.flush(&mut self.inner.write())?;
                let work: Vec<Vec<u64>> = {
                    let inner = self.inner.read();
                    if inner.open.is_none() && inner.segments.len() > 1 {
                        vec![inner.segments.iter().map(|s| s.number).collect()]
                    } else {
                        Self::groups(&inner)
                            .into_iter()
                            .filter(|g| {
                                g.len() > 1
                                    || inner
                                        .segments
                                        .iter()
                                        .any(|s| s.number == g[0] && s.dead_rows() > 0)
                            })
                            .collect()
                    }
                };
                for group in work {
                    if !self.fold(&group)? {
                        break;
                    }
                }
            }
        }
        Ok(MaintReport {
            level,
            bytes_before: before,
            bytes_after: dir_size(&self.dir),
            took_ms: started.elapsed().as_millis() as u64,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(source: u32, path: &str, size: i64) -> Entry {
        Entry {
            id: scour_core::EntryId::path_hash(SourceId(source), path),
            path: path.to_owned(),
            is_dir: false,
            meta: scour_core::Meta {
                mtime: size,
                size,
                ..scour_core::Meta::UNKNOWN
            },
        }
    }

    fn commit_rows(index: &NativeIndex, rows: impl IntoIterator<Item = Entry>) {
        index
            .apply(&mut rows.into_iter().map(Change::Upsert))
            .expect("apply rows");
        index.commit().expect("commit rows");
    }

    fn hits(index: &NativeIndex) -> Vec<Hit> {
        index
            .search(&SearchRequest {
                page: scour_core::Page::new(0, 100),
                ..SearchRequest::default()
            })
            .expect("search")
            .hits
    }

    fn block_existing_file(path: &Path) -> PathBuf {
        let saved = path.with_extension("test-saved");
        std::fs::rename(path, &saved).expect("move file aside");
        std::fs::create_dir(path).expect("put directory in the file's place");
        saved
    }

    fn restore_blocked_file(path: &Path, saved: &Path) {
        std::fs::remove_dir(path).expect("remove blocker");
        std::fs::rename(saved, path).expect("restore file");
    }

    fn two_segment_index(dir: &Path) -> std::sync::Arc<NativeIndex> {
        let index = std::sync::Arc::new(NativeIndex::open_or_create(dir).expect("create"));
        commit_rows(&index, [row(0, "/w/victim.txt", 1)]);
        commit_rows(&index, [row(0, "/w/keeper.txt", 2)]);
        assert_eq!(index.inner.read().segments.len(), 2);
        index
    }

    #[test]
    fn closing_a_scan_starts_a_new_compaction_cohort() {
        let mut inner = Inner {
            generation: 7,
            open: Some(7),
            ..Inner::default()
        };
        close_generation(&mut inner, 7);
        assert_eq!(inner.open, None);
        assert_eq!(inner.generation, 8);
        assert!(inner.meta_dirty);

        // A source with several vouched roots sweeps once per root. Only the
        // first closes the scan; the remaining roots must not keep advancing.
        close_generation(&mut inner, 7);
        assert_eq!(inner.generation, 8);
    }

    fn member(number: u64, rows: u64) -> Member {
        Member {
            number,
            rows,
            dead: 0,
        }
    }

    /// The cut, at the sizes the live index actually holds: a 4.6 M-row body, a
    /// 454,184-row tail, and watcher commits of a few rows.
    #[test]
    fn size_tiers_cut_where_a_member_outweighs_everything_below_it() {
        // Nothing below the tail comes to a quarter of it, so it stays where it
        // is and the trickle folds on its own.
        assert_eq!(
            size_tiers([(1, 4_600_000), (2, 454_184), (3, 40), (4, 40), (5, 40)]),
            vec![vec![3, 4, 5], vec![2], vec![1]]
        );
        // Once the trickle has grown to a quarter of the tail, the tail takes it
        // in — worth ~113k rows of churn rather than sixty seconds.
        assert_eq!(
            size_tiers([(1, 4_600_000), (2, 454_184), (3, 113_546)]),
            vec![vec![3, 2], vec![1]]
        );
        // Equal sizes are one tier: a fixture of ten 800-row segments has no
        // body to spare, and folding them together is the whole job.
        assert_eq!(
            size_tiers((1..=4).map(|n| (n, 800))),
            vec![vec![1, 2, 3, 4]]
        );
        // Order in, and ties, must not decide the answer.
        assert_eq!(
            size_tiers([(9, 40), (1, 4_600_000), (5, 40)]),
            size_tiers([(1, 4_600_000), (5, 40), (9, 40)])
        );
        assert_eq!(size_tiers([]), Vec::<Vec<u64>>::new());
    }

    /// The head is the trickle, and the tail is not in it.
    #[test]
    fn a_trickle_is_folded_without_the_tail() {
        let live = [
            member(1, 4_600_000),
            member(2, 454_184),
            member(3, 40),
            member(4, 40),
            member(5, 40),
        ];
        assert_eq!(head_of(&live), Some(vec![3, 4, 5]));

        // With the trickle already folded there is nothing left worth a
        // rewrite: three segments, and none of them touched.
        assert_eq!(
            head_of(&[member(1, 4_600_000), member(2, 454_184), member(6, 120)]),
            None
        );

        // The old rule is kept where it earns its keep: a largest segment a
        // quarter of which is dead is rewritten, and everything rides along.
        let dead = [
            Member {
                number: 1,
                rows: 4_600_000,
                dead: 2_000_000,
            },
            member(2, 454_184),
            member(3, 40),
        ];
        assert_eq!(head_of(&dead), Some(vec![1, 2, 3]));

        // And a middle segment alone in its tier can still be shrunk, or
        // deleting a large directory would leave its rows on disk until the
        // next rebuild. `fold` accepts a group of one on exactly this test.
        let hollow = [
            member(1, 4_600_000),
            Member {
                number: 2,
                rows: 454_184,
                dead: 300_000,
            },
            member(3, 40),
        ];
        assert_eq!(head_of(&hollow), Some(vec![2]));
    }

    /// The segment count stays bounded under a trickle that never stops — the rule
    /// this replaces left 241 segments across 205 generations with nothing
    /// foldable. Driven through `head_of` because what matters is the count after
    /// thousands of rounds at live sizes.
    #[test]
    fn a_long_trickle_stays_under_the_geometric_bound() {
        // Five hundred rounds of sixty forty-row commits on a 4.6 M-row body,
        // through both rules: this file's, and everything but the largest.
        let trickle = |pick: &dyn Fn(&[Member]) -> Option<Vec<u64>>| {
            let mut segments = vec![member(0, 4_600_000)];
            let mut next = 1u64;
            let (mut worst, mut folds, mut rewritten) = (0usize, 0usize, 0u64);
            for _ in 0..500 {
                for _ in 0..60 {
                    segments.push(member(next, 40));
                    next += 1;
                }
                // `maintain(Compact)`: fold what `next_head` hands back until it
                // hands back nothing. `groups` offers three or more, or nothing.
                while segments.len() >= 3 {
                    let Some(head) = pick(&segments) else { break };
                    if head.len() < 2 {
                        break;
                    }
                    let rows: u64 = segments
                        .iter()
                        .filter(|m| head.contains(&m.number))
                        .map(|m| m.rows)
                        .sum();
                    segments.retain(|m| !head.contains(&m.number));
                    segments.push(member(next, rows));
                    next += 1;
                    folds += 1;
                    rewritten += rows;
                }
                worst = worst.max(segments.len());
            }
            let total: u64 = segments.iter().map(|m| m.rows).sum();
            (worst, folds, rewritten, total)
        };

        let (worst, folds, rewritten, total) = trickle(&|m| head_of(m));
        // `ceil(log4(total)) + 1`: each tier below the largest holds one segment
        // more than four times the one under it, from a single row up.
        let bound = (64 - total.leading_zeros()).div_ceil(2) as usize + 1;
        assert_eq!(bound, 13, "1.2 M rows of trickle on a 4.6 M-row body");
        // Measured at five when this was written. The assertion is the bound,
        // because that is the promise; the five is how much room it has.
        assert!(
            worst <= bound,
            "{worst} segments at worst, against a bound of {bound}"
        );

        let (_, was_folds, was_rewritten, _) = trickle(&|members: &[Member]| {
            let biggest = members.iter().max_by_key(|m| m.rows)?;
            Some(
                members
                    .iter()
                    .map(|m| m.number)
                    .filter(|&n| n != biggest.number)
                    .collect(),
            )
        });
        // And this is the whole of it: the same trickle, the same number of
        // compactions, and the tail dragged through every one of them.
        assert_eq!(folds, was_folds, "the same rounds either way");
        assert!(
            rewritten * 20 < was_rewritten,
            "{rewritten} rows rewritten over {folds} folds against the old rule's \
             {was_rewritten}, on an index holding {total}"
        );
    }

    /// Folding by tiers answers exactly what folding the whole rest answered.
    /// Two indexes, the same rows and removals, compacted both ways: how the rows
    /// are divided into segments is not something a query can see.
    #[test]
    fn a_tiered_fold_answers_what_folding_the_whole_rest_did() {
        let tiered = tempfile::tempdir().expect("tmpdir");
        let whole = tempfile::tempdir().expect("tmpdir");
        let a = NativeIndex::open_or_create(tiered.path()).expect("create");
        let b = NativeIndex::open_or_create(whole.path()).expect("create");
        for index in [&a, &b] {
            // A body, a tail, and a trickle: the live shape, scaled down.
            commit_rows(
                index,
                (0..2_000).map(|i| row(0, &format!("/body/rapor-{i}.txt"), i % 97)),
            );
            commit_rows(
                index,
                (0..500).map(|i| row(0, &format!("/tail/rapor-{i}.md"), i % 89)),
            );
            for round in 0..9 {
                commit_rows(
                    index,
                    (0..3).map(|i| row(0, &format!("/w/{round}-{i}.txt"), round * 7 + i)),
                );
            }
            // Deaths, so the live bitmaps and the row counts disagree.
            index
                .apply(&mut (0..40).map(|i| Change::RemoveSubtree {
                    path: format!("/body/rapor-{}.txt", i * 7),
                }))
                .expect("apply removals");
            index.commit().expect("commit removals");
        }

        let fingerprint = |index: &NativeIndex| -> Vec<String> {
            let mut out = Vec::new();
            for sort in [
                scour_core::SortKey::Modified,
                scour_core::SortKey::Name,
                scour_core::SortKey::Path,
                scour_core::SortKey::Ext,
                scour_core::SortKey::Size,
            ] {
                for descending in [true, false] {
                    let answer = index
                        .search(&SearchRequest {
                            query: scour_query::parse("rapor"),
                            sort,
                            descending,
                            page: scour_core::Page::new(0, 4_000),
                        })
                        .expect("search");
                    out.push(format!("{sort:?} {descending} {}", answer.total));
                    out.extend(
                        answer
                            .hits
                            .iter()
                            .map(|h| format!("{} {} {}", h.path, h.meta.size, h.meta.mtime)),
                    );
                }
            }
            out
        };

        let before = fingerprint(&a);
        assert_eq!(fingerprint(&b), before, "the two fixtures start equal");

        a.maintain(Maintenance::Compact).expect("compact");
        // The rule this replaces, by hand: one group, everything but the
        // largest member, until folding stops changing anything.
        loop {
            let head = {
                let inner = b.inner.read();
                if inner.segments.len() < 3 {
                    break;
                }
                let biggest = inner
                    .segments
                    .iter()
                    .max_by_key(|s| s.rows())
                    .expect("a biggest")
                    .number;
                inner
                    .segments
                    .iter()
                    .map(|s| s.number)
                    .filter(|&n| n != biggest)
                    .collect::<Vec<u64>>()
            };
            if !b.fold(&head).expect("fold") {
                break;
            }
        }

        assert_eq!(fingerprint(&a), before, "tiered folding changed an answer");
        assert_eq!(fingerprint(&b), before, "the old rule changed an answer");
        assert_eq!(
            a.stats().expect("stats").entries,
            b.stats().expect("stats").entries
        );
        // Which is the whole trade: the same answers out of one more segment,
        // for a fold that did not touch the body or the tail.
        assert!(
            a.inner.read().segments.len() <= 3,
            "{} segments after a tiered compaction",
            a.inner.read().segments.len()
        );
    }

    /// A trickle does not drag the tail through a fold with it. The tail's
    /// 11.70 MB of names reappeared under a new segment number nine times in eight
    /// and a half minutes, 1.5–1.8 s of worker CPU and 51–68 MB each.
    #[test]
    fn a_trickle_of_small_segments_does_not_refold_the_tail() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let index = NativeIndex::open_or_create(tmp.path()).expect("create");
        commit_rows(
            &index,
            (0..4_000).map(|i| row(0, &format!("/body/{i}.txt"), i)),
        );
        commit_rows(
            &index,
            (0..1_000).map(|i| row(0, &format!("/tail/{i}.txt"), i)),
        );
        let numbers = || -> Vec<u64> {
            index
                .inner
                .read()
                .segments
                .iter()
                .map(|s| s.number)
                .collect()
        };
        let started = numbers();
        assert_eq!(started.len(), 2, "the fixture is a body and a tail");
        let (body, tail) = (started[0], started[1]);

        for round in 0..4 {
            for i in 0..6 {
                commit_rows(&index, [row(0, &format!("/w/{round}-{i}.txt"), i)]);
            }
            index.maintain(Maintenance::Compact).expect("compact");
            let after = numbers();
            assert!(after.contains(&body), "round {round} rewrote the body");
            assert!(
                after.contains(&tail),
                "round {round} rewrote the tail: this is the fold that cost 2.5-3% of a core"
            );
            assert_eq!(
                after.len(),
                3,
                "body, tail, one folded trickle — and nothing accumulating: {after:?}"
            );
        }
        assert_eq!(index.stats().expect("stats").entries, 4_000 + 1_000 + 24);
    }

    #[test]
    fn a_landed_segment_retries_late_removals_after_an_alive_error() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        {
            let index = NativeIndex::open_or_create(tmp.path()).expect("create");
            let number = 1;
            let rows = [row(0, "/w/victim.txt", 1), row(0, "/w/keeper.txt", 2)];
            let bytes = build(&rows);
            let live = index
                .disk_bytes
                .changing(|| Live::write(tmp.path(), number, 0, &bytes))
                .expect("write landed segment");
            let alive = tmp.path().join("seg-00000001.alive");
            let saved = block_existing_file(&alive);
            {
                let mut building = index.building.0.lock();
                building.flights.push(Flight {
                    number,
                    gone: scour_core::PrefixSet::new(vec!["/w/victim.txt".into()]),
                    ..Flight::default()
                });
                building.landed.push(Landed::Built { number, live });
            }

            assert!(index.collect(&mut index.inner.write()).is_err());
            {
                let building = index.building.0.lock();
                assert_eq!(building.flights.len(), 1);
                assert_eq!(building.landed.len(), 1);
            }

            restore_blocked_file(&alive, &saved);
            index
                .collect(&mut index.inner.write())
                .expect("retry collection");
        }

        let reopened = NativeIndex::open_or_create(tmp.path()).expect("reopen");
        let paths: Vec<_> = hits(&reopened).into_iter().map(|hit| hit.path).collect();
        assert_eq!(paths, ["/w/keeper.txt"]);
    }

    #[test]
    fn a_landed_segment_retries_manifest_publication_without_another_landing() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        {
            let index = NativeIndex::open_or_create(tmp.path()).expect("create");
            let number = 1;
            let bytes = build(&[row(0, "/w/landed.txt", 1)]);
            let live = index
                .disk_bytes
                .changing(|| Live::write(tmp.path(), number, 0, &bytes))
                .expect("write landed segment");
            {
                let mut building = index.building.0.lock();
                building.flights.push(Flight {
                    number,
                    ..Flight::default()
                });
                building.landed.push(Landed::Built { number, live });
            }
            let manifest = tmp.path().join(META_FILE);
            std::fs::create_dir(&manifest).expect("block first manifest");

            assert!(index.collect(&mut index.inner.write()).is_err());
            assert!(index.inner.read().meta_dirty);
            assert!(index.building.0.lock().landed.is_empty());

            std::fs::remove_dir(&manifest).expect("remove blocker");
            index
                .collect(&mut index.inner.write())
                .expect("retry empty collection");
        }

        let reopened = NativeIndex::open_or_create(tmp.path()).expect("reopen");
        assert_eq!(hits(&reopened)[0].path, "/w/landed.txt");
    }

    #[test]
    fn an_empty_commit_retries_a_failed_manifest_publication() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        {
            let index = NativeIndex::open_or_create(tmp.path()).expect("create");
            commit_rows(&index, [row(0, "/w/first.txt", 1)]);

            // The exact state at the final publication point of a synchronous
            // commit: the segment is mapped and visible, the manifest is not.
            let number = index.inner.read().next_segment;
            let bytes = build(&[row(0, "/w/second.txt", 2)]);
            let live = index
                .disk_bytes
                .changing(|| Live::write(tmp.path(), number, 0, &bytes))
                .expect("write second segment");
            let manifest = tmp.path().join(META_FILE);
            let saved = block_existing_file(&manifest);
            {
                let mut inner = index.inner.write();
                inner.next_segment = number + 1;
                inner.segments.push(live);
                inner.meta_dirty = true;
                assert!(index.save_meta(&mut inner).is_err());
            }

            restore_blocked_file(&manifest, &saved);
            index.commit().expect("empty retry commit");
        }

        let reopened = NativeIndex::open_or_create(tmp.path()).expect("reopen");
        let mut paths: Vec<_> = hits(&reopened).into_iter().map(|hit| hit.path).collect();
        paths.sort();
        assert_eq!(paths, ["/w/first.txt", "/w/second.txt"]);
    }

    #[test]
    fn fold_does_not_publish_a_row_removed_after_its_snapshot() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let index = two_segment_index(tmp.path());
        let gate = std::sync::Arc::new(FoldGate::new());
        index.gate_next_fold(std::sync::Arc::clone(&gate));
        let worker = std::sync::Arc::clone(&index);
        let folding = std::thread::spawn(move || worker.maintain(Maintenance::Rebuild));
        gate.snapshot_ready.wait();

        let changed = index
            .apply(&mut std::iter::once(Change::RemoveSubtree {
                path: "/w/victim.txt".into(),
            }))
            .and_then(|_| index.commit());
        gate.resume.wait();
        changed.expect("concurrent removal");
        folding.join().expect("fold thread").expect("rebuild");

        assert!(!hits(&index).iter().any(|hit| hit.path == "/w/victim.txt"));
        drop(index);
        let reopened = NativeIndex::open_or_create(tmp.path()).expect("reopen");
        assert!(
            !hits(&reopened)
                .iter()
                .any(|hit| hit.path == "/w/victim.txt")
        );
    }

    #[test]
    fn fold_does_not_publish_an_old_row_after_a_concurrent_upsert() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let index = two_segment_index(tmp.path());
        let gate = std::sync::Arc::new(FoldGate::new());
        index.gate_next_fold(std::sync::Arc::clone(&gate));
        let worker = std::sync::Arc::clone(&index);
        let folding = std::thread::spawn(move || worker.maintain(Maintenance::Rebuild));
        gate.snapshot_ready.wait();

        let changed = index
            .apply(&mut std::iter::once(Change::Upsert(row(
                0,
                "/w/victim.txt",
                99,
            ))))
            .and_then(|_| index.commit());
        gate.resume.wait();
        changed.expect("concurrent upsert");
        folding.join().expect("fold thread").expect("rebuild");

        let current: Vec<_> = hits(&index)
            .into_iter()
            .filter(|hit| hit.path == "/w/victim.txt")
            .collect();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].meta.size, 99);
        drop(index);
        let reopened = NativeIndex::open_or_create(tmp.path()).expect("reopen");
        let current: Vec<_> = hits(&reopened)
            .into_iter()
            .filter(|hit| hit.path == "/w/victim.txt")
            .collect();
        assert_eq!(current.len(), 1);
        assert_eq!(current[0].meta.size, 99);
    }

    #[test]
    fn an_empty_commit_retries_a_fold_manifest_failure() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let index = two_segment_index(tmp.path());
        let gate = std::sync::Arc::new(FoldGate::new());
        index.gate_next_fold(std::sync::Arc::clone(&gate));
        let worker = std::sync::Arc::clone(&index);
        let folding = std::thread::spawn(move || worker.maintain(Maintenance::Rebuild));
        gate.snapshot_ready.wait();

        let manifest = tmp.path().join(META_FILE);
        let saved = block_existing_file(&manifest);
        gate.resume.wait();
        assert!(
            folding.join().expect("fold thread").is_err(),
            "a directory accepted the replacement manifest"
        );

        restore_blocked_file(&manifest, &saved);
        index.commit().expect("empty retry commit");
        assert_eq!(index.stats().expect("stats").segments, 1);
        assert!(
            !tmp.path().join("seg-00000001.names").exists()
                && !tmp.path().join("seg-00000002.names").exists(),
            "the publication retry left the folded inputs on disk"
        );
        drop(index);

        let reopened = NativeIndex::open_or_create(tmp.path()).expect("reopen");
        assert_eq!(reopened.stats().expect("stats").segments, 1);
        assert_eq!(hits(&reopened).len(), 2);
    }
}
