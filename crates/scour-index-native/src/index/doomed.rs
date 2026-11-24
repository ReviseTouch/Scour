//! Removing rows: paths, everything under a folder, and the segments left empty.

use super::*;
use crate::columns::Field;
use crate::ids::digest;
use crate::search::Segment;
use crate::segment::Live;
use scour_core::{Result, SourceId};
use std::collections::HashMap;
use std::time::Instant;

/// Which rows of one segment a batch of subtree removals takes. Asking every row
/// about every path measured **2.24 s** for four thousand paths over a million
/// rows with the write lock held, so they become two sorted lookups a segment:
/// the directory numbers below them, and the ones named by parent and name.
pub(super) struct Doomed<'a> {
    /// Half-open ranges of directory numbers, disjoint and sorted.
    inside: Vec<(u32, u32)>,
    /// `(parent number, name)`, sorted by parent.
    named: Vec<(u32, &'a str)>,
}

impl<'a> Doomed<'a> {
    pub(super) fn new(seg: &Segment<'_>, prefixes: &'a scour_core::PrefixSet) -> Doomed<'a> {
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

    pub(super) fn is_empty(&self) -> bool {
        self.inside.is_empty() && self.named.is_empty()
    }

    pub(super) fn takes(&self, seg: &Segment<'_>, row: usize, dir: u32) -> bool {
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
pub(super) fn spare_unchanged(inner: &mut Inner) -> Result<u64> {
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
pub(super) fn joined_path<'a>(dir: &'a str, name: &'a str) -> impl Iterator<Item = u8> + 'a {
    let (head, slash) = match dir {
        "" => ("", false),
        "/" => ("/", false),
        d => (d, true),
    };
    head.bytes()
        .chain(slash.then_some(b'/'))
        .chain(name.bytes())
}

/// How many of these paths sit under each folder six levels down, most first.
pub(super) fn by_folder(paths: &[String]) -> Vec<(String, usize)> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for p in paths {
        let parent = p.rsplit_once('/').map_or("", |(dir, _)| dir);
        let folder: String = parent.split('/').take(7).collect::<Vec<_>>().join("/");
        *counts.entry(folder).or_default() += 1;
    }
    let mut out: Vec<(String, usize)> = counts.into_iter().collect();
    out.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    out
}

impl NativeIndex {
    /// Remove paths that name a file, touching no other row; returns the prefixes
    /// that still need the scan. A watcher reports every removal as a subtree and
    /// almost all are one file, where scanning was 10.5 ms at half a million rows
    /// and 64.9 ms at four million against a binary search on the identity table.
    pub(super) fn kill_leaves<'p>(
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

    pub(super) fn kill_under(live: &mut Live, prefixes: &scour_core::PrefixSet) -> Result<u64> {
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

    /// The paths a sweep removed and the first rows its pass wrote, into the
    /// journal. `SCOUR_SWEEP_TRACE` only: paths are the user's.
    fn trace_sweep(inner: &Inner, generation: u64, gone: &[String]) {
        let mut fresh: Vec<String> = Vec::new();
        for live in inner.segments.iter().filter(|s| s.generation == generation) {
            let Ok(seg) = live.view() else { continue };
            fresh.extend(
                (0..live.rows())
                    .filter(|&row| live.is_alive(row))
                    .map(|row| seg.path(row, &seg.names.get(row).unwrap_or_default())),
            );
        }
        // Where they are, counted, before which they are: tens of thousands of
        // rows say little one at a time and a great deal by folder.
        for (what, paths) in [("gone", gone), ("fresh", fresh.as_slice())] {
            for (dir, n) in by_folder(paths).into_iter().take(15) {
                scour_core::note!("scourd: sweep {what} {n} under {dir}");
            }
            for p in paths.iter().take(10) {
                scour_core::note!("scourd: sweep {what} {p}");
            }
        }
    }

    /// Erase segments nothing is left alive in: an emptied segment left in the
    /// list is walked end to end by every query, 1,204,270 rows for nothing.
    pub(super) fn forget_empty(&self, inner: &mut Inner) -> Vec<u64> {
        let mut gone = Vec::new();
        inner.segments.retain(|s| {
            if s.rows() > 0 && s.live_rows() == 0 {
                gone.push(s.number);
                false
            } else {
                true
            }
        });
        // One that was only in memory has no files and no line in the manifest.
        let written: Vec<u64> = gone
            .iter()
            .copied()
            .filter(|n| !inner.unwritten.remove(n))
            .collect();
        if !written.is_empty() {
            inner.meta_dirty = true;
            inner.pending_erase.extend(written);
        }
        gone
    }
}

impl NativeIndex {
    /// The body of [`Index::sweep_many`].
    pub(super) fn sweep_paths(
        &self,
        parts: &[(SourceId, Vec<String>)],
        generation: u64,
        spare: &scour_core::PrefixSet,
    ) -> Result<u64> {
        let began = Instant::now();
        // A segment in the air holds rows this generation stamped; sweeping
        // before it lands judges them by a walk that never saw them.
        self.settle()?;
        // The walk's own rows, made searchable in memory rather than by the flush
        // below, which writes and syncs a segment with the lock held throughout:
        // on a busy disk that held every search for seconds. A persist writes them.
        self.publish_now()?;
        let mut inner = self.inner.write();
        // Whatever was staged since, which on the one worker thread is nothing.
        self.publish_locked(&mut inner)?;
        close_generation(&mut inner, generation);
        let mut gone = 0u64;
        // **Taken once, for every root of the walk.** These marks belong to the
        // pass rather than to any one root: taken per root, the live index
        // deleted three of its four roots on alternate walks.
        let inner_seen = std::mem::take(&mut inner.seen);
        let mut touched = vec![false; inner.segments.len()];
        let tracing = std::env::var_os("SCOUR_SWEEP_TRACE").is_some();
        let mut gone_paths: Vec<String> = Vec::new();
        for (i, live) in inner.segments.iter_mut().enumerate() {
            if live.generation >= generation {
                continue;
            }
            for (source, under) in parts {
                let (source, under) = (*source, &under[..]);
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
                        // Under a subtree, a row's directory number decides first:
                        // it is one column read where the source is another, and a
                        // block whose numbers miss every root is not read at all —
                        // the zone map `kill_under` reads. Every row of every
                        // segment was read here, write lock held, after each
                        // subtree walk: 58% of an idle service's CPU.
                        let in_scope = |row: usize| {
                            let d = seg.dir_id(row);
                            scopes.iter().any(|s| s.contains(d))
                                || owns.iter().any(|(pd, name)| {
                                    *pd == d && seg.names.get(row).as_deref() == Some(*name)
                                })
                        };
                        let block_matters = |block: usize| {
                            whole
                                || match seg.cols.block_range(Field::DirId, block) {
                                    Some((lo, hi)) if lo >= 0 => {
                                        let (lo, hi) = (lo as u32, hi as u32);
                                        scopes.iter().any(|s| s.intersects(lo, hi))
                                            || owns.iter().any(|(pd, _)| lo <= *pd && *pd <= hi)
                                    }
                                    _ => true,
                                }
                        };
                        let rows = live.rows();
                        let mut out = Vec::new();
                        for block in 0..rows.div_ceil(crate::columns::BLOCK) {
                            if !block_matters(block) {
                                continue;
                            }
                            let from = block * crate::columns::BLOCK;
                            let to = (from + crate::columns::BLOCK).min(rows);
                            out.extend((from..to).filter(|&row| {
                                if !live.is_alive(row) {
                                    return false;
                                }
                                // The walk saw it and it had not changed, so it
                                // was not rewritten. That is a stamp.
                                if spared(row) {
                                    return false;
                                }
                                if !whole && !in_scope(row) {
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
                                !(!spare.is_empty()
                                    && spare.covers(
                                        &seg.path(row, &seg.names.get(row).unwrap_or_default()),
                                    ))
                            }));
                        }
                        if tracing {
                            gone_paths.extend(out.iter().map(|&row| {
                                seg.path(row, &seg.names.get(row).unwrap_or_default())
                            }));
                        }
                        out
                    }
                };
                for row in victims {
                    if live.kill(row) {
                        gone += 1;
                        touched[i] = true;
                    }
                }
            }
        }
        let dirty = inner
            .segments
            .iter()
            .enumerate()
            .filter_map(|(i, live)| touched[i].then_some(live.number))
            .collect::<Vec<_>>();
        // Killed in memory, like a published removal, and written by the next
        // persist. Writing the bitmaps and the manifest here synced twice with
        // the lock held after every subtree walk: seconds each on a busy disk,
        // every search waiting. A crash before the persist brings the rows back
        // until the next walk of that tree.
        inner.dirty_alive.extend(dirty);
        // The manifest stops naming an emptied segment at the persist, and its
        // files go after that: forgetting is the only part done now.
        self.forget_empty(&mut inner);
        if std::env::var_os("SCOUR_SPARE_TRACE").is_some() {
            scour_core::note!("scourd: sweep {gone} satir sildi, {:.0?}", began.elapsed());
        }
        // What a walk found that the watcher had not said: the rows it removed and
        // the ones it wrote. Twenty of each is enough to tell a miss from the
        // churn of the walk's own seconds.
        if tracing {
            Self::trace_sweep(&inner, generation, &gone_paths);
        }
        Ok(gone)
    }
}
