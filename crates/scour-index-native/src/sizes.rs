//! What a folder weighs, in O(1) per folder.
//!
//! Directory numbers are handed out in sorted path order, so a subtree is one
//! or two contiguous runs ([`DirTable::subtree`]) and a prefix sum over them is
//! a subtraction — 90 ms to build and 12.1 µs a folder over 1.97 M rows. Only
//! what this index holds is counted, so it reads smaller than `du`.

use std::collections::HashMap;

use crate::columns::Field;
use crate::search::Segment;
use crate::segment::Live;

/// One segment's totals, by directory number, prefix-summed: `disk[i]` covers
/// directories `0..i`, so the run `a..b` totals `disk[b] - disk[a]`. One extra
/// slot at the end makes that true for the last directory too.
///
/// **Built once, then corrected.** A build reads every row; on a four-million-row
/// segment that was 521 ms, and one file dying anywhere in it made the next page
/// with a folder pay it again. Rows only ever die, so what died since the build
/// is a short list subtracted at lookup, and a build waits for the list to grow.
#[derive(Debug, Default)]
pub struct Prefix {
    disk: Vec<u64>,
    files: Vec<u64>,
    /// What each *directory row* has under it, so a folder shown as `~13 GB`
    /// sorts as 13 GB rather than by its `Size` column, which is four kilobytes
    /// of entry table. In row order, so a lookup is a binary search.
    by_row: Vec<(u32, i64)>,
    /// Where each of those rows' subtree lies among the directory numbers, in
    /// the same order: its own number (`u32::MAX` if none), then the run below.
    scopes: Vec<[u32; 3]>,
    /// The live bits as of the last look: live there and dead now is a death.
    seen: Vec<u8>,
    /// Files dead since the build, by directory number, with what each held.
    /// Sorted, and summed alongside in `gone_disk`, one slot longer.
    gone: Vec<(u32, u64)>,
    gone_disk: Vec<u64>,
    /// The segment's death count as of the last look; anything else means look.
    deaths: u64,
    /// The death count `by_row` was last brought up to.
    rows_deaths: u64,
    /// Whether `by_row` and `scopes` have been built since the last build.
    rows_ready: bool,
}

impl Prefix {
    fn of(seg: &Segment<'_>, deaths: u64) -> Prefix {
        let mut prefix = Prefix::default();
        prefix.rebuild(seg, deaths);
        prefix
    }

    fn rebuild(&mut self, seg: &Segment<'_>, deaths: u64) {
        let n = seg.dirs.len();
        self.seen.clear();
        self.seen.extend_from_slice(seg.alive);
        self.gone.clear();
        self.gone_disk.clear();
        self.gone_disk.push(0);
        // Refreshed in the existing buffers: a replacement would double the peak.
        self.disk.resize(n + 1, 0);
        self.files.resize(n + 1, 0);
        self.disk.fill(0);
        self.files.fill(0);
        let (disk, files) = (&mut self.disk, &mut self.files);
        for row in 0..seg.rows() {
            // A directory's `st_size` is its entry table, not content, so its
            // own row is skipped. A row's share of a hard link is `disk/links`.
            if !seg.is_alive(row) {
                continue;
            }
            if seg.num_of(Field::IsDir, row) != 0 {
                continue;
            }
            let d = seg.dir_id(row) as usize;
            if d >= n {
                continue;
            }
            let links = seg.num_of(Field::Links, row).max(1) as u64;
            disk[d] += seg.num_of(Field::Disk, row).max(0) as u64 / links;
            files[d] += 1;
        }
        // Shifted by one: after this `v[i]` is everything *before* `i`.
        for v in [&mut *disk, &mut *files] {
            let mut run = 0u64;
            for slot in v.iter_mut() {
                let own = *slot;
                *slot = run;
                run += own;
            }
        }
        // The rows' totals wait for an order that reads them: building them
        // decodes a path a directory, and a page of folders needs none of it.
        self.by_row.clear();
        self.by_row.shrink_to_fit();
        self.scopes.clear();
        self.scopes.shrink_to_fit();
        self.rows_ready = false;

        self.deaths = deaths;
        self.rows_deaths = deaths;
    }

