//! The index, as the rest of Scour sees it.
//!
//! Everything under this file is a segment: written once, mapped, never
//! edited. This is what turns that into something that can be written to.
//!
//! ## Three states a change passes through
//!
//! 1. **Staged.** An upsert waits in memory. It is not searchable — the trait
//!    says a change is not durable until [`Index::commit`], and making it
//!    visible earlier would mean a second matching path that has to agree with
//!    the first one forever.
//! 2. **Hidden.** A removal is searchable *immediately*, because the opposite
//!    is what a user notices: deleting a folder and still seeing its contents
//!    for a second reads as a broken program. So a removal goes into an overlay
//!    that every search consults, before anything on disk changes.
//! 3. **Committed.** The staged entries become a new segment, the hidden ones
//!    become cleared bits in the segments that hold them, and the overlay
//!    empties.
//!
//! ## Back pressure, again
//!
//! The staging buffer is bounded and flushes itself. The engine this replaces
//! learned that the expensive way: a directory walk produces entries far faster
//! than anything consumes them, and the queue in between was the whole
//! filesystem — 1,580 MB of documents in flight, for an index that is 396 MB
//! when finished. Here the buffer is ours and it is counted.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Instant;

use parking_lot::RwLock;
use scour_core::{
    ApplyReport, Change, Entry, Error, Facet, FacetBy, FacetRequest, FacetResponse, Hit, Index,
    IndexStats, Kind, MaintReport, Maintenance, Result, SearchRequest, SearchResponse, SourceId,
};
use serde::{Deserialize, Serialize};

use crate::build::{build, build_sorted};
use crate::columns::Field;
use crate::durable::replace_synced;
use crate::ids::digest;
use crate::lock::DirLock;
use crate::search::{Plan, Segment, Wanted, run_with, sort_hits};
use crate::segment::Live;
use crate::usage::Rollup;

/// How many entries may wait in memory before a segment is written.
///
/// The number decides two things that pull in opposite directions. Too small
/// and a full scan leaves hundreds of segments, each of which a search has to
/// walk separately and each of which must find its own page before it can stop.
/// Too large and the buffer is the memory spike it exists to prevent.
///
/// A hundred thousand entries is roughly 25 MB of `Entry` values, and leaves
/// eleven segments after a million — which is a search at 5.15 ms instead of
/// 1.02 until something compacts, and compacting is 2.7 seconds. Raising it
/// buys fewer segments with memory at the ratio the whole design exists to
/// avoid paying, so the answer is to compact, not to buffer.
const MAX_STAGED: usize = 100_000;

/// How many rows a facet count will look at before it answers approximately.
///
/// A facet is a sidebar, not a result. Nobody waits for it.
const FACET_SCAN_CAP: usize = 200_000;

/// …except a distribution, which has to see everything or it is a wrong
/// picture rather than a rough one.
///
/// **Rows are stored newest first**, so a cap does not sample — it takes a
/// prefix, and a prefix of a date-ordered index is the recent end of it. A
/// histogram of ages built that way says "today" no matter what was asked,
/// which is how this was noticed. A top-ten list can be approximate because
/// the tenth item being wrong changes little; a shape cannot.
const AGE_SCAN_CAP: usize = usize::MAX;

