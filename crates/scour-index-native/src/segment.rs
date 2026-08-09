//! A segment on disk.
//!
//! A segment is written once and never edited. That is not a simplification to
//! be undone later — it is what lets the files be mapped and read without a
//! lock, by any number of threads, while writing continues elsewhere.
//!
//! One thing does change: which rows are still live. That is a bit a row, it is
//! held in memory and written back beside the segment, and it is the only
//! mutable part of an index on disk. Deleting a million files touches 125 KB.

use std::path::{Path, PathBuf};

use crate::durable::{replace_synced, write_synced};
use memmap2::Mmap;
use scour_core::{Entry, Error, Result, SourceId};

use crate::build::SegmentBytes;
use crate::columns::ColumnBlocks;
use crate::dirs::DirTable;
use crate::ids::IdMap;
use crate::names::NameArena;
use crate::search::Segment;
use crate::trigram::TrigramIndex;

/// The pieces a segment is made of, and the extension each is stored under.
const PARTS: [&str; 7] = ["names", "cols", "dirs", "ids", "tgrams", "tpost", "fnames"];

fn part_path(dir: &Path, number: u64, ext: &str) -> PathBuf {
    dir.join(format!("seg-{number:08}.{ext}"))
}

/// One piece of a segment: mapped from a file, or held in memory.
///
/// **The second case is what lets a change be searchable before it is
/// durable.** Those were the same thing here, and the cost of conflating them
/// was measured: a search could not see a new file until a commit, and a commit
/// writes seven files and calls `fsync` about ten times — 22.5 ms whether it
/// carries one row or a hundred and twenty-eight. Freshness was therefore
/// bought in units of a whole segment write, and at a five-second clock that
/// was the single largest thing an idle service did.
///
/// Nothing downstream can tell the difference: every reader takes a `&[u8]`,
/// and both arms give it one.
#[derive(Debug)]
enum Part {
    Mapped(Mmap),
    Owned(Vec<u8>),
}

impl std::ops::Deref for Part {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Part::Mapped(m) => m,
            Part::Owned(v) => v,
        }
    }
}

/// An opened segment: six parts and one bitmap that is not.
#[derive(Debug)]
pub struct Live {
    pub number: u64,
    /// The reconciliation pass this segment was written during.
    ///
    /// Kept per segment rather than per row because [`crate::NativeIndex`]
    /// flushes when a generation begins, so a segment never spans two.
    pub generation: u64,
    maps: Vec<Part>,
    alive: Vec<u8>,
    rows: usize,
    /// How many of those rows are directories.
    ///
    /// Counted once, here, because the alternative was counting it on every
    /// call to `stats()` — which walks a column of every row, and which the
    /// engine came to call once a second to decide whether to compact. On a
    /// 2.1 M-entry index that was a whole core, every second, to answer a
    /// question about the *segment count*.
    ///
    /// The number is the total, not the live one: rows die after this is
    /// computed and `live_rows` is what says how many. A directory count that
    /// drifts by the number of deleted folders is a status line being slightly
    /// stale; recomputing it was a service being unusable.
    dirs: usize,
}

impl Live {
    /// Write a segment and open it.
    pub fn write(dir: &Path, number: u64, generation: u64, bytes: &SegmentBytes) -> Result<Live> {
        let blobs = [
            &bytes.names,
            &bytes.cols,
            &bytes.dirs,
            &bytes.ids,
            &bytes.tri_dict,
            &bytes.tri_post,
            &bytes.fnames,
        ];
        // Synced, not merely written: the manifest is about to name these
        // files, and a manifest that survives a crash while its segments do
        // not is an index that cannot be opened.
        for (ext, blob) in PARTS.iter().zip(blobs) {
            write_synced(&part_path(dir, number, ext), blob)?;
        }
        write_synced(&part_path(dir, number, "alive"), &bytes.alive)?;
        Live::open(dir, number, generation)
    }

