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
    ApplyReport, Change, Entry, EntryId, Error, Facet, FacetBy, FacetRequest, FacetResponse, Hit,
    Index, IndexStats, Kind, MaintReport, Maintenance, Result, SearchRequest, SearchResponse,
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
const FORMAT: u32 = 7;

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
#[derive(Debug, Default)]
struct Pending {
    /// The new segment's number, generation and rows.
    staged: Option<(u64, u64, Vec<Entry>)>,
    /// Live bits to replace, by segment number.
    alive: Vec<(u64, Vec<u8>)>,
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
    /// Where each staged identity sits, so a second upsert of the same file
    /// replaces the first instead of adding a second row for it.
    staged_at: HashMap<u64, usize>,
    /// Removals that have taken effect for searches but not yet for the files.
    hidden: HashMap<u64, EntryId>,
    /// Subtrees in the same state.
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

#[derive(Debug)]
pub struct NativeIndex {
    dir: PathBuf,
    inner: RwLock<Inner>,
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
        let meta: Meta = match std::fs::read_to_string(dir.join(META_FILE)) {
            Ok(s) => serde_json::from_str(&s).map_err(|e| Error::IndexCorrupt {
                detail: format!("{META_FILE}: {e}"),
            })?,
            Err(_) => Meta::default(),
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
    fn flush(&self, inner: &mut Inner) -> Result<()> {
        let pending = self.flush_prepare(inner)?;
        pending.write_alive(&self.dir)?;
        if let Some((number, generation, staged)) = pending.staged {
            let bytes = build(&staged);
            let live = Live::write(&self.dir, number, generation, &bytes)?;
            inner.segments.push(live);
            inner.segments.sort_by_key(|s| s.number);
            self.save_meta(inner)?;
        }
        Ok(())
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
    fn flush_prepare(&self, inner: &mut Inner) -> Result<Pending> {
        if inner.staged.is_empty() && inner.hidden.is_empty() && inner.hidden_prefixes.is_empty() {
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
            for (i, live) in inner.segments.iter_mut().enumerate() {
                let victims: Vec<usize> = {
                    let seg = live.view()?;
                    let doomed = Doomed::new(&seg, &prefixes);
                    if doomed.is_empty() {
                        Vec::new()
                    } else {
                        (0..live.rows())
                            .filter(|&row| {
                                live.is_alive(row) && doomed.takes(&seg, row, seg.dir_id(row))
                            })
                            .collect()
                    }
                };
                for row in victims {
                    live.kill(row);
                    touched[i] = true;
                }
            }
            inner.staged.retain(|e| !prefixes.covers(&e.path));
            inner.hidden_prefixes = prefixes;
        }

        // Named removals, and the old row of everything being re-upserted.
        //
        // One sorted list, one merge a segment. The obvious shape — look each
        // identity up in each segment — is a binary search per identity per
        // segment, and a bulk scan makes both numbers large at once: indexing
        // ten million entries spent most of a hundred seconds in probes that
        // found nothing. Sorting the identities once puts them in the same
        // order the segment's table is already in, and the whole check becomes
        // one sequential pass.
        //
        // The old rows are killed *always*, not only when a generation says
        // they might exist. A cheaper rule exists — during a bulk pass every
        // existing row carries an older generation and `sweep` will take it —
        // but it is wrong the moment the engine skips a sweep, and the failure
        // is a duplicated row rather than an error.
        let mut wanted: Vec<(u32, EntryId)> = inner
            .hidden
            .values()
            .chain(inner.staged.iter().map(|e| &e.id))
            .map(|id| (crate::ids::IdMap::key_of(id), id.clone()))
            .collect();
        wanted.sort_unstable_by_key(|(h, _)| *h);
        for (i, live) in inner.segments.iter_mut().enumerate() {
            if live.kill_ids(&wanted)? > 0 {
                touched[i] = true;
            }
        }
        let t_kill = Instant::now();
        let hidden_digests: Vec<u64> = inner.hidden.keys().copied().collect();
        inner
            .staged
            .retain(|e| !hidden_digests.contains(&digest(&e.id)));

        let pending = Self::take_staged(inner);
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
        inner.hidden.clear();
        inner.hidden_prefixes.clear();
        // Order, and it is the difference between a crash costing a commit and
        // a crash costing the index: the manifest stops naming these segments
        // *before* their files go. The other way round — which is how this was
        // written — leaves a window in which the manifest points at files that
        // are no longer there, and the index does not open again.
        let gone = self.forget_empty(inner);
        self.save_meta(inner)?;
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
    if !inner.hidden.is_empty() {
        let id = seg.entry_id(row);
        if inner
            .hidden
            .get(&digest(&id))
            .is_some_and(|hidden| *hidden == id)
        {
            return true;
        }
    }
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

impl Index for NativeIndex {
    fn apply(&self, changes: &mut dyn Iterator<Item = Change>) -> Result<ApplyReport> {
        let mut inner = self.inner.write();
        let mut report = ApplyReport::default();
        for c in changes {
            match c {
                Change::Upsert(e) => {
                    let d = digest(&e.id);
                    // A file that was removed and has come back must stop being
                    // hidden, or the row the user just created stays invisible.
                    inner.hidden.remove(&d);
                    // `get`, not `[]`. The two collections are cleared
                    // together in `flush` and I could not construct a case
                    // where the position outlives the buffer — but the cost of
                    // being sure is nothing, and the cost of being wrong is a
                    // panic inside a write lock in a long-lived service.
                    match inner.staged_at.get(&d).copied() {
                        Some(i) if inner.staged.get(i).is_some_and(|s| s.id == e.id) => {
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
                        self.flush(&mut inner)?;
                    }
                }
                Change::Remove(id) => {
                    inner.hidden.insert(digest(&id), id);
                    report.removed += 1;
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
        Ok(report)
    }

    fn begin_generation(&self) -> Result<u64> {
        let mut inner = self.inner.write();
        // Flush first, so that no segment ever spans two generations. That is
        // what lets the generation be one number a segment rather than a column
        // a row, and it is why `sweep` is a loop over segments and not a scan.
        self.flush(&mut inner)?;
        inner.generation += 1;
        let g = inner.generation;
        // Whatever was open is finished: callers scan one at a time. A scan
        // that ended without sweeping — a cancelled walk, an unreadable root —
        // deliberately leaves nothing to reconcile.
        inner.open = Some(g);
        self.save_meta(&inner)?;
        Ok(g)
    }

    fn sweep(&self, under_path: &str, generation: u64) -> Result<u64> {
        let mut inner = self.inner.write();
        self.flush(&mut inner)?;
        if inner.open == Some(generation) {
            inner.open = None;
        }
        let mut gone = 0u64;
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
                    (0..live.rows())
                        .filter(|&row| {
                            if !live.is_alive(row) {
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
        // The bookkeeping half: kills, subtree removals, the manifest. It has
        // to be inside the lock because it edits rows other threads read, and
        // it is cheap now that a subtree is a range check rather than 2.1 M
        // paths.
        let held = Instant::now();
        let pending = self.flush_prepare(&mut self.inner.write())?;
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
        pending.write_alive(&self.dir)?;
        let Some((number, generation, staged)) = pending.staged else {
            return Ok(());
        };
        let bytes = build(&staged);
        drop(staged);
        let live = Live::write(&self.dir, number, generation, &bytes)?;
        drop(bytes);

        let mut inner = self.inner.write();
        inner.next_segment = inner.next_segment.max(number + 1);
        inner.segments.push(live);
        inner.segments.sort_by_key(|s| s.number);
        self.save_meta(&inner)
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
        let hiding = !inner.hidden.is_empty() || !inner.hidden_prefixes.is_empty();

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
                    count_cap: budget,
                },
                hiding.then_some(&mut veto as &mut dyn FnMut(&Segment<'_>, usize, &[u8]) -> bool),
            );
            counted += found.total;
            budget = budget.saturating_sub(found.total as usize);
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

    fn facets(&self, req: &FacetRequest) -> Result<FacetResponse> {
        let started = Instant::now();
        let inner = self.inner.read();
        let mut counts: HashMap<String, u64> = HashMap::new();
        let mut seen = 0usize;
        let top = match &req.by {
            FacetBy::Kind => 16,
            FacetBy::Ext { top } | FacetBy::Dir { top, .. } => (*top).max(1) as usize,
            // Every band, or the chart has holes in it.
            FacetBy::Age { edges } => edges.len() + 1,
        };
        // `now` once, not per row: a walk of two hundred thousand rows that
        // asks the clock each time is asking it two hundred thousand times.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let cap = match &req.by {
            FacetBy::Age { .. } => AGE_SCAN_CAP,
            _ => FACET_SCAN_CAP,
        };
        let parent = match &req.by {
            FacetBy::Dir { path, .. } => path.trim_end_matches('/').to_owned(),
            _ => String::new(),
        };

        self.for_each_match(&inner, &req.query, |seg, row| {
            match &req.by {
                FacetBy::Kind => {
                    let k = Kind::from_u8(seg.num_of(Field::Kind, row) as u8).unwrap_or(Kind::File);
                    // The token, not the label: a rail turns a facet into a
                    // `kind:` term, and a label can be two words and can be
                    // translated. `by` in the reply is what tells the renderer
                    // to translate it back for display.
                    *counts.entry(k.token().to_owned()).or_default() += 1;
                }
                FacetBy::Ext { .. } => {
                    let ext = scour_core::ext_of(seg.names.get(row).unwrap_or_default());
                    if !ext.is_empty() {
                        *counts.entry(ext).or_default() += 1;
                    }
                }
                FacetBy::Dir { .. } => {
                    let path = seg.path(row, seg.names.get(row).unwrap_or_default());
                    if let Some(rest) = path
                        .strip_prefix(parent.as_str())
                        .and_then(|r| r.strip_prefix('/'))
                    {
                        let child = rest.split('/').next().unwrap_or(rest);
                        *counts.entry(child.to_owned()).or_default() += 1;
                    }
                }
                FacetBy::Age { edges } => {
                    let days = (now - seg.num_of(Field::Mtime, row)).max(0) / 86_400;
                    // The bands are ascending, so the first one it fits is its
                    // own. Linear because there are a couple of dozen of them
                    // and a binary search over that is not worth the branch.
                    let key = edges
                        .iter()
                        .find(|&&e| days <= e as i64)
                        .map(|e| e.to_string())
                        .unwrap_or_else(|| "older".to_owned());
                    *counts.entry(key).or_default() += 1;
                }
            }
            seen += 1;
            seen < cap
        })?;

        let mut facets: Vec<Facet> = counts
            .into_iter()
            .map(|(key, count)| Facet { key, count })
            .collect();
        facets.sort_unstable_by(|a, b| b.count.cmp(&a.count).then(a.key.cmp(&b.key)));
        facets.truncate(top);
        Ok(FacetResponse {
            facets,
            by: req.by.clone(),
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
            pending_removals: inner.hidden.len() as u64,
            // No extractor is registered yet. The column is reserved, not used.
            has_content: false,
        })
    }

    fn maintain(&self, level: Maintenance) -> Result<MaintReport> {
        let started = Instant::now();
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