const META_FILE: &str = "native-index.json";
/// Bumped when the files change shape **or when a stored value changes what it
/// means**. Version 2 added the trigram filter, version 3 the per-block minimum
/// and maximum, version 4 a distance byte a directory, and version 5 the file
/// kinds — where nothing changed shape at all and every row of an older index
/// would still decode, into the wrong answer. `kind:build` would find nothing
/// and half the sidebar would read `File`: nothing corrupt and everything
/// wrong, which is the case this constant exists for. Version 6 added the
/// folded name arena, without which a search has nothing to walk. Version 7
/// took the block from 128 rows to 32 — every offset in every file is relative
/// to it, so an older index decodes into noise rather than into an answer.
/// Version 8 made a row's identity its path: three identity columns went, and
/// the lookup table is keyed on the path rather than on whatever the source
/// called the entry, so an older table answers about nothing. Version 9 added
/// the link count, without which a disk-usage report counts a hard-linked file
/// once per name.
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
///
/// **It also holds the only copy of those rows**, which is what makes losing it
/// unacceptable: the entries have been lifted out of the staging buffer and
/// exist nowhere else until the segment is on disk. Every path that can fail
/// after this point hands it back to [`NativeIndex::restore`] instead of
/// dropping it — a write that could not happen has to leave the index where it
/// was, not quietly poorer.
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
    fn write_alive(&self, dir: &Path) -> Result<()> {
        for (number, bits) in &self.alive {
            Live::write_alive(dir, *number, bits)?;
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
struct Inner {
    segments: Vec<Live>,
    /// Upserts not yet written. Searchable only after a commit.
    staged: Vec<Entry>,
    /// Rows a walk found exactly as they already were, by segment number.
    ///
    /// **A sweep deletes what the walk did not stamp, and the stamp is one
    /// number a segment.** So a row that is skipped because nothing about it
    /// changed has no way to say it was seen — and a rescan of an untouched
    /// filesystem would empty the index while reporting success. This is that
    /// way: one bit a row, held only while a generation is open, and never
    /// written anywhere.
    ///
    /// 275 KB for two million rows, against the three seconds and twenty
    /// segments that writing them all again costs. It is keyed on the segment
    /// number, which is why folding is refused while a generation is open —
    /// renumbering would leave every bit pointing at the wrong row, and the
    /// failure would be the silent one.
    seen: HashMap<u64, Vec<u8>>,
    /// How many rows have been spared since the last time somebody was told.
    ///
    /// Sparing happens when the batch is flushed, which is not when the entry
    /// was handed over — so the count reaches [`ApplyReport`] one batch late,
    /// and the tail of a scan is reported by whatever calls `apply` next. It is
    /// a diagnostic, and this is the honest shape of it rather than an accurate
    /// number bought with a probe per entry.
    spared: u64,
    /// Every source this index has been handed a row for.
    ///
    /// **`RemoveSubtree` carries a path and no source**, and the identity
    /// table is keyed on both — so a removal cannot ask "which row is this
    /// path" without one. Guessing is not needed: there are as many of these
    /// as there are configured sources, two on the machine this was written
    /// for, and trying each is two lookups against a scan of every row.
    ///
    /// Learned rather than stored. An index reopened and not yet written to
    /// knows none, and a removal arriving before the first upsert falls back
    /// to the scan — which is correct, and does not happen in practice
    /// because a scan upserts before a watcher reports anything.
    sources: Vec<SourceId>,
    /// Where each staged path sits, so a second upsert of the same file
    /// replaces the first instead of adding a second row for it.
    staged_at: HashMap<u64, usize>,
    /// Paths whose removal has taken effect for searches but not yet for the
    /// files.
    ///
    /// One structure rather than two. There used to be a map of identities
    /// beside it for removing a single entry, and nothing ever filled it: a
    /// watcher reporting a deletion has a path and nothing else, so every real
    /// removal arrived here. A prefix set holding one path removes exactly
    /// that path, because nothing is under a file.
    hidden_prefixes: scour_core::PrefixSet,
    generation: u64,
    /// A generation that has been handed out and not yet reconciled.
    ///
    /// A scan takes a generation, writes its rows under it, and finishes by
    /// sweeping away whatever it did not stamp. Between those two moments the
    /// index holds rows that are *about to be* judged, and folding a segment
    /// across a generation boundary in that window would hide them from the
    /// judgement. Outside it, every row in the index is current by definition,
    /// which is what makes a full fold safe.
    ///
    /// Cleared by the sweep, and replaced when a newer generation begins —
    /// callers are expected to run one scan at a time, and the engine's single
    /// worker thread is what guarantees it.
    open: Option<u64>,
    next_segment: u64,
}

/// A segment being built on another thread.
///
/// Its rows are exactly as invisible as staged rows — the trait says a change
/// is not durable until `commit`, and this is the window in which that is
/// literally true. What it has to carry is the set of paths it holds: a
/// removal arriving while it is in the air has no row to kill yet, and the one
/// it was meant to kill is about to appear.
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

/// How many segments may be built at once.
///
/// Each one holds its rows and its output bytes — about 35 MB for a full
/// hundred-thousand-row segment — so this is a memory bound as much as a
/// concurrency one. Past it, whoever wanted the flush builds it themselves,
/// which is the back pressure this design needs and costs nothing to write.
const MAX_BUILDING: usize = 4;

#[derive(Debug)]
pub struct NativeIndex {
    dir: PathBuf,
    inner: RwLock<Inner>,
    /// Segments in the air, and something to wait on.
    ///
    /// Separate from `inner`, and deliberately: a builder thread never takes
    /// the index lock at all. It writes its files, drops the result here and
    /// stops — whoever next holds the write lock puts it in the list. Waiting
    /// for a build while holding the lock the build needs is the deadlock this
    /// avoids by construction, and it also means a slow disk cannot block a
    /// search.
    building: std::sync::Arc<(parking_lot::Mutex<Building>, parking_lot::Condvar)>,
    /// Builds that failed. Their rows were put back; this is how the next
    /// commit finds out it has something to report.
    build_failed: std::sync::atomic::AtomicUsize,
    /// Released when this is dropped, or by the kernel if the process dies.
    /// Held for the lifetime of the index because every writing path — commit,
    /// sweep, maintain — goes through this value.
    _lock: DirLock,
}

impl NativeIndex {
    /// Open the index in `dir`, creating an empty one if there is none.
    pub fn open_or_create(dir: &Path) -> Result<NativeIndex> {
        std::fs::create_dir_all(dir).map_err(|e| Error::io(&e, &dir.to_string_lossy()))?;
        // Before anything is read, and long before anything is written: a
        // second writer here does not merely lose an update, it calls
        // `File::create` on a file the first one has mmapped.
        let lock = DirLock::acquire(dir)?;
        // **Only a missing manifest means a new index.** Every other error —
        // a permission change, a bad block, a directory that is not readable
        // right now — used to land here as `Meta::default()`, which says "this
        // index holds nothing". `sweep_orphans` then reads that as a directory
        // full of segments nothing refers to and erases them. One unreadable
        // JSON file was enough to destroy an intact index; failing closed costs
        // a service that will not start until the cause is dealt with, which is
        // the cheaper of the two by a distance.
        let meta: Meta = match std::fs::read_to_string(dir.join(META_FILE)) {
            Ok(s) => serde_json::from_str(&s).map_err(|e| Error::IndexCorrupt {
                detail: format!("{META_FILE}: {e}"),
            })?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Meta::default(),
            Err(e) => return Err(Error::io(&e, &dir.join(META_FILE).to_string_lossy())),
        };
        if meta.format != FORMAT {
            // Not damaged — written by another version. Nothing here is worth
            // recovering and nothing here is lost: every row came from the
            // filesystem and can come from it again. Saying so is the caller's
            // decision, and `discard` is how they act on it.
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
        Ok(NativeIndex {
            dir: dir.to_owned(),
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
            _lock: lock,
        })
    }

    /// Throw away an index so the next `open_or_create` starts empty.
    ///
    /// For [`Error::IndexOutdated`] and nothing else: the caller has decided
    /// that a rebuild is cheaper than a migration, which it is whenever the
    /// index is derived from something still there to be read.
    ///
    /// Removes the manifest and the segment files and leaves everything else,
    /// including the lock, alone. Deliberately not `remove_dir_all`: the
    /// directory comes from configuration, and a wrong one there should cost a
    /// confusing error rather than somebody's files.
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

    fn save_meta(&self, inner: &Inner) -> Result<()> {
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
        // Written beside itself and renamed over, so a kill mid-write leaves
        // the old manifest rather than a truncated one. `std::fs::write`
        // truncates first, and a zero-length manifest is an index that reports
        // itself corrupt and has to be rebuilt from a walk of the disk.
        let p = self.dir.join(META_FILE);
        replace_synced(&p, json.as_bytes())
    }

    /// Turn everything staged and hidden into files.
    ///
    /// Order matters and is not arbitrary: removals are applied to the existing
    /// segments *before* the new one is written, because one of those removals
    /// is the old row of every entry being re-upserted, and applying them
    /// afterwards would find — and kill — the row that was just added.
    /// Flush with the lock held throughout. For callers already inside it.
    /// Flush, and be finished when it returns.
    ///
    /// For everything whose next line depends on the rows being *in* the
    /// index: a sweep about to judge them, a generation about to stamp them, a
    /// fold about to rewrite them. Only the staging buffer overflowing can
    /// afford to let a segment be built elsewhere, because nothing is waiting
    /// on it.
    fn flush(&self, inner: &mut Inner) -> Result<()> {
        self.flush_maybe_elsewhere(inner, false)
    }

    fn flush_maybe_elsewhere(&self, inner: &mut Inner, elsewhere: bool) -> Result<()> {
        // `elsewhere` twice over, and it is the same fact both times: this
        // flush happens because a buffer filled up, not because a caller needs
        // the rows in. That is what lets the segment be built on another
        // thread, and it is what lets the flush be called off entirely.
        let mut pending = self.flush_prepare(inner, elsewhere)?;
        if let Err(e) = pending.write_alive(&self.dir) {
            Self::restore(inner, pending);
            return Err(e);
        }
        let Some((number, generation, staged)) = pending.staged.take() else {
            return Ok(());
        };

        // **Somewhere else, if anybody else is free.**
        //
        // A hundred thousand rows take about 150 ms to turn into a segment,
        // and the scan profile put 85% of a full index in exactly that: one
        // thread interning directories, folding names and extracting trigrams
        // while nineteen others had nothing to do. Segments are independent —
        // separate files, separate numbers, merged at query time — so there is
        // no reason to build them one after another.
        //
        // Past `MAX_BUILDING` the caller builds it itself. That is the back
        // pressure: memory is bounded by the number in the air, and a walk
        // that outruns the builders is made to wait by doing the work.
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
            let written = Live::write(&self.dir, number, generation, &bytes);
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
            // The scan's builder buffers are the process's largest anonymous
            // allocation. They are gone here, not at the end of the scan: a
            // builder may run on this long-lived worker, and glibc otherwise
            // keeps its freed pages until something happens to trim this arena.
            trim_builder_allocator();
            inner.segments.push(live);
            inner.segments.sort_by_key(|s| s.number);
            // Past here the rows are on disk. A manifest that will not save is
            // a real failure and is reported, but the segment is named by the
            // next successful save and swept as an orphan if there never is
            // one — so the entries are not put back, which would duplicate
            // them.
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
                let written = Live::write(&dir, number, generation, &bytes);
                drop(bytes);
                let done = match written {
                    Ok(live) => {
                        drop(staged);
                        // These threads are deliberately short-lived. Trimming
                        // while the thread still owns its arena releases the
                        // pages its entries and output buffers just occupied;
                        // a later trim from `scour-worker` left them mapped.
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

    /// Kill every row of a segment that is under one of these prefixes.
    /// Remove paths that name a file, without looking at any row that is not
    /// one of them.
    ///
    /// **A watcher cannot tell a file from a folder**, so it reports every
    /// removal as a subtree and the overwhelming majority of them are one
    /// file. Answering those by scanning was linear in how much had been
    /// indexed — 10.5 ms at half a million rows and 64.9 ms at four million,
    /// with a realistic spread of timestamps — while the identity table
    /// answers "which row is this path" in a binary search.
    ///
    /// The path is a file here only if the directory table does not hold it.
    /// A path that names a directory has descendants to find and goes to
    /// [`NativeIndex::kill_under`] as before; a path that names neither is
    /// looked up, missed, and costs nothing.
    ///
    /// Returns the prefixes that still need the scan.
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
                // No source yet means no way to key a lookup, so the scan
                // has to answer it. Dropping it instead would lose the
                // removal outright, which is what the first version did.
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
                    // prefix holds nothing this removal is about. See
                    // `Doomed::touches`.
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

    /// Put every finished build in the list. The caller holds the write lock.
    ///
    /// Called from every path that takes it, because a segment sitting in
    /// `landed` is a set of rows that exist on disk and answer no query.
    fn collect(&self, inner: &mut Inner) -> Result<()> {
        let done: Vec<Landed> = {
            let mut held = self.building.0.lock();
            if held.landed.is_empty() {
                return Ok(());
            }
            std::mem::take(&mut held.landed)
        };
        let mut added = false;
        let mut failure = None;
        for one in done {
            match one {
                Landed::Built { number, mut live } => {
                    // Whatever was removed while this was in the air. The rows
                    // did not exist to be killed then and do now.
                    let (doomed, gone) = {
                        let mut held = self.building.0.lock();
                        let mut taken = (Vec::new(), scour_core::PrefixSet::default());
                        held.flights.retain(|f| {
                            if f.number == number {
                                taken = (f.doomed.clone(), f.gone.clone());
                                false
                            } else {
                                true
                            }
                        });
                        taken
                    };
                    let mut hit = false;
                    if !doomed.is_empty() {
                        let mut wanted: Vec<(u32, SourceId, &str)> = doomed
                            .iter()
                            .map(|(k, s, p)| (*k, *s, p.as_str()))
                            .collect();
                        wanted.sort_unstable_by_key(|(k, _, _)| *k);
                        hit |= live.kill_paths(&wanted)? > 0;
                    }
                    if !gone.is_empty() {
                        hit |= Self::kill_under(&mut live, &gone)? > 0;
                    }
                    if hit {
                        let (n, bits) = live.alive_snapshot();
                        Live::write_alive(&self.dir, n, &bits)?;
                    }
                    inner.next_segment = inner.next_segment.max(number + 1);
                    inner.segments.push(live);
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
            self.save_meta(inner)?;
        }
        self.building.1.notify_all();
        match failure {
            Some(detail) => Err(Error::Io { detail }),
            None => Ok(()),
        }
    }

    /// Wait until nothing is being built, then put the results in the list.
    ///
    /// **Every operation that reasons about what the index contains has to do
    /// this first.** A sweep judges rows by generation, a fold rewrites them, a
    /// rebuild reads all of them: a segment still in the air is a set of rows
    /// none of those would see, and the failure is silent — rows that outlive a
    /// reconciliation they should have been judged by.
    fn settle(&self) -> Result<()> {
        {
            let mut held = self.building.0.lock();
            while !held.flights.is_empty() && held.landed.len() < held.flights.len() {
                self.building.1.wait(&mut held);
            }
        }
        self.collect(&mut self.inner.write())
    }

    /// Put back what a failed publication was carrying.
    ///
    /// The staged rows go in **behind** whatever arrived while the write was
    /// happening, and only where that has not already replaced them: an upsert
    /// that landed in the meantime is newer than the one being restored, and
    /// the whole point of the staging map is that a path appears once.
    fn restore(inner: &mut Inner, pending: Pending) {
        let prefixes = std::mem::take(&mut inner.hidden_prefixes);
        inner.hidden_prefixes = pending.prefixes;
        inner.hidden_prefixes.extend(prefixes.into_paths());
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

    /// Lift the staged entries out, leaving the index consistent without them.
    ///
    /// They were never searchable — the trait says a change is not durable
    /// until `commit` — so removing them from the buffer changes no answer.
    /// What it buys is that turning them into a segment, which is the only
    /// expensive part of a flush, can happen with the lock released.
    fn take_staged(inner: &mut Inner) -> Option<(u64, u64, Vec<Entry>)> {
        if inner.staged.is_empty() {
            return None;
        }
        let number = inner.next_segment;
        inner.next_segment += 1;
        let staged = std::mem::take(&mut inner.staged);
        inner.staged.shrink_to_fit();
        inner.staged_at.clear();
        inner.staged_at.shrink_to_fit();
        Some((number, inner.generation, staged))
    }

    /// Everything a flush does **except** building and writing the segment.
    ///
    /// Returns what still has to be written, so a caller that can afford to
    /// let go of the lock does — see [`Index::commit`]. Split rather than
    /// duplicated, because the order inside matters: the identities to kill
    /// are read *from* the staged entries, and taking them out first meant a
    /// re-indexed file kept its old row. Two tests said so immediately.
    fn flush_prepare(&self, inner: &mut Inner, discretionary: bool) -> Result<Pending> {
        // Anything that finished building belongs in the list before this
        // decides what to kill: a row that has just landed is a row this flush
        // may have to replace.
        self.collect(inner)?;
        if inner.staged.is_empty() && inner.hidden_prefixes.is_empty() {
            return Ok(Pending::default());
        }
        let mut touched = vec![false; inner.segments.len()];

        // Subtrees. One pass over each segment covers every prefix at once.
        //
        // By **directory number**, not by path. The first version rebuilt a
        // path for every live row of every segment and compared strings —
        // 2.1 M path constructions to delete one folder, with the write lock
        // held and every search waiting behind it. The directory table already
        // answers "is this row under that prefix" as a range check on a
        // column, which is the same thing `under:` uses to make scoping a
        // search a comparison rather than a scan.
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
            // A segment still being built holds rows this removal is about and
            // has none of them yet. It carries the prefixes and applies them
            // the moment it lands — otherwise a folder deleted during a scan
            // comes back with the segment that was in the air when it went.
            {
                let mut held = self.building.0.lock();
                for f in held.flights.iter_mut() {
                    f.gone.extend(prefixes.iter().map(str::to_owned));
                }
            }
            inner.staged.retain(|e| !prefixes.covers(&e.path));
            inner.hidden_prefixes = prefixes;
        }

        // The old row of everything being re-upserted.
        //
        // **By path**, which is the whole of the duplicate problem: the row a
        // save replaces is the one with the same name, whatever identity the
        // filesystem gave the new file. Keying this on an inode meant every
        // write-and-rename left the previous row in place — 267 of them at one
        // path, measured on the live index.
        //
        // One sorted list, one merge a segment. The obvious shape — look each
        // path up in each segment — is a binary search per path per segment,
        // and a bulk scan makes both numbers large at once: indexing ten
        // million entries spent most of a hundred seconds in probes that found
        // nothing. Sorting once puts them in the order the segment's table is
        // already in, and the whole check becomes one sequential pass.
        //
        // The old rows are killed *always*, not only when a generation says
        // they might exist. A cheaper rule exists — during a bulk pass every
        // existing row carries an older generation and `sweep` will take it —
        // but it is wrong the moment the engine skips a sweep, and the failure
        // is a duplicated row rather than an error.
        //
        // The two fields are borrowed apart rather than the paths copied. A
        // bulk flush stages a hundred thousand entries, and cloning a path for
        // each of them to satisfy the borrow checker was **0.32 µs an entry**
        // — a third of what writing an entry costs in total, spent on strings
        // that are three lines away from the originals.
        //
        // First, though: the rows that are already right leave the batch, so
        // neither the kill below nor the build after it is asked to do anything
        // about them. See `spare_unchanged`.
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
            // The same rows, in segments that do not exist yet. A path being
            // re-indexed while an earlier segment holding it is still being
            // written has no row to kill — and would have two the moment that
            // segment landed, which is the duplicate this index exists to make
            // impossible.
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

        // **A buffer that emptied itself has nothing to flush.** The overflow
        // that called this in is the only discretionary flush there is, and on
        // a rescan almost every entry in it leaves through `spare_unchanged` a
        // few lines up. Writing what is left anyway turned a rescan of an
        // untouched disk into nine segments of two rows each — one a batch, the
        // index twice as many segments as it started with, every search reading
        // all of them and a compaction owed for the rest.
        //
        // So the rows stay in the buffer and wait for the next batch, or for
        // the commit clock, which is a flush that is not discretionary.
        let pending = if discretionary && inner.staged.len() < MAX_STAGED {
            None
        } else {
            Self::take_staged(inner)
        };
        let _ = t_kill;
        // Copied, not written. The write is an `fsync` a segment and it happens
        // once a second; doing it here held the index for 33 to 56 ms while
        // every search waited.
        let mut alive: Vec<(u64, Vec<u8>)> = inner
            .segments
            .iter()
            .enumerate()
            .filter(|(i, _)| touched.get(*i).copied().unwrap_or(false))
            .map(|(_, live)| live.alive_snapshot())
            .collect();
        let prefixes = std::mem::take(&mut inner.hidden_prefixes);
        // Order, and it is the difference between a crash costing a commit and
        // a crash costing the index: the manifest stops naming these segments
        // *before* their files go. The other way round — which is how this was
        // written — leaves a window in which the manifest points at files that
        // are no longer there, and the index does not open again.
        let gone = self.forget_empty(inner);
        if let Err(e) = self.save_meta(inner) {
            Self::restore(
                inner,
                Pending {
                    staged: pending,
                    alive: Vec::new(),
                    prefixes,
                },
            );
            return Err(e);
        }
        for n in &gone {
            Live::erase(&self.dir, *n);
        }
        // **A segment that was swept empty is both touched and gone**, so its
        // bitmap is in the snapshot above *and* its files have just been
        // unlinked — and the caller writes the snapshot after releasing the
        // lock, which puts the file back. Nothing ever opens it and nothing
        // ever removes it. Found by counting: 182 orphan segments on the live
        // index against 55 in the manifest, almost all of them a lone `.alive`,
        // one of them 160 KB. They also inflate `bytes_on_disk`, which is the
        // size of the whole directory.
        alive.retain(|(n, _)| !gone.contains(n));
        Ok(Pending {
            staged: pending,
            alive,
            prefixes,
        })
    }

    /// Erase segments nothing is left alive in.
    ///
    /// Not housekeeping — a correctness-shaped performance bug. A full rescan
    /// stamps a new generation and the sweep kills every row of the old one,
    /// and until this ran the emptied segment stayed in the list and was walked
    /// end to end by every query. Measured on a real index: 1,204,270 rows read
    /// per search to produce nothing.
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
        gone
    }

    /// Fold these segments into one, dropping rows that are no longer live.
    ///
    /// Merge segments, **without holding the index against a search**.
    ///
    /// This used to run start to finish under the write lock, and on a real
    /// index that meant a query issued during a rebuild waited for the whole
    /// rebuild: **22,984 ms**, measured, against 3 ms when nothing else was
    /// happening. A search box that is usually instant and occasionally
    /// twenty-three seconds is not a fast search box.
    ///
    /// It does not have to be that way, because a segment is written once and
    /// never edited. Reading N of them and writing one more touches nothing a
    /// search looks at; only the *list* changes, and swapping a list is
    /// microseconds. So the build happens under the **read** lock, which
    /// searches also hold and therefore do not queue behind, and the write
    /// lock is taken once at the end.
    ///
    /// What that admits is a commit waiting on a long fold — and the engine
    /// runs both on one worker thread, so it cannot happen there. A caller
    /// that does otherwise gets a slow commit rather than a wrong answer: the
    /// numbers folded are checked against the list again before the swap.
    fn fold(&self, which_numbers: &[u64]) -> Result<()> {
        // **Not while a walk is running.** Folding renumbers what is left, and
        // the marks that say "this row was seen unchanged" are keyed on the
        // number a row's segment had when it was seen. Renumbering leaves every
        // one of them pointing at a different row, and the sweep that follows
        // deletes files that are on the disk — silently, and reporting success.
        //
        // The condition is the marks themselves rather than "a walk is
        // running": a generation with nothing marked has nothing to invalidate,
        // and a test that folds mid-generation said so before this shipped.
        //
        // Refusing is free: a fold is housekeeping and the next one is a minute
        // away, while a walk is measured in seconds.
        if !self.inner.read().seen.is_empty() {
            return Ok(());
        }
        // **The number is claimed under the write lock, before anything is
        // built.** Reading `next_segment` under the read lock is not reserving
        // it: a commit takes the write lock meanwhile, claims the same number
        // in `take_staged`, and writes its eight files over the ones this fold
        // is about to write — or has already written and mapped.
        //
        // Found by the test below rather than reasoned about. With a writer
        // committing throughout, a rebuild came back `IndexCorrupt {
        // "seg-00000009.fnames is unreadable" }`: a segment whose files had
        // been replaced underneath a live mapping.
        let number = {
            let mut inner = self.inner.write();
            let n = inner.next_segment;
            inner.next_segment += 1;
            n
        };
        let (generation, bytes) = {
            let inner = self.inner.read();
            let segs: Vec<&Live> = inner
                .segments
                .iter()
                .filter(|s| which_numbers.contains(&s.number))
                .collect();
            if segs.len() < 2 && segs.iter().all(|s| s.dead_rows() == 0) {
                return Ok(());
            }
            // The merged segment can carry only one stamp, so the group must
            // either share one or be known to be entirely current — see
            // [`Inner::open`] and the caller.
            let generation = segs.iter().map(|s| s.generation).max().unwrap_or(0);
            let views: Vec<Segment<'_>> = segs.iter().map(|s| s.view()).collect::<Result<_>>()?;
            let bytes =
                build_sorted(&mut |emit: &mut dyn FnMut(&Entry)| merge_rows(&segs, &views, emit));
            (generation, bytes)
        };

        // The segment is written before the lock is taken: it is a new file
        // that nothing names yet, so nobody can be reading it.
        let folded = if bytes.names.is_empty() || bytes.alive.is_empty() {
            None
        } else {
            Some(Live::write(&self.dir, number, generation, &bytes)?)
        };
        drop(bytes);

        let mut inner = self.inner.write();
        // Between the read and the write another commit may have appended a
        // segment, and `next_segment` may have moved. Take a number that is
        // still free and fold only what is still there.
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
        // Newest last is what the search loop and `merge_rows` both assume; a
        // fold has to leave the order it found.
        inner.segments.sort_by_key(|s| s.number);
        self.save_meta(&inner)?;
        drop(inner);
        for n in old {
            Live::erase(&self.dir, n);
        }
        trim_allocator();
        Ok(())
    }

    /// Segments grouped by generation, **by number rather than by position**.
    ///
    /// A number survives the lock being dropped and a position does not, which
    /// matters now that a fold releases the lock while it builds: an index of
    /// eight segments can become nine underneath it, and index 3 would then be
    /// a different segment than the one that was chosen.
    fn groups(inner: &Inner) -> Vec<Vec<u64>> {
        let mut by_gen: HashMap<u64, Vec<u64>> = HashMap::new();
        for s in &inner.segments {
            by_gen.entry(s.generation).or_default().push(s.number);
        }
        let mut out: Vec<Vec<u64>> = by_gen.into_values().collect();
        out.sort_by_key(|g| std::cmp::Reverse(g.len()));
        out
    }

    /// The next group of segments worth folding, or nothing.
    ///
    /// Read under its own lock and answered in numbers, so the caller can let
    /// go of the index before it starts building.
    fn next_head(&self) -> Option<Vec<u64>> {
        let inner = self.inner.read();
        let group = Self::groups(&inner).into_iter().find(|g| g.len() >= 3)?;
        let rows = |n: u64| {
            inner
                .segments
                .iter()
                .find(|s| s.number == n)
                .map_or((0, 0), |s| (s.rows() as u64, s.dead_rows()))
        };
        let biggest = *group.iter().max_by_key(|&&n| rows(n).0)?;
        let (big_rows, big_dead) = rows(biggest);
        // A quarter of it dead is the point at which rewriting the largest
        // segment gives back more than it costs.
        if big_dead * 4 > big_rows {
            Some(group)
        } else {
            Some(group.into_iter().filter(|&n| n != biggest).collect())
        }
    }

    /// Hand each segment to `f`. For diagnostics that need to see inside.
    pub fn for_each_segment(&self, f: &mut dyn FnMut(usize, &Segment<'_>)) -> Result<()> {
        let inner = self.inner.read();
        for (i, live) in inner.segments.iter().enumerate() {
            f(i, &live.view()?);
        }
        Ok(())
    }

    /// Run `f` for every live row that the query accepts, across all segments.
    ///
    /// The shared walk behind counting and faceting. Stops when `f` returns
    /// `false`.
    /// Run `f` for every live row the query accepts, across all segments.
    ///
    /// The shared walk behind counting and faceting. Stops when `f` returns
    /// `false`.
    ///
    /// **Narrowed the same way a search is**, which it was not: this used to
    /// walk every row of every segment while `run_with` skipped whole blocks
    /// on the trigram filter and the zone map. So the sidebar cost more than
    /// the list beside it, on a design whose whole claim is that a sidebar can
    /// be recomputed on every keystroke. It also handed `accepts` the spelled
    /// name where a search hands it the folded one, which is a quiet wrong
    /// answer rather than a slow one.
    fn for_each_match(
        &self,
        inner: &Inner,
        query: &scour_core::Ast,
        mut f: impl FnMut(&Segment<'_>, usize) -> bool,
    ) -> Result<()> {
        for live in &inner.segments {
            let seg = live.view()?;
            let plan = Plan::compile(query, &seg)?;
            let go = crate::search::walk_matches(&seg, &plan, |row| {
                // The overlay is consulted here rather than inside the walk,
                // because it costs nothing when nothing is pending — which is
                // every moment except the second after a delete.
                if conceals(inner, &seg, row, b"") {
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

/// Which rows of one segment a batch of subtree removals takes.
///
/// The list of removed paths arrives in the thousands — a delete reports one
/// per file and one per directory, and a commit lands once a second — and the
/// obvious loop asks every row about every path. Measured on a million rows:
/// four thousand paths cost **2.24 seconds** with the write lock held, one
/// search behind it for every one of those seconds.
///
/// So the paths are turned into two things a row can be looked up in, once per
/// segment rather than once per row:
///
/// * the **directory numbers** below them, which the front-coded table makes
///   contiguous, merged into disjoint ranges;
/// * the ones identified by **parent and name** — a removed file has no
///   directory number of its own, and a removed directory's own row lives in
///   its parent and so carries the parent's number. `/home/u/Projeler`
///   surviving the removal of `/home/u/Projeler` is what taught that.
///
/// Both are sorted, so a row costs two binary searches and, for the few rows
/// whose parent is in the second list, one name comparison.
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

    /// Could any row in a block of directory numbers `lo..=hi` be taken?
    ///
    /// **The zone map, used for a removal the way a search already uses it.**
    /// Deleting one file was a scan of every row of every segment: measured at
    /// 4.58 ms over 250,000 rows, 19.99 ms over a million and 64.14 ms over
    /// four — linear in how much had been indexed, against 535 µs for an upsert
    /// of the same size, and it is the common case because a watcher reports
    /// every removed file this way.
    ///
    /// A block holds 128 rows and the column keeps its smallest and largest
    /// directory number. One deleted file lives under one directory, so almost
    /// every block can be dismissed on two comparisons rather than read.
    fn touches(&self, lo: u32, hi: u32) -> bool {
        if self.inside.iter().any(|&(s, e)| s <= hi && e > lo) {
            return true;
        }
        let at = self.named.partition_point(|&(parent, _)| parent < lo);
        self.named.get(at).is_some_and(|&(parent, _)| parent <= hi)
    }
}

/// Take out of the batch every entry the index already holds, exactly as it is.
///
/// **A rescan of an untouched filesystem should cost nothing to write**, and
/// used to cost the whole index: the walk hands over every entry it saw and
/// each one was staged, built into a segment and committed over a row that
/// already said the same thing.
///
/// Asked here rather than in [`Index::apply`], and that placement is the whole
/// of the performance. `apply` sees one entry at a time, so asking there is a
/// binary search per entry per segment — twenty segments deep by two million
/// entries, and it measured *slower* than writing the rows. Here the batch is
/// already assembled, so it is sorted once and merged against each segment's id
/// table in a single pass. See [`Live::spare_paths`].
///
/// Nothing is written and no live bit moves: an entry is dropped from the batch
/// and its row is marked in [`Inner::seen`] so the sweep knows the walk saw it.
fn spare_unchanged(inner: &mut Inner) -> Result<u64> {
    // Only inside a generation. Outside one there is no sweep coming, so
    // nothing needs the mark — and the marks are cleared when a generation
    // opens, which would make a spared row look unstamped to the sweep that
    // follows.
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
    // How much of a batch was already there, and how many segments it had to
    // be asked about. Both numbers, because a ratio that falls is either the
    // disk changing or this deciding wrongly, and the segment count is what
    // says which — a rescan that keeps adding segments is not sparing.
    if std::env::var_os("SCOUR_SPARE_TRACE").is_some() {
        scour_core::note!(
            "scourd: spare {spared} / {} across {} segments",
            drop_at.len(),
            inner.segments.len()
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
    // would have a later upsert overwrite an unrelated entry. Cheap because it
    // runs over what is *left*, which is the changed files.
    inner.staged_at.clear();
    for (i, e) in inner.staged.iter().enumerate() {
        inner.staged_at.insert(digest(e.id.source, &e.path), i);
    }
    inner.spared += spared;
    Ok(spared)
}

/// Is this row hidden by a removal that has not been committed yet?
///
/// Costs nothing when there are none, which is the normal state: the checks are
/// behind an emptiness test, and the dearer of the two — building the path —
/// only runs when a subtree is pending.
/// Should this row be hidden from a search that otherwise accepted it?
///
/// `name` is ignored, and deliberately: the walk yields the **folded** name
/// now, and a path built from that is lowercase — so `under("/home/u/projeler",
/// "/home/u/Projeler")` is false and a removed subtree comes back. The spelled
/// name is read from the other arena, which costs one lookup on a path that
/// already runs only for rows a query has accepted.
fn conceals(inner: &Inner, seg: &Segment<'_>, row: usize, _name: &[u8]) -> bool {
    if !inner.hidden_prefixes.is_empty() {
        let path = seg.path(row, seg.names.get(row).unwrap_or_default());
        if inner.hidden_prefixes.covers(&path) {
            return true;
        }
    }
    false
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

/// Emit every live row of every segment, in the merged stored order.
///
/// Each segment is already in that order, so this is a k-way merge and the
/// heap never holds more than one row a segment.
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

/// Give freed memory back to the operating system.
///
/// A fold builds the whole of a new segment in memory — a name arena, four
/// output buffers, a table of interned directories — and then drops it. glibc
/// keeps the freed arena in its own pools rather than returning it, so the
/// process stays large for the rest of its life having briefly needed the room.
/// Everywhere else this is someone else's problem and does nothing.
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

/// Remove segment files the manifest does not name.
///
/// Two things leave them behind, and the manifest is the answer to both: a
/// crash between the first of a segment's eight files and the last leaves a
/// partial set nothing will ever open, and a bug — since fixed — wrote back the
/// bitmap of a segment that had just been erased. On the live index that was
/// **182 orphan segments against 55 real ones**, and because `bytes_on_disk` is
/// the size of the whole directory, the status line counted them.
///
/// Safe by construction: the manifest is written before any file is unlinked
/// and rewritten before any is added, so a file it does not name is a file
/// nothing can reach. Failures are ignored — this is housekeeping, and an index
/// that opens with a stray file is better than one that refuses to open.
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
        // Only what is already behind the manifest's next number. A segment
        // being written right now by nobody-should-be-there is still not worth
        // racing, and `next_segment` is exactly the line between the two.
        if !named.contains(&number) && number < meta.next_segment {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| e.metadata().ok())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

/// Close a scan's compaction cohort before ordinary changes resume.
///
/// A generation originally ended only by clearing `open`, so every later
/// watcher commit kept the scan's stamp. Compaction then saw the scan body and
/// the trickle as one group: after a 1.55 M-row scan left 1,250,797 and 100,000
/// row segments, four one-row commits made the group eligible and rewrote the
/// 100,000-row segment. Measured: **477 ms CPU**, once a minute, which is 0.8%
/// of a core while the machine appears idle.
///
/// Advancing here changes no reconciliation answer. The scan's rows retain the
/// generation they were stamped with, while the next scan still receives a
/// newer number and judges every older row exactly as before. It only says
/// that subsequent commits are a different batch for compaction.
fn close_generation(inner: &mut Inner, generation: u64) {
    if inner.open == Some(generation) {
        inner.open = None;
        inner.generation = inner.generation.saturating_add(1);
    }
}

impl Index for NativeIndex {
    fn apply(&self, changes: &mut dyn Iterator<Item = Change>) -> Result<ApplyReport> {
        let mut inner = self.inner.write();
        let mut report = ApplyReport::default();
        for c in changes {
            match c {
                Change::Upsert(e) => {
                    if !inner.sources.contains(&e.id.source) {
                        inner.sources.push(e.id.source);
                    }
                    // A file that was removed and has come back must stop being
                    // hidden, or the row the user just created stays invisible.
                    //
                    // Behind the emptiness test because the common case by far
                    // is a bulk pass with nothing hidden at all, and asking a
                    // `HashSet<String>` about a path it does not hold still
                    // costs hashing that path.
                    if !inner.hidden_prefixes.is_empty() {
                        inner.hidden_prefixes.forget(&e.path);
                    }
                    let d = digest(e.id.source, &e.path);
                    // `get`, not `[]`. The two collections are cleared
                    // together in `flush` and I could not construct a case
                    // where the position outlives the buffer — but the cost of
                    // being sure is nothing, and the cost of being wrong is a
                    // panic inside a write lock in a long-lived service.
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
                        // The one place a build may happen elsewhere: this is
                        // a buffer overflowing during a walk, and nothing is
                        // waiting on the result.
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
        // What the flushes inside this call decided was already indexed. See
        // `Inner::spared` for why it is drained here rather than counted above.
        //
        // Moved out of `upserted` rather than added beside it: every spared
        // entry was counted as an upsert on the way in, and `seen()` adds the
        // two. Saturating because the batch that was spared is not always the
        // batch that was staged.
        let spared = std::mem::take(&mut inner.spared);
        report.upserted = report.upserted.saturating_sub(spared);
        report.unchanged = spared;
        Ok(report)
    }

    fn begin_generation(&self) -> Result<u64> {
        // Rows still being written would be stamped with the generation they
        // were staged under and judged by the one that starts here.
        self.settle()?;
        let mut inner = self.inner.write();
        // Whatever was open is finished: callers scan one at a time. A scan
        // that ended without sweeping — a cancelled walk, an unreadable root —
        // deliberately leaves nothing to reconcile.
        //
        // **Closed before the flush, not after.** The flush below would
        // otherwise spare rows into marks that the `clear` two lines down
        // throws away, leaving them unwritten *and* unstamped — which the next
        // sweep reads as "the walk did not find them".
        inner.open = None;
        inner.seen.clear();
        // Flush second, so that no segment ever spans two generations. That is
        // what lets the generation be one number a segment rather than a column
        // a row, and it is why `sweep` is a loop over segments and not a scan.
        self.flush(&mut inner)?;
        inner.generation += 1;
        let g = inner.generation;
        inner.open = Some(g);
        self.save_meta(&inner)?;
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
            for (i, live) in inner.segments.iter().enumerate() {
                if touched[i] {
                    let (number, bits) = live.alive_snapshot();
                    Live::write_alive(&self.dir, number, &bits)?;
                }
            }
            let gone_segments = self.forget_empty(&mut inner);
            self.save_meta(&inner)?;
            for n in &gone_segments {
                Live::erase(&self.dir, *n);
            }
        }
        Ok(gone)
    }

    fn sweep(
        &self,
        source: SourceId,
        under_path: &str,
        generation: u64,
        spare: &scour_core::PrefixSet,
    ) -> Result<u64> {
        // A segment in the air holds rows this generation stamped; sweeping
        // before it lands judges them by a walk that never saw them.
        self.settle()?;
        let mut inner = self.inner.write();
        self.flush(&mut inner)?;
        close_generation(&mut inner, generation);
        let mut gone = 0u64;
        let inner_seen = std::mem::take(&mut inner.seen);
        let mut touched = vec![false; inner.segments.len()];
        for (i, live) in inner.segments.iter_mut().enumerate() {
            if live.generation >= generation {
                continue;
            }
            let victims: Vec<usize> = {
                let seg = live.view()?;
                let whole = under_path.is_empty() || under_path == "/";
                let scope = seg.dirs.subtree(under_path);
                // The swept directory's **own** row is not under itself: it
                // lives in its parent and carries the parent's number, so the
                // range check walks straight past it. Same trap as in
                // `flush_prepare`, same answer — the parent's number and the
                // last component, and the name is read only for the handful of
                // rows that sit there.
                let own: Option<(u32, &str)> = (!whole)
                    .then(|| {
                        let p = under_path.trim_end_matches('/');
                        let (parent, name) = p.rsplit_once('/')?;
                        let parent = if parent.is_empty() { "/" } else { parent };
                        Some((seg.dirs.exact(parent)?, name))
                    })
                    .flatten();
                // **No path is built per row, and that is the whole cost of
                // this loop.** It used to fall back to
                // `under(&seg.path(row, …), under_path)` for every row the
                // directory scope did not already accept — which, for a walk of
                // one subtree, is every row of the index. Now that a created
                // directory queues a walk, this runs whenever anyone makes a
                // folder, and a string per row two million times over is not a
                // thing to do under the write lock.
                //
                // Nothing is lost by dropping it: a row's directory number *is*
                // its parent, so a descendant is in `scope.below` and a child is
                // `scope.own`. An empty scope with no `own` means the segment
                // holds nothing under the path at all.
                if !whole && scope.is_empty() && own.is_none() {
                    Vec::new()
                } else {
                    // Rows the walk found unchanged are stamped here rather
                    // than by being rewritten. Without this they look
                    // unstamped, and an untouched filesystem empties the
                    // index. See `Inner::seen`.
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
                            // judge.** A sweep says "I looked under here and
                            // did not find these"; where two sources' roots
                            // overlap, that is a statement about one of them
                            // and was being applied to both.
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
                            scope.contains(d)
                                || own.is_some_and(|(pd, name)| {
                                    pd == d && seg.names.get(row) == Some(name)
                                })
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
        for (i, live) in inner.segments.iter().enumerate() {
            if touched[i] {
                live.save_alive(&self.dir)?;
            }
        }
        // Same order as `flush`: forget, record, then unlink.
        let erased = self.forget_empty(&mut inner);
        self.save_meta(&inner)?;
        for n in erased {
            Live::erase(&self.dir, n);
        }
        Ok(gone)
    }

    /// Write everything staged, **without holding the index while it writes**.
    ///
    /// The engine calls this once a second. Everything a flush does except
    /// building and writing the segment is bookkeeping the lock has to cover;
    /// the build and the write are the seconds, and a search issued during
    /// them used to wait for all of it.
    ///
    /// So: take the staged entries out under the lock — they were never
    /// searchable, so nothing sees a different answer — release, build and
    /// write, and take the lock again to put the segment in the list. The
    /// window in between is one where the index holds exactly what it held
    /// before the commit, which is a state it is allowed to be in.
    ///
    /// The same shape as `fold`, and for the same reason: a segment is written
    /// once and never edited, so writing one touches nothing a search reads.
    fn commit(&self) -> Result<()> {
        #[cfg(feature = "memory-trace")]
        let trace_start = CommitStamp::now();
        // The bookkeeping half: kills, subtree removals, the manifest. It has
        // to be inside the lock because it edits rows other threads read, and
        // it is cheap now that a subtree is a range check rather than 2.1 M
        // paths.
        let held = Instant::now();
        // Everything that was being built is on disk and in the list before
        // this returns: the engine announces a revision on the strength of it,
        // and a window told to look again has to find what was written.
        self.settle()?;
        #[cfg(feature = "memory-trace")]
        let trace_settled = CommitStamp::now();
        let mut pending = self.flush_prepare(&mut self.inner.write(), false)?;
        #[cfg(feature = "memory-trace")]
        let trace_prepared = CommitStamp::now();
        // How long a search could have been waiting. Printed rather than
        // guessed at, because the last three things blamed for this tail were
        // each the wrong one.
        if std::env::var_os("SCOUR_LOCK_TRACE").is_some() && held.elapsed().as_millis() > 20 {
            eprintln!(
                "scourd: commit held the index for {:.0?} ({} staged)",
                held.elapsed(),
                pending.staged.as_ref().map_or(0, |(_, _, v)| v.len())
            );
        }
        // Both of the expensive halves, now that the lock is gone: the bits
        // that say which rows are dead, and the segment holding the new ones.
        //
        // **Every failure from here hands the rows back.** They are out of the
        // staging buffer and this is the only copy; dropping it on an `ENOSPC`
        // or a permission change loses whatever was written since the last
        // commit, and the engine used to be told the commit had succeeded.
        if let Err(e) = pending.write_alive(&self.dir) {
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
        let live = match Live::write(&self.dir, number, generation, &bytes) {
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
        let saved = self.save_meta(&inner);
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

        // Only when something is hidden. With no veto and no test that reads
        // names, the walk never touches the name arena at all.
        let hiding = !inner.hidden_prefixes.is_empty();

        let mut all: Vec<Hit> = Vec::new();
        let mut counted = 0u64;
        let mut budget = cap;
        let mut visited = 0u64;
        // Paths reconstructed, page and discarded prefix alike — see
        // `SearchResponse::rows_built`.
        let mut built = 0u64;
        let mut rows = 0u64;
        let mut veto =
            |seg: &Segment<'_>, row: usize, name: &[u8]| conceals(&inner, seg, row, name);
        for live in &inner.segments {
            rows += live.rows() as u64;
            let seg = live.view()?;
            let plan = Plan::compile(&req.query, &seg)?;
            // **A query with no conditions matches every live row**, and how
            // many that is is a number the segment already keeps.
            //
            // Nothing hidden, nothing to test: the walk's only remaining job
            // is to produce the page, and rows are stored newest-first so it
            // stops as soon as the page is full. What it was doing instead was
            // visiting all of them to arrive at a total — measured on this
            // index, `/api/count` on the empty query took **1.233 s** across
            // 2,094,185 rows, against 5.5 ms for the same query's first page.
            // A window opens showing the empty query, so that second and a
            // quarter was part of every time one was opened.
            let matches_all = !hiding && plan.is_empty();
            if matches_all && need == 0 {
                // A count and nothing else. There is no page to build, so
                // there is nothing left to walk for.
                let live_rows = live.live_rows();
                counted += live_rows;
                budget = budget.saturating_sub(live_rows as usize);
                continue;
            }
            let found = run_with(
                &seg,
                &plan,
                Wanted {
                    sort: req.sort,
                    descending: req.descending,
                    // Nothing is skipped per segment: the row that a global
                    // offset skips may live in any of them, so paging is
                    // applied once, after the merge.
                    offset: 0,
                    limit: need,
                    // What is left of the *global* count, not a fresh copy of
                    // it. Handing every segment the whole cap was measured at
                    // thirteen times the cost of one segment on a fragmented
                    // index: each one walked until it had found five hundred
                    // matches of its own, and thirty-two of those is the whole
                    // corpus. With the budget shared, a segment that cannot
                    // add to the count stops as soon as it has enough rows to
                    // be considered for the page — which is what it is for.
                    // Everything matches, so the walk needs only enough rows
                    // to fill the page; the total comes from the segment.
                    count_cap: if matches_all { need } else { budget },
                },
                hiding.then_some(&mut veto as &mut dyn FnMut(&Segment<'_>, usize, &[u8]) -> bool),
            );
            let total = if matches_all {
                live.live_rows()
            } else {
                found.total
            };
            counted += total;
            budget = budget.saturating_sub(total as usize);
            visited += found.rows_visited;
            built += found.rows_built;
            all.extend(found.hits);
        }

        // The merge across segments has to score against the same terms the
        // segments did. `narrowing_terms` is the query's own answer to "what
        // must a name contain", which is the same question relevance asks.
        let terms = req.query.narrowing_terms(1);
        sort_hits(&mut all, req.sort, req.descending, &terms);
        let hits: Vec<Hit> = all.into_iter().skip(offset).take(limit).collect();
        Ok(SearchResponse {
            hits,
            total: counted.min(cap as u64),
            capped: counted >= cap as u64,
            took_us: started.elapsed().as_micros() as u64,
            // Did the walk get away with looking at less than everything?
            //
            // Not "did every segment stop", which was the first definition and
            // was useless: a segment holding fewer matches than a page runs to
            // its own end and reports no early exit, so one small segment made
            // a query that had stopped after four hundred rows out of a million
            // report a full scan.
            fast_path: rows > 0 && visited < rows,
            rows_visited: visited,
            rows_built: built,
        })
    }

    /// Every question about the matching set, from **one** walk of it.
    ///
    /// The sidebar wants a count, a breakdown by kind and a distribution by
    /// age, and each of those used to be its own request — three walks of the
    /// same rows to produce three views of them, 100 to 200 ms behind a
    /// keystroke on 2.1 M entries. They are answered together now, which is a
    /// third of the work by construction and needs no cleverness at all: the
    /// walk was always the cost and the counting never was.
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
        // distribution cannot be sampled (see `AGE_SCAN_CAP`), so asking for
        // one alongside a top-ten makes the top-ten exact as a side effect.
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
        // Read once per row however many questions want it, and not at all
        // when none does — which is the common case, since kind and age are
        // both columns.
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
                        // The token, not the label: a rail turns a facet into
                        // a `kind:` term, and a label can be two words and can
                        // be translated. `by` in the reply is what tells the
                        // renderer to translate it back for display.
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
                        // Linear because there are a couple of dozen and a
                        // binary search over that is not worth the branch.
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
            rollup.add_segment(&live.view()?);
        }
        rollup.finish(started)
    }

    fn stats(&self) -> Result<IndexStats> {
        let inner = self.inner.read();
        let mut entries = 0u64;
        let mut dirs = 0u64;
        let mut largest = 0u64;
        // Read, not walked. This used to count directories by visiting every
        // row of every segment, and the engine calls it once a second to
        // decide whether to compact — 2.1 M rows a second, one whole core, and
        // the read lock held against every search while it happened.
        for live in &inner.segments {
            let live_rows = live.live_rows();
            entries += live_rows;
            largest = largest.max(live_rows);
            dirs += live.dirs() as u64;
        }
        Ok(IndexStats {
            entries,
            dirs,
            bytes_on_disk: dir_size(&self.dir),
            segments: inner.segments.len() as u32,
            // Everything outside the largest segment. After a rebuild that is
            // nothing; it grows with every commit, and it is what a search pays
            // for twice — once per segment that cannot stop early.
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
                // There is no arena to give back — the segments are mapped, so
                // what they cost is page cache the kernel reclaims on its own.
                // All this can return is the staging buffer.
                let mut inner = self.inner.write();
                self.flush(&mut inner)?;
                inner.staged.shrink_to_fit();
                inner.staged_at.shrink_to_fit();
            }
            // Fold the head; leave the body alone.
            //
            // What a search pays for is the *number* of segments, not their
            // size: at 1,083,334 entries one segment answers `"rapor"` in 1.11
            // ms and eleven answer it in 5.20, because each of the eleven has
            // to find its own page before it can stop. Folding ten of them
            // into one leaves two, and costs a pass over a tenth of the index
            // instead of over all of it.
            //
            // The largest member of the group is what a rebuild left behind,
            // so it is the one worth not rewriting — unless a quarter of it is
            // rows nobody can see any more, at which point rewriting is what
            // gives the space back.
            Maintenance::Compact => {
                self.flush(&mut self.inner.write())?;
                // The lock is taken to *choose* and released to *build*. Each
                // round re-reads the list, because folding renumbers what is
                // left and because a commit may have added to it meanwhile. A
                // group of eleven becomes two, which no longer qualifies, so
                // this terminates.
                while let Some(head) = self.next_head() {
                    self.fold(&head)?;
                }
            }
            // Everything becomes one segment, generations included.
            //
            // What makes that safe is not that generations do not matter —
            // they do, and a merged segment carries one stamp — but that
            // outside a scan there is nothing left for one to decide. Folding
            // per generation was the old rule and it meant a second source
            // pinned the index at two segments forever: each source's scan
            // takes its own generation. Measured at 2,951,074 entries, two
            // segments, 1,441,890 of them unsorted, `rapor` at 125 ms.
            //
            // **The work is decided once, before the first fold.** This used
            // to be a loop that re-read the list and folded again while
            // anything was left to fold, which cannot finish on a machine that
            // is being used: a fold releases the lock while it builds — that is
            // what makes it safe against searches — so a commit lands during
            // it and appends a segment to the very generation just folded, and
            // the loop folds the whole index again. Measured: `scour maintain
            // rebuild` ran for **over ten minutes** on 2.1 M entries and was
            // still at 38 segments when it was given up on.
            //
            // `fold` re-checks which of the numbers it was given still exist,
            // so a group that a previous round already absorbed costs nothing.
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
                    self.fold(&group)?;
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

        // A source with several vouched roots sweeps once per root. Only the
        // first closes the scan; the remaining roots must not keep advancing.
        close_generation(&mut inner, 7);
        assert_eq!(inner.generation, 8);
    }
}