    pub fn open(dir: &Path, number: u64, generation: u64) -> Result<Live> {
        let mut maps = Vec::with_capacity(PARTS.len());
        for ext in PARTS {
            let p = part_path(dir, number, ext);
            let f = std::fs::File::open(&p).map_err(|e| Error::io(&e, &p.to_string_lossy()))?;
            // Safe as long as nobody rewrites the file underneath us, which
            // nothing does: a segment is written once and then only deleted.
            let m = unsafe { Mmap::map(&f) }.map_err(|e| Error::io(&e, &p.to_string_lossy()))?;
            maps.push(Part::Mapped(m));
        }
        let p = part_path(dir, number, "alive");
        let alive = std::fs::read(&p).map_err(|e| Error::io(&e, &p.to_string_lossy()))?;
        Live::assemble(number, generation, maps, alive)
    }

    /// A segment that was never written, and may never be.
    ///
    /// The same bytes `write` would have put in seven files, kept in memory
    /// instead. It is a real segment to every reader — same format, same
    /// trigram filter, same zone maps — and the only thing it is not is
    /// durable. See [`Part`] for why that distinction had to be made.
    pub fn in_memory(number: u64, generation: u64, bytes: &SegmentBytes) -> Result<Live> {
        let maps = vec![
            Part::Owned(bytes.names.clone()),
            Part::Owned(bytes.cols.clone()),
            Part::Owned(bytes.dirs.clone()),
            Part::Owned(bytes.ids.clone()),
            Part::Owned(bytes.tri_dict.clone()),
            Part::Owned(bytes.tri_post.clone()),
            Part::Owned(bytes.fnames.clone()),
        ];
        Live::assemble(number, generation, maps, bytes.alive.clone())
    }

    /// Check the pieces agree with each other and count what a search needs.
    ///
    /// Shared by both ways in, so an in-memory segment is validated exactly as
    /// hard as one read off a disk. A bug that built a short bitmap would
    /// otherwise be caught in one path and silently answer "nothing matched" in
    /// the other.
    fn assemble(number: u64, generation: u64, maps: Vec<Part>, alive: Vec<u8>) -> Result<Live> {
        let rows = NameArena::open(&maps[0])
            .ok_or_else(|| Error::IndexCorrupt {
                detail: format!("seg-{number:08}.names is unreadable"),
            })?
            .rows();
        // **A short bitmap is not "these rows are dead".** It reopened as a
        // segment whose every row was gone — a truncated write, a full disk, a
        // half-copied index directory, all of them silently answering "nothing
        // matched" for as long as the segment lived. The bitmap is one bit a
        // row and is written whole; a length that does not say so is damage.
        let want = rows.div_ceil(8);
        if alive.len() != want {
            return Err(Error::IndexCorrupt {
                detail: format!(
                    "seg-{number:08}.alive is {} bytes for {rows} rows, expected {want}",
                    alive.len()
                ),
            });
        }
        let dirs = ColumnBlocks::open(&maps[1])
            .map(|cols| {
                (0..rows)
                    .filter(|&r| cols.get(crate::columns::Field::IsDir, r).unwrap_or(0) != 0)
                    .count()
            })
            .unwrap_or(0);
        Ok(Live {
            number,
            generation,
            maps,
            alive,
            rows,
            dirs,
        })
    }

    /// Erase a segment's files. Called once nothing refers to it.
    pub fn erase(dir: &Path, number: u64) {
        for ext in PARTS.iter().chain(std::iter::once(&"alive")) {
            // A missing file is the desired state, so a failure to remove one
            // that is already gone is not worth reporting.
            let _ = std::fs::remove_file(part_path(dir, number, ext));
        }
    }

