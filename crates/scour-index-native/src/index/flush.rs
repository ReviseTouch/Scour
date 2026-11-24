//! Staged rows made a segment, published, and written to disk — in that order,
//! and each step undone if a later one fails.

use super::*;
use crate::build::build;
use crate::directory_bytes::DirectoryBytes;
use crate::ids::digest;
use crate::segment::Live;
use scour_core::{Entry, Error, Result, SourceId};
use std::path::Path;
use std::time::Instant;

/// How many entries may wait in memory before a segment is written.
/// 100k is ~25 MB of `Entry` and leaves eleven segments after a million: a
/// search at 5.15 ms against 1.02 until a 2.7 s compaction.
pub(super) const MAX_STAGED: usize = 100_000;

/// What a flush left for the caller to write once it has let go of the index.
/// **It holds the only copy of those rows**, so every path that can fail after
/// this point hands it to [`NativeIndex::restore`] rather than dropping it.
#[derive(Debug, Default)]
pub(super) struct Pending {
    /// The new segment's number, generation and rows.
    pub(super) staged: Option<(u64, u64, Vec<Entry>)>,
    /// Live bits to replace, by segment number.
    alive: Vec<(u64, Vec<u8>)>,
    /// The removals this flush was going to make durable.
    prefixes: scour_core::PrefixSet,
}

impl Pending {
    pub(super) fn write_alive(&self, dir: &Path, disk_bytes: &DirectoryBytes) -> Result<()> {
        disk_bytes.changing(|| {
            for (number, bits) in &self.alive {
                Live::write_alive(dir, *number, bits)?;
            }
            Ok(())
        })
    }
}

/// A segment being built on another thread, as invisible as staged rows. It
/// carries the set of paths it holds: a removal arriving while it is in the air
/// has no row to kill yet.
#[derive(Debug, Default)]
pub(super) struct Flight {
    pub(super) number: u64,
    /// The path digests this segment will hold.
    pub(super) paths: std::collections::HashSet<u64>,
    /// What to kill in it the moment it lands, in `kill_paths` order.
    pub(super) doomed: Vec<(u32, SourceId, String)>,
    /// Subtrees removed while it was in the air. Applied on landing for the
    /// same reason: the rows were not there to be killed.
    pub(super) gone: scour_core::PrefixSet,
}

/// A build that has finished.
#[derive(Debug)]
pub(super) enum Landed {
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
pub(super) struct Building {
    /// Handed to a thread and not yet finished.
    pub(super) flights: Vec<Flight>,
    /// Finished, waiting for someone holding the write lock to install them.
    pub(super) landed: Vec<Landed>,
}

/// The most staged rows a publish keeps in memory; a larger batch is committed.
/// A trickle of a second is tens of rows, and a segment costs about 150 bytes a
/// row, so this is 7.5 MB.
pub(super) const PUBLISH_MAX_STAGED: usize = 50_000;

/// The most rows kept in memory across published segments before they are
/// written anyway, however soon the next persist was due.
pub(super) const UNWRITTEN_MAX_ROWS: usize = 200_000;

/// How many segments may be built at once. Each holds its rows and its output
/// bytes — about 35 MB for a full segment — so this is a memory bound; past it
/// the caller builds the flush itself.
const MAX_BUILDING: usize = 4;

impl NativeIndex {
    /// Turn everything staged and hidden into files, lock held throughout, for
    /// callers whose next line needs the rows in. Removals are applied *before*
    /// the new segment: one of them is the old row of every re-upserted entry.
    pub(super) fn flush(&self, inner: &mut Inner) -> Result<()> {
        self.flush_maybe_elsewhere(inner, false)
    }

