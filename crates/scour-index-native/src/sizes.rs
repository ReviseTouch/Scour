//! What a folder weighs, in O(1) per folder.
//!
//! Directory numbers are handed out in sorted path order, so a subtree is one
//! or two contiguous runs ([`DirTable::subtree`]) and a prefix sum over them is
//! a subtraction — 90 ms to build and 12.1 µs a folder over 1.97 M rows. Only
//! what this index holds is counted, so it reads smaller than `du`.

use std::collections::HashMap;

use scour_core::SourceId;

use crate::columns::Field;
use crate::ids::IdMap;
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
    /// Counts, which no segment's rows can take past `u32`: half the bytes of
    /// the `u64` it was, at 653,000 directories 2.6 MB.
    files: Vec<u32>,
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
        let mut run = 0u64;
        for slot in disk.iter_mut() {
            let own = *slot;
            *slot = run;
            run += own;
        }
        let mut run = 0u32;
        for slot in files.iter_mut() {
            let own = *slot;
            *slot = run;
            run += own;
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
    ///
    /// **Without the names.** A directory row's scope was its path's, built
    /// from its name and looked up in the table: 941,000 names read from all
    /// over the name arena and as many bisections of the table, 0.97 s and
    /// 230 MB of the index brought in, the first time a window opened by
    /// size. The table is read once in order instead, which gives every
    /// directory its descendants, and the segment's path digests give every
    /// table directory its own row. A row the digests cannot place reads its
    /// name as before, when anything could be under it: an ambiguous digest,
    /// or a parent with a descendant whose own directory the table skips.
    fn build_rows(&mut self, seg: &Segment<'_>, ids: Option<&IdMap<'_>>) {
        self.by_row.clear();
        self.scopes.clear();
        let mut rows: Vec<u32> = (0..seg.rows())
            .filter(|&row| seg.is_alive(row) && seg.num_of(Field::IsDir, row) != 0)
            .map(|row| row as u32)
            .collect();
        rows.shrink_to_fit();
        self.by_row.reserve_exact(rows.len());
        self.scopes.reserve_exact(rows.len());
        let placed = ids.and_then(|ids| place(seg, ids, &rows));
        let mut parents = ParentPaths::default();
        let mut path = String::new();
        for (i, &row) in rows.iter().enumerate() {
            let bounds = match placed.as_ref().map(|p| p.bounds[i]) {
                Some(Some(bounds)) => bounds,
                // Read by name: nothing placed it, and something may be below.
                _ => {
                    let Some(name) = seg.names.get(row as usize) else {
                        continue;
                    };
                    let parent = parents.get(&seg.dirs, seg.dir_id(row as usize));
                    path.clear();
                    path.push_str(parent);
                    if !parent.is_empty() && parent != "/" {
                        path.push('/');
                    }
                    path.push_str(&name);
                    let scope = seg.dirs.subtree(&path);
                    [
                        scope.own.unwrap_or(u32::MAX),
                        scope.below.start,
                        scope.below.end,
                    ]
                }
            };
            self.by_row.push((row, self.scope_total(bounds)));
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
            .as_chunks::<8>()
            .0
            .iter()
            .zip(seg.alive.as_chunks::<8>().0)
            .enumerate()
        {
            let mut died = u64::from_le_bytes(*was) & !u64::from_le_bytes(*now);
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
                files += u64::from(b.saturating_sub(*a));
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
                prefix.build_rows(&seg, live.ids().ok().as_ref());
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
                ((p.disk.capacity() + p.gone_disk.capacity()) * std::mem::size_of::<u64>()
                    + p.files.capacity() * std::mem::size_of::<u32>()
                    + p.by_row.capacity() * std::mem::size_of::<(u32, i64)>()
                    + p.scopes.capacity() * std::mem::size_of::<[u32; 3]>()
                    + p.gone.capacity() * std::mem::size_of::<(u32, u64)>()
                    + p.seen.capacity()) as u64
            })
            .sum()
    }
}

/// Where each directory row's subtree lies, found without its name: `None` at
/// a row that has to be read by name. `bounds[i]` is for `rows[i]`.
struct Placed {
    bounds: Vec<Option<[u32; 3]>>,
}

/// Nothing below, and no directory of its own: a scope that totals zero.
const EMPTY: [u32; 3] = [u32::MAX, 0, 0];
const NONE: u32 = u32::MAX;