    /// Does this row already say exactly what an entry says?
    ///
    /// **Opens the columns and nothing else.** The obvious way to ask is
    /// through [`Live::view`], and the obvious way is two million times slower
    /// on a rescan: a view builds the name arena, the directory table and the
    /// trigram index as well, and none of them is consulted here. Measured
    /// against the whole point of asking — a rescan of an untouched two
    /// million rows went from 3.04 s to 6.65 s with the view, which is worse
    /// than writing the rows it was trying not to write.
    ///
    /// `atime` is not compared. It moves when a file is *read*, so including
    /// it would call almost everything changed and the answer would always be
    /// no.
    pub fn row_matches(&self, row: usize, meta: &scour_core::Meta, is_dir: bool) -> bool {
        use crate::columns::Field;
        let Some(cols) = ColumnBlocks::open(&self.maps[1]) else {
            return false;
        };
        let n = |f: Field| cols.get(f, row).unwrap_or(0);
        n(Field::Size) == meta.size
            && n(Field::Mtime) == meta.mtime
            && n(Field::Ctime) == meta.ctime
            && n(Field::Mode) == meta.mode
            && n(Field::Uid) == meta.uid
            && n(Field::Gid) == meta.gid
            && (n(Field::IsDir) != 0) == is_dir
    }