    pub(super) fn flush_maybe_elsewhere(&self, inner: &mut Inner, elsewhere: bool) -> Result<()> {
        // `elsewhere` says this flush happens because a buffer filled up, not
        // because a caller needs the rows in.
        let mut pending = self.flush_prepare(inner, elsewhere, true)?;
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

    /// Put every finished build in the list; the caller holds the write lock.
    /// A segment sitting in `landed` exists on disk and answers no query.
    pub(super) fn collect(&self, inner: &mut Inner) -> Result<()> {
        let done: Vec<Landed> = {
            let mut held = self.building.0.lock();
            std::mem::take(&mut held.landed)
        };
        // Nothing landed, nothing to record. A manifest left dirty by something
        // else is saved by the next durable write, not by whoever settles first:
        // that was a sync under the lock after every begun walk.
        if done.is_empty() {
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
    pub(super) fn settle(&self) -> Result<()> {
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
    pub(super) fn restore(inner: &mut Inner, pending: Pending) {
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
        // A segment only in memory has no bitmap file to replace: its bits go
        // down with the rest of it.
        inner
            .segments
            .iter()
            .filter(|live| dirty.contains(&live.number) && !inner.unwritten.contains(&live.number))
            .map(Live::alive_snapshot)
            .collect()
    }

    /// Persist dirty bitmaps while the caller holds the write lock. A failed
    /// replacement leaves the stamps behind, so the next invocation retries even
    /// though killing an already-dead row reports zero.
    pub(super) fn write_dirty_alive(&self, inner: &mut Inner) -> Result<()> {
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
    fn take_staged(inner: &mut Inner, durable: bool) -> Option<(u64, u64, Vec<Entry>)> {
        if inner.staged.is_empty() {
            return None;
        }
        let number = inner.next_segment;
        inner.next_segment += 1;
        // A number handed to a segment that stays in memory is never on disk, so
        // the manifest has nothing to learn from it until that segment is.
        inner.meta_dirty |= durable;
        let staged = std::mem::take(&mut inner.staged);
        inner.staged.shrink_to_fit();
        inner.staged_at.clear();
        inner.staged_at.shrink_to_fit();
        Some((number, inner.generation, staged))
    }

    /// Everything a flush does **except** building and writing the segment, so a
    /// caller that can let go of the lock does. The order inside matters: the
    /// identities to kill are read *from* the staged entries.
    pub(super) fn flush_prepare(
        &self,
        inner: &mut Inner,
        discretionary: bool,
        durable: bool,
    ) -> Result<Pending> {
        // Anything that finished building belongs in the list first: a row that
        // has just landed is a row this flush may have to replace.
        self.collect(inner)?;
        if inner.staged.is_empty()
            && inner.hidden_prefixes.is_empty()
            && inner.dirty_alive.is_empty()
        {
            if durable && inner.meta_dirty {
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
            Self::take_staged(inner, durable)
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
        // Only when durable: a publish is not a sync, and saving here synced the
        // manifest under the lock whenever a walk had begun — twice a subtree walk.
        if durable
            && inner.meta_dirty
            && let Err(e) = self.save_meta(inner)
        {
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

    /// A commit that stops short of the disk: the staged rows become a segment in
    /// memory and removals are made in memory, all of it searchable at once and
    /// none of it synced. [`NativeIndex::persist_unwritten`] writes it down.
    pub(super) fn publish_now(&self) -> Result<()> {
        self.settle()?;
        let mut pending = {
            let mut inner = self.inner.write();
            let pending = self.flush_prepare(&mut inner, false, false)?;
            // Killed in memory; the bitmaps are written with the next persist.
            for (number, _) in &pending.alive {
                inner.dirty_alive.insert(*number);
            }
            pending
        };
        pending.alive.clear();
        let Some((number, generation, staged)) = pending.staged.take() else {
            return Ok(());
        };
        let live = match Live::from_bytes(number, generation, build(&staged)) {
            Ok(live) => live,
            Err(e) => {
                pending.staged = Some((number, generation, staged));
                Self::restore(&mut self.inner.write(), pending);
                return Err(e);
            }
        };
        drop(staged);
        let mut inner = self.inner.write();
        inner.next_segment = inner.next_segment.max(number + 1);
        inner.segments.push(live);
        inner.segments.sort_by_key(|s| s.number);
        inner.unwritten.insert(number);
        Ok(())
    }

    /// [`NativeIndex::publish_now`] for a caller already holding the lock, which
    /// also builds the segment under it: for a few rows, where order matters.
    pub(super) fn publish_locked(&self, inner: &mut Inner) -> Result<()> {
        let mut pending = self.flush_prepare(inner, false, false)?;
        for (number, _) in &pending.alive {
            inner.dirty_alive.insert(*number);
        }
        pending.alive.clear();
        let Some((number, generation, staged)) = pending.staged.take() else {
            return Ok(());
        };
        let live = match Live::from_bytes(number, generation, build(&staged)) {
            Ok(live) => live,
            Err(e) => {
                pending.staged = Some((number, generation, staged));
                Self::restore(inner, pending);
                return Err(e);
            }
        };
        inner.next_segment = inner.next_segment.max(number + 1);
        inner.segments.push(live);
        inner.segments.sort_by_key(|s| s.number);
        inner.unwritten.insert(number);
        Ok(())
    }

    /// The published segments in groups a fold may take together: all of them
    /// outside a walk, one generation apiece inside one — as [`Self::groups`].
    pub(super) fn unwritten_groups(inner: &Inner) -> Vec<Vec<u64>> {
        Self::groups(inner)
            .into_iter()
            .map(|g| {
                g.into_iter()
                    .filter(|n| inner.unwritten.contains(n))
                    .collect::<Vec<_>>()
            })
            .filter(|g| !g.is_empty())
            .collect()
    }

    /// Write down what publishing kept in memory: the segments of a generation
    /// folded into one where a fold is allowed and each as it is where not, then
    /// the live bits and the manifest. Every sync the index owes, at once.
    pub(super) fn persist_unwritten(&self) -> Result<()> {
        // One at a time; the second finds the first's work done. Taken with no
        // other lock held, and every caller comes here holding none.
        let _one = self.persisting.lock();
        let groups = Self::unwritten_groups(&self.inner.read());
        for group in groups {
            // Refused while a walk has marked them, or overtaken by a commit:
            // then each is written under the number the marks know it by.
            if group.len() > 1 && self.fold_to(&group, true)? {
                continue;
            }
            for number in group {
                self.write_unwritten(number)?;
            }
        }
        let mut inner = self.inner.write();
        self.write_dirty_alive(&mut inner)?;
        if inner.meta_dirty {
            self.save_meta(&mut inner)?;
        }
        Ok(())
    }

    /// Put one in-memory segment on disk under its own number. The files are
    /// written without the lock; if rows died in the meantime, its bitmap is
    /// written again with the manifest.
    fn write_unwritten(&self, number: u64) -> Result<()> {
        let (generation, bytes, deaths) = {
            let inner = self.inner.read();
            let Some(live) = inner.segments.iter().find(|s| s.number == number) else {
                return Ok(());
            };
            if !inner.unwritten.contains(&number) {
                return Ok(());
            }
            (live.generation, live.bytes(), live.deaths())
        };
        #[cfg(test)]
        self.unwritten_writes
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut written = self
            .disk_bytes
            .changing(|| Live::write(&self.dir, number, generation, &bytes))?;
        drop(bytes);
        let mut inner = self.inner.write();
        let Some(at) = inner.segments.iter().position(|s| s.number == number) else {
            // Folded or emptied while it was being written: nothing names these.
            drop(inner);
            drop(written);
            self.disk_bytes.changing(|| Live::erase(&self.dir, number));
            return Ok(());
        };
        written.adopt_alive(&inner.segments[at]);
        if inner.segments[at].deaths() != deaths {
            inner.dirty_alive.insert(number);
        }
        inner.segments[at] = written;
        inner.unwritten.remove(&number);
        inner.meta_dirty = true;
        Ok(())
    }
}
