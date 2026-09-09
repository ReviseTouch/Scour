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
#[derive(Debug, Default)]
pub struct Prefix {
    disk: Vec<u64>,
    files: Vec<u64>,
    /// What each *directory row* has under it, so a folder shown as `~13 GB`
    /// sorts as 13 GB rather than by its `Size` column, which is four kilobytes
    /// of entry table. In row order, so a lookup is a binary search.
    by_row: Vec<(u32, i64)>,
    /// The segment's death count when this was built; anything else is stale.
    deaths: u64,
}

impl Prefix {
    fn of(seg: &Segment<'_>, deaths: u64) -> Prefix {
        let mut prefix = Prefix::default();
        prefix.rebuild(seg, deaths);
        prefix
    }

    fn rebuild(&mut self, seg: &Segment<'_>, deaths: u64) {
        let n = seg.dirs.len();
        // Refreshed in the existing buffers: a replacement would double the peak.
        self.disk.resize(n + 1, 0);
        self.files.resize(n + 1, 0);
        self.disk.fill(0);
        self.files.fill(0);
        let (disk, files) = (&mut self.disk, &mut self.files);
        let mut directory_rows = 0;
        for row in 0..seg.rows() {
            // A directory's `st_size` is its entry table, not content, so its
            // own row is skipped. A row's share of a hard link is `disk/links`.
            if !seg.is_alive(row) {
                continue;
            }
            if seg.num_of(Field::IsDir, row) != 0 {
                directory_rows += 1;
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
        // The rows that *are* directories. `dir_id` is the parent; a bounded
        // cache reuses decoded parents rather than retaining one String each.
        self.by_row.clear();
        self.by_row.reserve_exact(directory_rows);
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
            let mut total = 0i64;
            if let Some(own) = scope.own {
                let i = own as usize;
                total += disk[i + 1].saturating_sub(disk[i]) as i64;
            }
            let (a, b) = (scope.below.start as usize, scope.below.end as usize);
            if let (Some(from), Some(to)) = (disk.get(a), disk.get(b)) {
                total += to.saturating_sub(*from) as i64;
            }
            self.by_row.push((row as u32, total));
        }

        self.deaths = deaths;
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
                    if o.get().deaths != deaths {
                        o.get_mut().rebuild(&seg, deaths);
                    }
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
                ((p.disk.capacity() + p.files.capacity()) * std::mem::size_of::<u64>()
                    + p.by_row.capacity() * std::mem::size_of::<(u32, i64)>())
                    as u64
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