    /// The searchable view.
    pub fn view(&self) -> Result<Segment<'_>> {
        let corrupt = |what: &str| Error::IndexCorrupt {
            detail: format!("seg-{:08}.{what} is unreadable", self.number),
        };
        Ok(Segment {
            names: NameArena::open(&self.maps[0]).ok_or_else(|| corrupt("names"))?,
            cols: ColumnBlocks::open(&self.maps[1]).ok_or_else(|| corrupt("cols"))?,
            dirs: DirTable::open(&self.maps[2]).ok_or_else(|| corrupt("dirs"))?,
            tri: TrigramIndex::open(&self.maps[4], &self.maps[5])
                .ok_or_else(|| corrupt("tgrams"))?,
            folded: NameArena::open(&self.maps[6]).ok_or_else(|| corrupt("fnames"))?,
            alive: &self.alive,
        })
    }

    pub fn ids(&self) -> Result<IdMap<'_>> {
        IdMap::open(&self.maps[3]).ok_or_else(|| Error::IndexCorrupt {
            detail: format!("seg-{:08}.ids is unreadable", self.number),
        })
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    /// Directories among them, counted when the segment was opened.
    pub fn dirs(&self) -> usize {
        self.dirs
    }

    pub fn live_rows(&self) -> u64 {
        self.alive.iter().map(|b| b.count_ones() as u64).sum()
    }

    pub fn dead_rows(&self) -> u64 {
        self.rows as u64 - self.live_rows()
    }

    pub fn is_alive(&self, row: usize) -> bool {
        match self.alive.get(row / 8) {
            Some(b) => b & (1 << (row % 8)) != 0,
            None => false,
        }
    }

    /// Clear a row's live bit. Returns whether it was live before.
    pub fn kill(&mut self, row: usize) -> bool {
        if row >= self.rows {
            return false;
        }
        let was = self.is_alive(row);
        self.alive[row / 8] &= !(1 << (row % 8));
        was
    }

    /// The row holding this path, if the segment has it and it is live.
    ///
    /// The candidate list from the id table is confirmed against the directory
    /// and name the row actually carries, so a digest collision costs one extra
    /// comparison and cannot produce a wrong answer.
    pub fn find(&self, source: SourceId, path: &str) -> Result<Option<usize>> {
        // The identity table first, and the segment view only if it says there
        // is something to confirm.
        //
        // A commit probes every entry it writes against every existing segment,
        // and almost every one of those probes finds nothing — so what the
        // probe costs *when it finds nothing* is the whole cost. Opening the
        // view parses four headers; doing that before the binary search made
        // indexing ten million entries quadratic in the segment count.
        let mut dirs = std::collections::HashMap::new();
        let rows: Vec<usize> = self
            .ids()?
            .candidates(source, path)
            .map(|r| r as usize)
            .filter(|&r| self.is_alive(r))
            .collect();
        if rows.is_empty() {
            return Ok(None);
        }
        let seg = self.view()?;
        Ok(rows
            .into_iter()
            .find(|&row| seg.is_at(&mut dirs, row, source, path)))
    }

    /// Kill the rows holding any of these paths. Returns how many died.
    ///
    /// `wanted` must be sorted by [`IdMap::key_of`].
    ///
    /// **Two strategies, chosen by size, and the second one was missing.** A
    /// merge walks both sides once, which is right when a bulk pass hands over
    /// a hundred thousand identities: doing that one at a time is quadratic in
    /// the segment count and was measured at a hundred seconds to index ten
    /// million entries, almost all of it in probes that found nothing.
    ///
    /// But a merge is `O(rows in the segment)` *however few* identities are
    /// wanted, because it advances through the table until it passes the last
    /// of them. A watcher commit carries three. Measured on the live index:
    /// **a commit with 3 to 13 staged entries held the write lock for 44 to
    /// 74 ms**, once a second, with every search queued behind it — to check
    /// three identities against 2.1 M rows.
    ///
    /// So below the crossover it is a binary search per identity, which is
    /// what the table is sorted for. The crossover is where `wanted × log
    /// rows` stops being cheaper than `rows`, and `log2` of a two-million-row
    /// table is about 21.
    pub fn kill_paths(&mut self, wanted: &[(u32, SourceId, &str)]) -> Result<u64> {
        if wanted.is_empty() || self.rows == 0 {
            return Ok(0);
        }
        let victims: Vec<usize> = {
            let ids = self.ids()?;
            let mut pairs: Vec<(usize, usize)> = Vec::new();
            // The small case: probe for each path rather than sweep the
            // table. `ids.len().ilog2()` is the cost of one probe, so this is
            // the point where probing everything stops being cheaper than
            // walking everything.
            if !ids.is_empty()
                && wanted
                    .len()
                    .saturating_mul(ids.len().ilog2().max(1) as usize)
                    < ids.len()
            {
                for (w, (key, _, _)) in wanted.iter().enumerate() {
                    for row in ids.rows_for(*key) {
                        pairs.push((row as usize, w));
                    }
                }
                let seg = if pairs.is_empty() {
                    None
                } else {
                    Some(self.view()?)
                };
                let victims: Vec<usize> = match seg {
                    None => Vec::new(),
                    Some(seg) => {
                        let mut dirs = std::collections::HashMap::new();
                        pairs
                            .into_iter()
                            .filter(|&(row, w)| {
                                self.is_alive(row)
                                    && seg.is_at(&mut dirs, row, wanted[w].1, wanted[w].2)
                            })
                            .map(|(row, _)| row)
                            .collect()
                    }
                };
                let mut gone = 0;
                for row in victims {
                    if self.kill(row) {
                        gone += 1;
                    }
                }
                return Ok(gone);
            }
            let (mut i, mut j) = (0usize, 0usize);
            while i < ids.len() && j < wanted.len() {
                let (h, row) = ids.at(i);
                match h.cmp(&wanted[j].0) {
                    std::cmp::Ordering::Less => i += 1,
                    std::cmp::Ordering::Greater => j += 1,
                    std::cmp::Ordering::Equal => {
                        // A run of equal hashes on each side. Both are tiny —
                        // a collision in a 32-bit key is rare and a repeated
                        // identity is a bug — so the cross product is cheap.
                        let mut i2 = i;
                        while i2 < ids.len() && ids.at(i2).0 == h {
                            i2 += 1;
                        }
                        let mut j2 = j;
                        while j2 < wanted.len() && wanted[j2].0 == h {
                            j2 += 1;
                        }
                        for k in i..i2 {
                            for w in j..j2 {
                                pairs.push((ids.at(k).1 as usize, w));
                            }
                        }
                        let _ = row;
                        i = i2;
                        j = j2;
                    }
                }
            }
            if pairs.is_empty() {
                Vec::new()
            } else {
                // Only now is the segment opened, and only to confirm that the
                // half-digest was not a collision.
                let seg = self.view()?;
                let mut dirs = std::collections::HashMap::new();
                pairs
                    .into_iter()
                    .filter(|&(row, w)| {
                        self.is_alive(row) && seg.is_at(&mut dirs, row, wanted[w].1, wanted[w].2)
                    })
                    .map(|(row, _)| row)
                    .collect()
            }
        };
        let mut gone = 0;
        for row in victims {
            if self.kill(row) {
                gone += 1;
            }
        }
        Ok(gone)
    }

    /// Every live entry, in stored order. Used by a merge.
    pub fn entries(&self) -> Result<impl Iterator<Item = Entry> + '_> {
        let seg = self.view()?;
        Ok((0..self.rows).filter_map(move |row| {
            if self.is_alive(row) {
                seg.entry(row)
            } else {
                None
            }
        }))
    }

    /// A copy of the live bits, to be written once the lock is released.
    ///
    /// 262 KB at two million rows, against an `fsync` — measured at **13 ms a
    /// segment**, three or four segments a commit, once a second, with every
    /// search waiting. Copying is the cheap half of that by two orders of
    /// magnitude.
    pub fn alive_snapshot(&self) -> (u64, Vec<u8>) {
        (self.number, self.alive.clone())
    }

    /// Write bits taken by [`Live::alive_snapshot`], with no lock held.
    ///
    /// A crash between the snapshot and this leaves the older bitmap on disk,
    /// which is what a crash before the write always did: the removed rows
    /// come back until the next sweep takes them. Nothing new is risked by
    /// moving it out.
    pub fn write_alive(dir: &Path, number: u64, bits: &[u8]) -> Result<()> {
        replace_synced(&part_path(dir, number, "alive"), bits)
    }

    pub fn save_alive(&self, dir: &Path) -> Result<()> {
        // Replaced rather than overwritten: this file is read whole at open
        // time, and a half-written one turns every row in the segment into a
        // coin flip between alive and dead.
        replace_synced(&part_path(dir, self.number, "alive"), &self.alive)
    }
}

