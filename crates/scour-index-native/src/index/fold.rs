//! Folding segments into fewer: which groups are worth it, by size tier, and
//! the gate a test holds a fold at.

use super::*;
use crate::build::build_sorted;
use crate::search::Segment;
use crate::segment::Live;
use scour_core::{Entry, Result};
use std::collections::HashMap;

/// How much larger a segment has to be than everything below it before it stops
/// being folded with them — `K` in size-tiered arithmetic, see [`head_of`]. Two
/// would allow 24 segments at 5 M rows instead of 13, and a search pays per
/// segment: 1.11 ms for one against 5.20 for eleven at 1.08 M entries.
const TIER_RATIO: u64 = 4;

/// Split segments into size tiers, smallest first: `(number, rows)` walked by
/// ascending size, a new tier at the first member larger than [`TIER_RATIO`]
/// times everything below it. Ties break on the number, not the manifest order.
pub(super) fn size_tiers(sizes: impl IntoIterator<Item = (u64, u64)>) -> Vec<Vec<u64>> {
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
pub(super) struct Member {
    pub(super) number: u64,
    pub(super) rows: u64,
    pub(super) dead: u64,
}

/// Which of a group of segments to fold together next, or nothing. **The head is
/// the smallest size tier, not the whole of the rest**, so the tail absorbs a
/// trickle every ~113k rows of churn rather than once a minute, and the count
/// stays under `ceil(log4(rows)) + 1`: 13 at 5 M rows.
pub(super) fn head_of(members: &[Member]) -> Option<Vec<u64>> {
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
pub(super) struct FoldGate {
    pub(super) snapshot_ready: std::sync::Barrier,
    pub(super) resume: std::sync::Barrier,
}

#[cfg(test)]
impl FoldGate {
    pub(super) fn new() -> FoldGate {
        FoldGate {
            snapshot_ready: std::sync::Barrier::new(2),
            resume: std::sync::Barrier::new(2),
        }
    }
}

impl NativeIndex {
    #[cfg(test)]
    pub(super) fn gate_next_fold(&self, gate: std::sync::Arc<FoldGate>) {
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

    /// Fold these segments into one, dropping rows that are no longer live —
    /// **without holding the index against a search**. A segment is written once
    /// and never edited, so the build runs under the **read** lock; held
    /// throughout, a query issued during a rebuild waited **22,984 ms**.
    pub(super) fn fold(&self, which_numbers: &[u64]) -> Result<bool> {
        self.fold_to(which_numbers, true)
    }

    /// [`NativeIndex::fold`], ending on disk or, with `durable` false, in memory:
    /// segments published a second apart become one without a file being
    /// written, and the one is written when the index is persisted.
    pub(super) fn fold_to(&self, which_numbers: &[u64], durable: bool) -> Result<bool> {
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
            inner.meta_dirty |= durable;
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
        let mut folded = if bytes.fnames.is_empty() || bytes.alive.is_empty() {
            None
        } else if durable {
            Some(
                self.disk_bytes
                    .changing(|| Live::write(&self.dir, number, generation, &bytes))?,
            )
        } else {
            Some(Live::from_bytes(number, generation, bytes)?)
        };

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
            if durable {
                self.disk_bytes.changing(|| Live::erase(&self.dir, number));
            }
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
        if !durable && let Some(live) = &folded {
            inner.unwritten.insert(live.number);
        }
        inner.segments.extend(folded);
        // What was only in memory had no files to erase and no line to drop.
        let written: Vec<u64> = old
            .iter()
            .copied()
            .filter(|n| !inner.unwritten.remove(n))
            .collect();
        if durable || !written.is_empty() {
            inner.meta_dirty = true;
        }
        inner.pending_erase.extend(written);
        // Newest last is what the search loop and `merge_rows` both assume; a
        // fold has to leave the order it found.
        inner.segments.sort_by_key(|s| s.number);
        if inner.meta_dirty {
            self.save_meta(&mut inner)?;
        }
        drop(inner);
        trim_allocator();
        Ok(true)
    }

    /// Segments grouped by generation, **by number rather than by position**: a
    /// fold releases the lock while it builds, and eight segments can become nine
    /// underneath it.
    pub(super) fn groups(inner: &Inner) -> Vec<Vec<u64>> {
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
        // Segments only in memory are merged in memory and written by a persist;
        // folding them in here would write them a second at a time after all.
        let group = Self::groups(&inner)
            .into_iter()
            .map(|g| {
                g.into_iter()
                    .filter(|n| !inner.unwritten.contains(n))
                    .collect::<Vec<_>>()
            })
            .find(|g| g.len() >= 3)?;
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
}

impl NativeIndex {
    /// The body of [`Index::maintain`].
    pub(super) fn maintain_now(&self, level: Maintenance) -> Result<MaintReport> {
        let started = Instant::now();
        // A fold rewrites segments and a rebuild reads every row of every one
        // of them. Both have to see the ones still being written.
        self.settle()?;
        let before = dir_size(&self.dir);
        match level {
            Maintenance::Flush => {
                self.flush(&mut self.inner.write())?;
                self.persist_unwritten()?;
            }
            Maintenance::Idle => {
                // The segments are mapped, so what they cost is page cache the
                // kernel reclaims. All this can return is the staging buffer —
                // and what publishing held in memory, which a quiet moment writes.
                {
                    let mut inner = self.inner.write();
                    self.flush(&mut inner)?;
                    inner.staged.shrink_to_fit();
                    inner.staged_at.shrink_to_fit();
                }
                self.persist_unwritten()?;
                // And the mapped pages whatever came before touched.
                self.release_memory();
            }
            // Fold the head; leave the body alone. A search pays for the *number*
            // of segments: at 1,083,334 entries one answers `"rapor"` in 1.11 ms
            // and eleven in 5.20. The largest is spared unless a quarter is dead.
            Maintenance::Compact => {
                self.flush(&mut self.inner.write())?;
                // What publishing left a second at a time becomes one segment, in
                // memory — all of it outside a walk; a refusal leaves it for later.
                let unwritten: Vec<Vec<u64>> = Self::unwritten_groups(&self.inner.read())
                    .into_iter()
                    .filter(|g| g.len() > 1)
                    .collect();
                for group in unwritten {
                    self.fold_to(&group, false)?;
                }
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
                self.persist_unwritten()?;
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