/// See [`Prefix::build_rows`]. `rows` are the live directory rows, ascending.
/// `None` when the table cannot be read through.
fn place(seg: &Segment<'_>, ids: &IdMap<'_>, rows: &[u32]) -> Option<Placed> {
    let n = seg.dirs.len();
    // Every table directory's nearest ancestor *in the table*, and whether that
    // is its parent. Paths come sorted, and a path's ancestors are prefixes of
    // it ending before a `/`; a prefix ending before `-`, which sorts before
    // `/`, is a sibling's, and stays open for the descendants still to come.
    let mut up = vec![NONE; n];
    let mut direct = vec![false; n];
    let mut stack: Vec<(u32, usize)> = Vec::new();
    let mut prev: Vec<u8> = Vec::new();
    let mut root = NONE;
    // The directory of rows with no directory, whose children's paths have no
    // `/` and so no ancestors to find: read by name.
    let mut nowhere = NONE;
    let mut read = 0usize;
    seg.dirs.each_bytes(|id, path| {
        read += 1;
        let common = prev.iter().zip(path).take_while(|(a, b)| a == b).count();
        while let Some(&(_, len)) = stack.last() {
            let stays = len <= common
                && path.len() > len
                && (path[len] <= b'/' || (len == 1 && path[0] == b'/'));
            if stays {
                break;
            }
            stack.pop();
        }
        let ancestor = stack
            .iter()
            .rev()
            .find(|&&(_, len)| path[len] == b'/' || (len == 1 && path[0] == b'/'));
        if let Some(&(at, len)) = ancestor {
            up[id] = at;
            // The parent is the path up to its last `/`, or `/` for a child of
            // the root.
            let last = path.iter().rposition(|&b| b == b'/').unwrap_or(0);
            direct[id] = last == len || (last == 0 && len == 1);
        }
        if path == b"/" {
            root = id as u32;
        }
        if path.is_empty() {
            nowhere = id as u32;
        }
        if !path.is_empty() {
            stack.push((id as u32, path.len()));
        }
        prev.clear();
        prev.extend_from_slice(path);
    });
    if read != n {
        return None;
    }

    // Descendants are contiguous, and every one is below its nearest ancestor:
    // a pass from the deepest numbers up gathers each directory's run.
    let mut lo = vec![NONE; n];
    let mut hi = vec![0u32; n];
    // A parent one of whose descendants has no directory of its own here, so a
    // row the table does not hold may still have something below it.
    let mut gapped = vec![false; n];
    for id in (0..n).rev() {
        let p = up[id];
        if p == NONE {
            continue;
        }
        let p = p as usize;
        lo[p] = lo[p].min(lo[id]).min(id as u32);
        hi[p] = hi[p].max(hi[id]).max(id as u32 + 1);
        if !direct[id] {
            gapped[p] = true;
        }
    }

    // Each table directory's own row, by its path's digest, which names a
    // source too. A directory's row is of the source its parent's other
    // directory rows are, so one digest a directory, except under a parent
    // two sources share — `/`, `/home` — where it is one a source.
    const MIXED: u32 = u32::MAX - 1;
    let mut source_in = vec![NONE; n];
    let mut sources: Vec<u32> = Vec::new();
    for &row in rows {
        let source = seg.source_of(row as usize).0;
        if !sources.contains(&source) {
            sources.push(source);
        }
        if let Some(slot) = source_in.get_mut(seg.dir_id(row as usize) as usize) {
            *slot = match *slot {
                NONE => source,
                held if held == source => held,
                _ => MIXED,
            };
        }
    }
    let mut keys: Vec<(u32, u32, u32)> = Vec::new();
    seg.dirs.each_bytes(|id, path| {
        if !direct[id] || id as u32 == root {
            return;
        }
        let Ok(path) = std::str::from_utf8(path) else {
            return;
        };
        let mut key = |source: u32| {
            keys.push((IdMap::key_of(SourceId(source), path), id as u32, source));
        };
        match source_in[up[id] as usize] {
            // No directory row sits in its parent, so neither does its own.
            NONE => {}
            MIXED => sources.iter().for_each(|&s| key(s)),
            source => key(source),
        }
    });
    keys.sort_unstable();
    let mut own: Vec<u32> = vec![NONE; rows.len()];
    let mut doubtful = vec![false; rows.len()];
    ids.join(
        keys.len(),
        |j| keys[j].0,
        |row, j| {
            let (_, dir, source) = keys[j];
            let Ok(at) = rows.binary_search(&row) else {
                return;
            };
            // A digest is half a path's: the row has to be a directory of
            // this source sitting in this directory's parent.
            if seg.source_of(row as usize).0 != source
                || seg.dir_id(row as usize) != up[dir as usize]
            {
                return;
            }
            match own[at] {
                NONE => own[at] = dir,
                held if held != dir => doubtful[at] = true,
                _ => {}
            }
        },
    );

    let bounds = rows
        .iter()
        .enumerate()
        .map(|(i, &row)| {
            if doubtful[i] || seg.dir_id(row as usize) == nowhere {
                return None;
            }
            match own[i] {
                NONE => {
                    let parent = seg.dir_id(row as usize) as usize;
                    // Nothing below it in the table, unless its parent has
                    // a descendant whose own directory is missing.
                    (!gapped.get(parent).copied().unwrap_or(true)).then_some(EMPTY)
                }
                dir => {
                    let d = dir as usize;
                    let below = if hi[d] > 0 { [lo[d], hi[d]] } else { [0, 0] };
                    Some([dir, below[0], below[1]])
                }
            }
        })
        .collect();
    Some(Placed { bounds })
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

#[cfg(test)]
mod tests {
    use super::*;
    use scour_core::Index;

    /// The table built without names is the table built with them: every
    /// directory row's total, over directories that sort between a folder and
    /// its children, empty ones, one whose own row the table skips, two
    /// sources and a segment with dead rows.
    #[test]
    fn folder_totals_without_names_are_the_totals_with_them() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let index = crate::NativeIndex::open_or_create(tmp.path()).expect("create");
        let entry = |source: u32, path: &str, size: i64, dir: bool| scour_core::Entry {
            id: scour_core::EntryId::path_hash(SourceId(source), path),
            path: path.to_owned(),
            is_dir: dir,
            meta: scour_core::Meta {
                mtime: 1_000 + size,
                size,
                disk: size * 2,
                ..scour_core::Meta::UNKNOWN
            },
        };
        let mut first = Vec::new();
        for (source, top) in [(0u32, "/h"), (1, "/m")] {
            for d in [
                "", "/a", "/a-b", "/a-b/c", "/a/x", "/a.old", "/a/x/y", "/empty", "/e-1",
            ] {
                first.push(entry(source, &format!("{top}{d}"), 4_096, true));
            }
            for (i, f) in [
                "/a/f", "/a-b/f", "/a-b/c/f", "/a/x/f", "/a/x/y/f", "/a.old/f", "/f",
            ]
            .iter()
            .enumerate()
            {
                first.push(entry(
                    source,
                    &format!("{top}{f}"),
                    10 * (i as i64 + 1),
                    false,
                ));
            }
            // A file three levels down whose middle directory has no row:
            // its parent holds a descendant without a directory of its own.
            first.push(entry(source, &format!("{top}/g"), 4_096, true));
            first.push(entry(source, &format!("{top}/g/h/i"), 4_096, true));
            first.push(entry(source, &format!("{top}/g/h/i/f"), 77, false));
        }
        first.push(entry(0, "/usr", 4_096, true));
        first.push(entry(0, "/usr/f", 5, false));
        index
            .apply(&mut first.into_iter().map(scour_core::Change::Upsert))
            .expect("apply");
        index.commit().expect("commit");
        let second: Vec<_> = (0..40)
            .map(|i| entry(0, &format!("/h/a/x/n{i}"), i, i % 3 == 0))
            .collect();
        index
            .apply(&mut second.into_iter().map(scour_core::Change::Upsert))
            .expect("apply");
        index.commit().expect("commit");
        index
            .apply(
                &mut [scour_core::Change::RemoveSubtree {
                    path: "/h/a-b/c".into(),
                }]
                .into_iter(),
            )
            .expect("remove");
        index.commit().expect("commit");

        index.with_segments(|segments| {
            assert!(segments.len() >= 2, "{} segments", segments.len());
            for live in segments {
                let seg = live.view().expect("view");
                let ids = live.ids().expect("ids");
                let mut by_name = Prefix::of(&seg, live.deaths());
                by_name.build_rows(&seg, None);
                let mut without = Prefix::of(&seg, live.deaths());
                without.build_rows(&seg, Some(&ids));
                assert!(!by_name.by_row.is_empty());
                // Read by name: only the rows under a folder with a gap below it —
                // `g`, `empty` and `e-1`, in each source.
                assert_eq!(without.by_row, by_name.by_row, "segment {}", live.number);
                let placed = place(
                    &seg,
                    &ids,
                    &without.by_row.iter().map(|r| r.0).collect::<Vec<_>>(),
                )
                .expect("placed");
                assert!(
                    placed.bounds.iter().filter(|b| b.is_none()).count() <= 6,
                    "rows read by name: {:?}",
                    placed.bounds
                );
            }
        });
    }
}