#[cfg(test)]
mod memory_segment {
    use super::*;
    use scour_core::{EntryId, Meta, SourceId};

    fn entry(path: &str) -> Entry {
        Entry {
            id: EntryId::path_hash(SourceId(0), path),
            path: path.into(),
            is_dir: false,
            meta: Meta::UNKNOWN,
        }
    }

    /// A segment held in memory has to be the same segment, not a near one.
    ///
    /// Written and unwritten are the same bytes by construction, so the thing
    /// worth asserting is that both arrive at the same *reader* — same row
    /// count, same names, same directory table. If they ever diverge, a search
    /// would answer differently depending on whether a commit had happened,
    /// which is exactly the bug this split exists to remove.
    #[test]
    fn an_unwritten_segment_reads_the_same_as_a_written_one() {
        let rows = [
            entry("/a/one.txt"),
            entry("/a/two.txt"),
            entry("/b/three.txt"),
        ];
        let bytes = crate::build::build(&rows);

        let dir = tempfile::tempdir().expect("tempdir");
        let on_disk = Live::write(dir.path(), 1, 0, &bytes).expect("write");
        let in_ram = Live::in_memory(1, 0, &bytes).expect("in_memory");

        assert_eq!(in_ram.rows(), on_disk.rows());
        assert_eq!(in_ram.live_rows(), on_disk.live_rows());

        let a = on_disk.view().expect("view");
        let b = in_ram.view().expect("view");
        let mut seen = Vec::new();
        for r in 0..on_disk.rows() {
            assert_eq!(b.dir_id(r), a.dir_id(r), "row {r}");
            seen.push(b.path(r, "x"));
            assert_eq!(b.path(r, "x"), a.path(r, "x"), "row {r}");
        }
        seen.sort();
        assert_eq!(
            seen,
            ["/a/x", "/a/x", "/b/x"],
            "the directory table came through"
        );
    }