    /// What each directory row has under it, for an order by size: its scope
    /// and its total, the build's less what died since. Asked for, not kept.
    fn build_rows(&mut self, seg: &Segment<'_>) {
        let directory_rows = seg.dirs.len();
        // The rows that *are* directories. `dir_id` is the parent; a bounded
        // cache reuses decoded parents rather than retaining one String each.
        self.by_row.clear();
        self.by_row.reserve_exact(directory_rows);
        self.scopes.clear();
        self.scopes.reserve_exact(directory_rows);
        let mut parents = ParentPaths::default();
        let mut path = String::new();
        for row in 0..seg.rows() {
            if !seg.is_alive(row) || seg.num_of(Field::IsDir, row) == 0 {
                continue;
            }
            let Some(name) = seg.names.get(row) else {
                continue;
            };
            let parent = parents.get(&seg.dirs, seg.dir_id(row));
            path.clear();
            path.push_str(parent);
            if !parent.is_empty() && parent != "/" {
                path.push('/');
            }
            path.push_str(name);
            let scope = seg.dirs.subtree(&path);
            let bounds = [
                scope.own.unwrap_or(u32::MAX),
                scope.below.start,
                scope.below.end,
            ];
            self.by_row.push((row as u32, self.scope_total(bounds)));
            self.scopes.push(bounds);
        }

        self.rows_ready = true;
        self.rows_deaths = self.deaths;
    }

    /// Catch up with the rows that died since the last look. Cheap: a pass over
    /// the live bits, which is an eighth of a byte a row, and a sort of the few.
    fn refresh(&mut self, seg: &Segment<'_>, deaths: u64) {
        if deaths == self.deaths {
            return;
        }
        if seg.alive.len() != self.seen.len() {
            return self.rebuild(seg, deaths);
        }
        let n = self.disk.len().saturating_sub(1);
        let before = self.gone.len();
        // Taken out for the pass, which records deaths into `self` as it goes.
        let mut seen = std::mem::take(&mut self.seen);
        for (at, (was, now)) in seen
            .chunks_exact(8)
            .zip(seg.alive.chunks_exact(8))
            .enumerate()
        {
            let word = |b: &[u8]| u64::from_le_bytes(b.try_into().unwrap_or([0; 8]));
            let mut died = word(was) & !word(now);
            while died != 0 {
                let row = at * 64 + died.trailing_zeros() as usize;
                died &= died - 1;
                self.note_death(seg, row, n);
            }
        }
        // The bytes after the last whole word, one at a time.
        let tail = seen.len() / 8 * 8;
        for (at, (&was, &now)) in seen.iter().zip(seg.alive).enumerate().skip(tail) {
            let mut died = was & !now;
            while died != 0 {
                let row = at * 8 + died.trailing_zeros() as usize;
                died &= died - 1;
                self.note_death(seg, row, n);
            }
        }
        seen.copy_from_slice(seg.alive);
        self.seen = seen;
        // Past this the list costs more to consult than a build would.
        if self.gone.len() > (seg.rows() / 64).max(1_024) {
            return self.rebuild(seg, deaths);
        }
        if self.gone.len() != before {
            self.gone.sort_unstable_by_key(|&(dir, _)| dir);
            self.gone_disk.clear();
            self.gone_disk.push(0);
            let mut run = 0u64;
            for &(_, disk) in &self.gone {
                run += disk;
                self.gone_disk.push(run);
            }
        }
        self.deaths = deaths;
    }

    /// A dead row's share, when it had one: files only, as the build counts.
    fn note_death(&mut self, seg: &Segment<'_>, row: usize, n: usize) {
        if row >= seg.rows() || seg.num_of(Field::IsDir, row) != 0 {
            return;
        }
        let d = seg.dir_id(row);
        if d as usize >= n {
            return;
        }
        let links = seg.num_of(Field::Links, row).max(1) as u64;
        self.gone
            .push((d, seg.num_of(Field::Disk, row).max(0) as u64 / links));
    }

    /// What died since the build within directories `a..b`: bytes and files.
    fn gone_in(&self, a: u32, b: u32) -> (u64, u64) {
        let i = self.gone.partition_point(|&(dir, _)| dir < a);
        let j = self.gone.partition_point(|&(dir, _)| dir < b);
        (self.gone_disk[j] - self.gone_disk[i], (j - i) as u64)
    }

    /// The total a directory row's scope holds now: the build's, less the dead.
    fn scope_total(&self, [own, start, end]: [u32; 3]) -> i64 {
        let mut total = 0u64;
        let mut add = |a: u32, b: u32| {
            if let (Some(x), Some(y)) = (self.disk.get(a as usize), self.disk.get(b as usize)) {
                total += y.saturating_sub(*x);
            }
            total = total.saturating_sub(self.gone_in(a, b).0);
        };
        if own != u32::MAX {
            add(own, own + 1);
        }
        add(start, end);
        total as i64
    }

    /// Bring the rows' totals up to the last look, for an order that reads them.
    fn refresh_rows(&mut self) {
        if !self.rows_ready || self.rows_deaths == self.deaths {
            return;
        }
        for i in 0..self.by_row.len() {
            self.by_row[i].1 = self.scope_total(self.scopes[i]);
        }
        self.rows_deaths = self.deaths;
    }

