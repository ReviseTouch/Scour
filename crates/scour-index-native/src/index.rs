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
use crate::names::Folded;
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

const META_FILE: &str = "native-index.json";
/// Bumped when the files change shape **or when a stored value changes what it
/// means**. Version 2 added the trigram filter, version 3 the per-block minimum
/// and maximum, version 4 a distance byte a directory, and version 5 the file
/// kinds — where nothing changed shape at all and every row of an older index
/// would still decode, into the wrong answer. `kind:build` would find nothing
/// and half the sidebar would read `File`: nothing corrupt and everything
/// wrong, which is the case this constant exists for. Version 6 added the
/// folded name arena, without which a search has nothing to walk.
const FORMAT: u32 = 6;

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
    hidden_prefixes: Vec<String>,
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
    fn flush(&self, inner: &mut Inner) -> Result<()> {
        if inner.staged.is_empty() && inner.hidden.is_empty() && inner.hidden_prefixes.is_empty() {
            return Ok(());
        }
        let mut touched = vec![false; inner.segments.len()];

        // Subtrees. One pass over each segment covers every prefix at once.
        if !inner.hidden_prefixes.is_empty() {
            let prefixes: Vec<String> = inner.hidden_prefixes.clone();
            for (i, live) in inner.segments.iter_mut().enumerate() {
                let victims: Vec<usize> = {
                    let seg = live.view()?;
                    let mut out = Vec::new();
                    seg.names.walk(0, |row, name| {
                        if live.is_alive(row) {
                            let path = seg.path(row, &String::from_utf8_lossy(name));
                            if prefixes.iter().any(|p| under(&path, p)) {
                                out.push(row);
                            }
                        }
                        true
                    });
                    out
                };
                for row in victims {
                    live.kill(row);
                    touched[i] = true;
                }
            }
            inner
                .staged
                .retain(|e| !prefixes.iter().any(|p| under(&e.path, p)));
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
        let hidden_digests: Vec<u64> = inner.hidden.keys().copied().collect();
        inner
            .staged
            .retain(|e| !hidden_digests.contains(&digest(&e.id)));

        if !inner.staged.is_empty() {
            let number = inner.next_segment;
            inner.next_segment += 1;
            let bytes = build(&inner.staged);
            let live = Live::write(&self.dir, number, inner.generation, &bytes)?;
            inner.segments.push(live);
            inner.staged.clear();
            inner.staged.shrink_to_fit();
            inner.staged_at.clear();
            inner.staged_at.shrink_to_fit();
        }
        for (i, live) in inner.segments.iter().enumerate() {
            if touched.get(i).copied().unwrap_or(false) {
                live.save_alive(&self.dir)?;
            }
        }
        inner.hidden.clear();
        inner.hidden_prefixes.clear();
        // Order, and it is the difference between a crash costing a commit and
        // a crash costing the index: the manifest stops naming these segments
        // *before* their files go. The other way round — which is how this was
        // written — leaves a window in which the manifest points at files that
        // are no longer there, and the index does not open again.
        let gone = self.forget_empty(inner);
        self.save_meta(inner)?;
        for n in gone {
            Live::erase(&self.dir, n);
        }
        Ok(())
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
    /// They must share a generation, and that is not a formality: the merged
    /// segment can only carry one stamp, so folding across a generation
    /// boundary would give rows from an old pass a new one and a later
    /// [`Index::sweep`] would walk past exactly the rows it exists to remove.
    ///
    /// The entries are never all in memory at once: the merge runs twice, once
    /// to collect directories and names and once to write the columns, which
    /// costs a second read of what is already mapped and saves holding a
    /// million `Entry` values — about a quarter of a gigabyte at that size.
    fn fold(&self, inner: &mut Inner, which: &[usize]) -> Result<()> {
        let done = self.fold_inner(inner, which);
        // After `fold_inner` has returned, and not inside it: the segment it
        // built is half a gigabyte at ten million entries, and trimming while
        // that is still held gives back nothing.
        trim_allocator();
        done
    }

    fn fold_inner(&self, inner: &mut Inner, which: &[usize]) -> Result<()> {
        if which.len() < 2 && which.iter().all(|&i| inner.segments[i].dead_rows() == 0) {
            return Ok(());
        }
        // The merged segment can carry only one stamp, so the group must
        // either share one or be known to be entirely current — see
        // [`Inner::open`] and the caller.
        let generation = which
            .iter()
            .map(|&i| inner.segments[i].generation)
            .max()
            .unwrap_or(0);
        let number = inner.next_segment;
        inner.next_segment += 1;
        let bytes = {
            let segs: Vec<&Live> = which.iter().map(|&i| &inner.segments[i]).collect();
            let views: Vec<Segment<'_>> = segs.iter().map(|s| s.view()).collect::<Result<_>>()?;
            build_sorted(&mut |emit: &mut dyn FnMut(&Entry)| merge_rows(&segs, &views, emit))
        };
        let old: Vec<u64> = which.iter().map(|&i| inner.segments[i].number).collect();
        // A generation whose rows have all been swept folds to nothing, and
        // nothing is what should be left. Writing the empty segment anyway put
        // two of them in a real index — permanent, since a segment with no rows
        // has no dead rows either and so never qualifies to be folded again.
        let folded = if bytes.names.is_empty() || bytes.alive.is_empty() {
            None
        } else {
            Some(Live::write(&self.dir, number, generation, &bytes)?)
        };
        let mut keep = 0;
        inner.segments.retain(|_| {
            let drop = which.contains(&keep);
            keep += 1;
            !drop
        });
        inner.segments.extend(folded);
        // Newest last is what the search loop and `merge_rows` both assume; a
        // fold has to leave the order it found.
        inner.segments.sort_by_key(|s| s.number);
        self.save_meta(inner)?;
        for n in old {
            Live::erase(&self.dir, n);
        }
        Ok(())
    }

    /// Segment indices grouped by generation, largest group first.
    fn groups(inner: &Inner) -> Vec<Vec<usize>> {
        let mut by_gen: HashMap<u64, Vec<usize>> = HashMap::new();
        for (i, s) in inner.segments.iter().enumerate() {
            by_gen.entry(s.generation).or_default().push(i);
        }
        let mut out: Vec<Vec<usize>> = by_gen.into_values().collect();
        out.sort_by_key(|g| std::cmp::Reverse(g.len()));
        out
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
    fn for_each_match(
        &self,
        inner: &Inner,
        query: &scour_core::Ast,
        mut f: impl FnMut(&Segment<'_>, usize, &[u8]) -> bool,
    ) -> Result<()> {
        let mut fold = Folded::new();
        for live in &inner.segments {
            let seg = live.view()?;
            let plan = Plan::compile(query, &seg)?;
            let mut go = true;
            seg.names.walk(0, |row, name| {
                if seg.is_alive(row)
                    && plan.accepts(&seg, row, name, &mut fold)
                    && !conceals(inner, &seg, row, name)
                {
                    go = f(&seg, row, name);
                }
                go
            });
            if !go {
                break;
            }
        }
        Ok(())
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
        if inner.hidden_prefixes.iter().any(|p| under(&path, p)) {
            return true;
        }
    }
    false
}

/// Is `path` at or below `prefix`?
fn under(path: &str, prefix: &str) -> bool {
    let p = prefix.trim_end_matches('/');
    if p.is_empty() {
        return true;
    }
    path == p || (path.len() > p.len() && path.starts_with(p) && path.as_bytes()[p.len()] == b'/')
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
                    inner.hidden_prefixes.push(path);
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
                let scope = seg.dirs.subtree(under_path);
                let whole = under_path.is_empty() || under_path == "/";
                let mut out = Vec::new();
                seg.names.walk(0, |row, name| {
                    if live.is_alive(row) {
                        let in_scope = whole
                            || scope.contains(seg.dir_id(row))
                            || under(&seg.path(row, &String::from_utf8_lossy(name)), under_path);
                        if in_scope {
                            out.push(row);
                        }
                    }
                    true
                });
                out
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

    fn commit(&self) -> Result<()> {
        let mut inner = self.inner.write();
        self.flush(&mut inner)
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
        };
        let parent = match &req.by {
            FacetBy::Dir { path, .. } => path.trim_end_matches('/').to_owned(),
            _ => String::new(),
        };

        self.for_each_match(&inner, &req.query, |seg, row, name| {
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
                    let ext = scour_core::ext_of(&String::from_utf8_lossy(name));
                    if !ext.is_empty() {
                        *counts.entry(ext).or_default() += 1;
                    }
                }
                FacetBy::Dir { .. } => {
                    let path = seg.path(row, &String::from_utf8_lossy(name));
                    if let Some(rest) = path
                        .strip_prefix(parent.as_str())
                        .and_then(|r| r.strip_prefix('/'))
                    {
                        let child = rest.split('/').next().unwrap_or(rest);
                        *counts.entry(child.to_owned()).or_default() += 1;
                    }
                }
            }
            seen += 1;
            seen < FACET_SCAN_CAP
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
            capped: seen >= FACET_SCAN_CAP,
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
        for live in &inner.segments {
            let live_rows = live.live_rows();
            entries += live_rows;
            largest = largest.max(live_rows);
            let seg = live.view()?;
            for row in 0..live.rows() {
                if live.is_alive(row) && seg.num_of(Field::IsDir, row) != 0 {
                    dirs += 1;
                }
            }
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
        let mut inner = self.inner.write();
        match level {
            Maintenance::Flush => self.flush(&mut inner)?,
            Maintenance::Idle => {
                // There is no arena to give back — the segments are mapped, so
                // what they cost is page cache the kernel reclaims on its own.
                // All this can return is the staging buffer.
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
                self.flush(&mut inner)?;
                // Recomputed each time round, because folding renumbers what
                // is left. A group of eleven becomes two, which no longer
                // qualifies, so this terminates.
                while let Some(group) = Self::groups(&inner).into_iter().find(|g| g.len() >= 3) {
                    let biggest = *group
                        .iter()
                        .max_by_key(|&&i| inner.segments[i].rows())
                        .expect("a group is not empty");
                    let stale = inner.segments[biggest].dead_rows() * 4
                        > inner.segments[biggest].rows() as u64;
                    let head: Vec<usize> = if stale {
                        group
                    } else {
                        group.into_iter().filter(|&i| i != biggest).collect()
                    };
                    self.fold(&mut inner, &head)?;
                }
            }
            // Everything becomes one segment, generations included.
            //
            // It used to fold within a generation, on the reasoning that there
            // is only ever one: a scan re-upserts every file it finds under a
            // new stamp and the sweep removes what it did not. That is true of
            // *one* source and false of two — each source's scan takes its own
            // generation — so a second source meant a rebuild that could never
            // get below two segments however often it ran. Measured on a real
            // index of two sources: 2,951,074 entries, 2 segments, 1,441,890
            // of them unsorted, and `rapor` at 125 ms where one segment
            // answers in single digits.
            Maintenance::Rebuild => {
                self.flush(&mut inner)?;
                // Everything into one segment when nothing is mid-scan, and
                // one segment per generation when something is.
                //
                // It used to be per generation always, on the reasoning that
                // there is only ever one: a scan re-upserts every file it
                // finds under a new stamp and the sweep removes what it did
                // not. That is true of *one* source and false of two — each
                // source's scan takes its own generation — so a second source
                // meant a rebuild that could never get below two segments
                // however often it ran. Measured on a real index of two
                // sources: 2,951,074 entries, two segments, 1,441,890 of them
                // unsorted, and `rapor` at 125 ms where one segment answers in
                // single digits.
                //
                // What makes the merge safe is not that generations do not
                // matter — they do — but that outside a scan there is nothing
                // left for one to decide.
                if inner.open.is_none() && inner.segments.len() > 1 {
                    let all: Vec<usize> = (0..inner.segments.len()).collect();
                    self.fold(&mut inner, &all)?;
                }
                while let Some(group) = Self::groups(&inner)
                    .into_iter()
                    .find(|g| g.len() > 1 || inner.segments[g[0]].dead_rows() > 0)
                {
                    self.fold(&mut inner, &group)?;
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
    fn a_path_is_under_its_own_prefix_but_not_under_a_longer_name() {
        assert!(under("/home/u/Projeler", "/home/u/Projeler"));
        assert!(under("/home/u/Projeler/a.rs", "/home/u/Projeler"));
        assert!(under("/home/u/Projeler/a.rs", "/home/u/Projeler/"));
        // The trap the directory table had too: a sibling whose name starts
        // with the prefix is not inside it.
        assert!(!under("/home/u/Projeler-414/a.rs", "/home/u/Projeler"));
        assert!(!under("/home/u/Proj", "/home/u/Projeler"));
        assert!(under("/anything", "/"));
    }
}