    /// The validation is shared, so damage is caught on both paths.
    #[test]
    fn an_unwritten_segment_with_a_short_bitmap_is_refused() {
        let rows = [entry("/a/one.txt"), entry("/a/two.txt")];
        let mut bytes = crate::build::build(&rows);
        bytes.alive.clear();
        assert!(
            Live::in_memory(2, 0, &bytes).is_err(),
            "a bitmap that does not cover the rows is damage, not a dead segment"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::build;
    use scour_core::{EntryId, Meta};

    fn entry(path: &str, mtime: i64, _ino: u64) -> Entry {
        Entry {
            id: EntryId::path_hash(SourceId(0), path),
            path: path.into(),
            is_dir: false,
            meta: Meta {
                mtime,
                size: 10,
                ..Meta::UNKNOWN
            },
        }
    }

    #[test]
    fn a_segment_survives_being_written_and_reopened() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let entries = vec![
            entry("/a/one.rs", 300, 1),
            entry("/a/two.rs", 200, 2),
            entry("/b/three.rs", 100, 3),
        ];
        let seg = Live::write(tmp.path(), 1, 7, &build(&entries)).expect("write");
        assert_eq!(seg.rows(), 3);
        assert_eq!(seg.generation, 7);
        drop(seg);

        let seg = Live::open(tmp.path(), 1, 7).expect("reopen");
        assert_eq!(
            seg.view().expect("view").entry(0).expect("row").path,
            "/a/one.rs"
        );
        assert_eq!(seg.live_rows(), 3);
    }

    #[test]
    fn an_entry_is_found_by_its_path_and_not_by_luck() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let entries: Vec<Entry> = (0..500)
            .map(|i| entry(&format!("/a/f{i}.rs"), 1000 - i, i as u64))
            .collect();
        let seg = Live::write(tmp.path(), 1, 0, &build(&entries)).expect("write");
        for e in &entries {
            let row = seg
                .find(SourceId(0), &e.path)
                .expect("find")
                .expect("present");
            assert_eq!(
                seg.view().expect("view").entry(row).expect("row").path,
                e.path
            );
        }
        assert_eq!(seg.find(SourceId(0), "/a/yok.rs").expect("find"), None);
    }

    #[test]
    fn a_killed_row_stops_being_found_and_the_bit_persists() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let entries = vec![entry("/a/one.rs", 300, 1), entry("/a/two.rs", 200, 2)];
        let mut seg = Live::write(tmp.path(), 1, 0, &build(&entries)).expect("write");
        let row = seg
            .find(SourceId(0), &entries[1].path)
            .expect("find")
            .expect("present");
        assert!(seg.kill(row));
        assert!(!seg.kill(row), "killing twice is not two removals");
        assert_eq!(seg.find(SourceId(0), &entries[1].path).expect("find"), None);
        assert_eq!(seg.live_rows(), 1);
        seg.save_alive(tmp.path()).expect("save");
        drop(seg);

        let seg = Live::open(tmp.path(), 1, 0).expect("reopen");
        assert_eq!(seg.live_rows(), 1, "the removal has to survive a restart");
        assert_eq!(seg.entries().expect("entries").count(), 1);
    }

    #[test]
    fn erasing_a_segment_leaves_nothing_behind() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        Live::write(tmp.path(), 3, 0, &build(&[entry("/a/one.rs", 1, 1)])).expect("write");
        Live::erase(tmp.path(), 3);
        assert!(Live::open(tmp.path(), 3, 0).is_err());
        assert_eq!(
            std::fs::read_dir(tmp.path()).expect("read_dir").count(),
            0,
            "a segment is five files and all five go"
        );
    }
}