    /// This segment's share of one subtree: bytes on disk, and files.
    fn subtree(&self, seg: &Segment<'_>, path: &str) -> (u64, u64) {
        let scope = seg.dirs.subtree(path);
        let mut disk = 0;
        let mut files = 0;
        let mut add = |from: usize, to: usize| {
            if let (Some(a), Some(b)) = (self.disk.get(from), self.disk.get(to)) {
                disk += b.saturating_sub(*a);
            }
            if let (Some(a), Some(b)) = (self.files.get(from), self.files.get(to)) {
                files += b.saturating_sub(*a);
            }
            let (gone_disk, gone_files) = self.gone_in(from as u32, to as u32);
            disk = disk.saturating_sub(gone_disk);
            files = files.saturating_sub(gone_files);
        };
        // The directory's own row is not adjacent to its descendants: a run of
        // one, holding whatever sits directly in the folder.
        if let Some(own) = scope.own {
            add(own as usize, own as usize + 1);
        }
        add(scope.below.start as usize, scope.below.end as usize);
        (disk, files)
    }
}

/// The prefix sums for every segment, kept until the segment changes.
#[derive(Debug, Default)]
pub struct Cache {
    /// By segment number, which is stable: a segment is never renumbered.
    per_segment: HashMap<u64, Prefix>,
}

impl Cache {
    /// Bring the cache up to date, then answer for each path: bytes on disk and
    /// file count. One call, because a page asks about thirty folders at once.
    pub fn subtrees(&mut self, segments: &[Live], paths: &[String]) -> Vec<(u64, u64)> {
        // Retire folded-away segments before allocating their replacements.
        self.per_segment
            .retain(|number, _| segments.iter().any(|live| live.number == *number));
        let mut out = vec![(0u64, 0u64); paths.len()];
        for live in segments {
            let Ok(seg) = live.view() else { continue };
            let deaths = live.deaths();
            let entry = self.per_segment.entry(live.number);
            let prefix = match entry {
                std::collections::hash_map::Entry::Occupied(mut o) => {
                    o.get_mut().refresh(&seg, deaths);
                    o.into_mut()
                }
                std::collections::hash_map::Entry::Vacant(v) => v.insert(Prefix::of(&seg, deaths)),
            };
            for (i, path) in paths.iter().enumerate() {
                let (disk, files) = prefix.subtree(&seg, path);
                out[i].0 += disk;
                out[i].1 += files;
            }
        }
        out
    }

    /// Bring every table already built up to date for an order that reads the
    /// rows' totals. Never builds one: a cold table sorts by the column.
    pub fn fresh_rows(&mut self, segments: &[Live]) {
        for live in segments {
            let Some(prefix) = self.per_segment.get_mut(&live.number) else {
                continue;
            };
            let Ok(seg) = live.view() else { continue };
            prefix.refresh(&seg, live.deaths());
            if prefix.rows_ready {
                prefix.refresh_rows();
            } else {
                prefix.build_rows(&seg);
            }
        }
    }

    /// What each directory row has under it — the table the sort reads. `None`
    /// until something asks for a folder size; the sort then falls back to the
    /// stored column.
    pub fn rows_of(&self, segment: u64) -> Option<&[(u32, i64)]> {
        self.per_segment.get(&segment).map(|p| &p.by_row[..])
    }

    /// What the cache is holding, for the memory line in `stats`.
    pub fn bytes(&self) -> u64 {
        self.per_segment
            .values()
            .map(|p| {
                ((p.disk.capacity() + p.files.capacity() + p.gone_disk.capacity())
                    * std::mem::size_of::<u64>()
                    + p.by_row.capacity() * std::mem::size_of::<(u32, i64)>()
                    + p.scopes.capacity() * std::mem::size_of::<[u32; 3]>()
                    + p.gone.capacity() * std::mem::size_of::<(u32, u64)>()
                    + p.seen.capacity()) as u64
            })
            .sum()
    }
}

/// Small decode working set; collisions replace a slot without changing lookup.
struct ParentPaths {
    slots: Vec<(u32, String)>,
}

impl Default for ParentPaths {
    fn default() -> Self {
        Self {
            slots: vec![(u32::MAX, String::new()); 512],
        }
    }
}

impl ParentPaths {
    fn get(&mut self, dirs: &crate::dirs::DirTable<'_>, id: u32) -> &str {
        let at = id as usize % self.slots.len();
        let (held, path) = &mut self.slots[at];
        if *held != id {
            if dirs.get_into(id, path).is_none() {
                path.clear();
            }
            *held = id;
        }
        path
    }
}
