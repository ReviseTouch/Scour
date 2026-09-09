//! A segment on disk.
//!
//! Written once and never edited, so the files are mapped and read without a
//! lock while writing continues elsewhere. One thing changes: which rows are
//! live — a bit a row, so deleting a million files touches 125 KB.

use std::path::{Path, PathBuf};

use crate::durable::{replace_synced, write_synced};
use memmap2::Mmap;
use scour_core::{Entry, Error, Result, SourceId};

use crate::build::SegmentBytes;
use crate::columns::ColumnBlocks;
use crate::dirs::DirTable;
use crate::extension_order::ExtensionOrder;
use crate::ids::IdMap;
use crate::name_order::NameOrder;
use crate::names::NameArena;
use crate::order::PathOrder;
use crate::search::Segment;
use crate::trigram::TrigramIndex;

/// The pieces a segment is made of, and the extension each is stored under.
const PARTS: [&str; 7] = ["names", "cols", "dirs", "ids", "tgrams", "tpost", "fnames"];

/// The persisted row orders a segment may be without. Absent means older, not
/// damaged: a search without one builds keys instead, so an old index is not
/// rescanned. Present and the wrong length is refused at [`Live::assemble`].
const PORDER: &str = "porder";
const NORDER: &str = "norder";
const EORDER: &str = "eorder";

fn part_path(dir: &Path, number: u64, ext: &str) -> PathBuf {
    dir.join(format!("seg-{number:08}.{ext}"))
}

/// One piece of a segment: mapped from a file, or held in memory. The second
/// case lets a change be searchable before it is durable — a segment write and
/// sync is 22.5 ms whatever it carries. Every reader takes a `&[u8]` either way.
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

/// An opened segment: immutable mapped parts and one mutable bitmap.
#[derive(Debug)]
pub struct Live {
    pub number: u64,
    /// The reconciliation pass this segment was written during. Per segment,
    /// not per row: a flush begins each generation, so no segment spans two.
    pub generation: u64,
    maps: Vec<Part>,
    /// The rows in path order, for a segment written since [`PORDER`] existed.
    porder: Option<Part>,
    /// The rows in folded-name order, for a segment written since [`NORDER`]
    /// existed.
    norder: Option<Part>,
    /// The rows in folded-extension order, for a segment written since
    /// [`EORDER`] existed.
    eorder: Option<Part>,
    alive: Vec<u8>,
    rows: usize,
    /// How many of those rows are directories, counted once at open: `stats()`
    /// runs once a second and this walks a column of every row. The total, not
    /// the live count — it drifts by the number of folders deleted since.
    dirs: usize,
    /// How many rows this segment has lost since it was opened: a stamp, not a
    /// statistic. Anything derived from live rows is valid only while this is
    /// unchanged, in `O(1)`. Zero at open, as a derived table is too.
    deaths: u64,
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
        // Synced: a manifest surviving a crash its segments did not is an
        // index that cannot be opened.
        for (ext, blob) in PARTS.iter().zip(blobs) {
            write_synced(&part_path(dir, number, ext), blob)?;
        }
        // Written only when there is one: absent is a meaningful state, and an
        // empty blob is what an unreadable directory table produces.
        if !bytes.porder.is_empty() {
            write_synced(&part_path(dir, number, PORDER), &bytes.porder)?;
        }
        if !bytes.norder.is_empty() {
            write_synced(&part_path(dir, number, NORDER), &bytes.norder)?;
        }
        if !bytes.eorder.is_empty() {
            write_synced(&part_path(dir, number, EORDER), &bytes.eorder)?;
        }
        write_synced(&part_path(dir, number, "alive"), &bytes.alive)?;
        Live::open(dir, number, generation)
    }

    pub fn open(dir: &Path, number: u64, generation: u64) -> Result<Live> {
        let mut maps = Vec::with_capacity(PARTS.len());
        for ext in PARTS {
            let p = part_path(dir, number, ext);
            let f = std::fs::File::open(&p).map_err(|e| Error::io(&e, &p.to_string_lossy()))?;
            // SAFETY: a segment is written once and then only deleted, so no
            // one rewrites the file underneath the mapping.
            let m = unsafe { Mmap::map(&f) }.map_err(|e| Error::io(&e, &p.to_string_lossy()))?;
            maps.push(Part::Mapped(m));
        }
        // Missing is allowed here and nowhere else (see [`PORDER`]), and only
        // `NotFound` means older — any other error is reported.
        let p = part_path(dir, number, PORDER);
        let porder = match std::fs::File::open(&p) {
            Ok(f) => Some(Part::Mapped(
                unsafe { Mmap::map(&f) }.map_err(|e| Error::io(&e, &p.to_string_lossy()))?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(Error::io(&e, &p.to_string_lossy())),
        };
        let p = part_path(dir, number, NORDER);
        let norder = match std::fs::File::open(&p) {
            Ok(f) => Some(Part::Mapped(
                unsafe { Mmap::map(&f) }.map_err(|e| Error::io(&e, &p.to_string_lossy()))?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(Error::io(&e, &p.to_string_lossy())),
        };
        let p = part_path(dir, number, EORDER);
        let eorder = match std::fs::File::open(&p) {
            Ok(f) => Some(Part::Mapped(
                unsafe { Mmap::map(&f) }.map_err(|e| Error::io(&e, &p.to_string_lossy()))?,
            )),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(Error::io(&e, &p.to_string_lossy())),
        };
        let p = part_path(dir, number, "alive");
        let alive = std::fs::read(&p).map_err(|e| Error::io(&e, &p.to_string_lossy()))?;
        Live::assemble(number, generation, maps, porder, norder, eorder, alive)
    }

    /// A segment that was never written, and may never be: the bytes `write`
    /// would have stored, kept in memory. A real segment to every reader; the
    /// only thing it is not is durable.
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
        let porder = (!bytes.porder.is_empty()).then(|| Part::Owned(bytes.porder.clone()));
        let norder = (!bytes.norder.is_empty()).then(|| Part::Owned(bytes.norder.clone()));
        let eorder = (!bytes.eorder.is_empty()).then(|| Part::Owned(bytes.eorder.clone()));
        Live::assemble(
            number,
            generation,
            maps,
            porder,
            norder,
            eorder,
            bytes.alive.clone(),
        )
    }

    /// Check the pieces agree with each other and count what a search needs.
    /// Shared by both ways in, so an in-memory segment is validated as hard as
    /// one read off a disk.
    fn assemble(
        number: u64,
        generation: u64,
        maps: Vec<Part>,
        porder: Option<Part>,
        norder: Option<Part>,
        eorder: Option<Part>,
        alive: Vec<u8>,
    ) -> Result<Live> {
        let rows = NameArena::open(&maps[0])
            .ok_or_else(|| Error::IndexCorrupt {
                detail: format!("seg-{number:08}.names is unreadable"),
            })?
            .rows();
        // A short bitmap is not "these rows are dead": it is one bit a row,
        // written whole, so a length saying otherwise is damage.
        let want = rows.div_ceil(8);
        if alive.len() != want {
            return Err(Error::IndexCorrupt {
                detail: format!(
                    "seg-{number:08}.alive is {} bytes for {rows} rows, expected {want}",
                    alive.len()
                ),
            });
        }
        // A path order of the wrong length is damage, refused on the bitmap's
        // argument. Contents are not checked further — nothing here is
        // checksummed — but a bad entry cannot be read as a row: see
        // [`PathOrder::at`].
        if let Some(p) = &porder {
            match PathOrder::open(p) {
                Some(o) if o.rows() == rows => {}
                found => {
                    return Err(Error::IndexCorrupt {
                        detail: format!(
                            "seg-{number:08}.{PORDER} holds {} rows, expected {rows}",
                            found.map_or_else(
                                || "an unreadable number of".to_owned(),
                                |o| o.rows().to_string()
                            )
                        ),
                    });
                }
            }
        }
        // Missing is a legacy segment and uses the keyed name walk. Present
        // but short is damage: streaming it would silently omit rows.
        if let Some(p) = &norder {
            match NameOrder::open(p) {
                Some(o) if o.rows() == rows => {}
                found => {
                    return Err(Error::IndexCorrupt {
                        detail: format!(
                            "seg-{number:08}.{NORDER} holds {} rows, expected {rows}",
                            found.map_or_else(
                                || "an unreadable number of".to_owned(),
                                |o| o.rows().to_string()
                            )
                        ),
                    });
                }
            }
        }
        // Extension order has the same optional-versus-damaged contract. A
        // mixed index streams current segments and keys legacy ones.
        if let Some(p) = &eorder {
            match ExtensionOrder::open(p) {
                Some(o) if o.rows() == rows => {}
                found => {
                    return Err(Error::IndexCorrupt {
                        detail: format!(
                            "seg-{number:08}.{EORDER} holds {} rows, expected {rows}",
                            found.map_or_else(
                                || "an unreadable number of".to_owned(),
                                |o| o.rows().to_string()
                            )
                        ),
                    });
                }
            }
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
            porder,
            norder,
            eorder,
            alive,
            rows,
            dirs,
            deaths: 0,
        })
    }

    /// Erase a segment's files. Called once nothing refers to it.
    pub fn erase(dir: &Path, number: u64) {
        for ext in PARTS
            .iter()
            .chain(std::iter::once(&PORDER))
            .chain(std::iter::once(&NORDER))
            .chain(std::iter::once(&EORDER))
            .chain(std::iter::once(&"alive"))
        {
            // A missing file is the desired state, so a failure to remove one
            // that is already gone is not worth reporting.
            let _ = std::fs::remove_file(part_path(dir, number, ext));
        }
    }

    /// Which of these entries this segment already holds, exactly as they are —
    /// what a rescan asks two million times. Two strategies as in
    /// [`Live::kill_paths`]: a merge for a bulk pass, probes for a watcher's
    /// handful. `keys` must be sorted by [`IdMap::key_of`], each `.1` indexing
    /// `staged`. A row this segment claims sets `decided` whether it matches or
    /// not, so an older segment cannot answer for it; matches reach `out`.
    pub fn spare_paths(
        &self,
        keys: &[(u32, u32)],
        staged: &[Entry],
        decided: &mut [bool],
        out: &mut Vec<(u32, usize)>,
    ) -> Result<()> {
        if keys.is_empty() || self.rows == 0 {
            return Ok(());
        }
        let ids = self.ids()?;
        if ids.is_empty() {
            return Ok(());
        }
        // Rows to confirm, with the entry each one might belong to.
        let mut pairs: Vec<(usize, u32)> = Vec::new();
        if keys.len().saturating_mul(ids.len().ilog2().max(1) as usize) < ids.len() {
            for &(key, at) in keys {
                if decided[at as usize] {
                    continue;
                }
                for row in ids.rows_for(key) {
                    pairs.push((row as usize, at));
                }
            }
        } else {
            let (mut i, mut j) = (0usize, 0usize);
            while i < ids.len() && j < keys.len() {
                let h = ids.at(i).0;
                match h.cmp(&keys[j].0) {
                    std::cmp::Ordering::Less => i += 1,
                    std::cmp::Ordering::Greater => j += 1,
                    std::cmp::Ordering::Equal => {
                        // A run of equal hashes each side: both are tiny, so
                        // the cross product is cheap.
                        let mut i2 = i;
                        while i2 < ids.len() && ids.at(i2).0 == h {
                            i2 += 1;
                        }
                        let mut j2 = j;
                        while j2 < keys.len() && keys[j2].0 == h {
                            j2 += 1;
                        }
                        for k in i..i2 {
                            for w in j..j2 {
                                if !decided[keys[w].1 as usize] {
                                    pairs.push((ids.at(k).1 as usize, keys[w].1));
                                }
                            }
                        }
                        i = i2;
                        j = j2;
                    }
                }
            }
        }
        if pairs.is_empty() {
            return Ok(());
        }
        let seg = self.view()?;
        let mut dirs = std::collections::HashMap::new();
        for (row, at) in pairs {
            let e = &staged[at as usize];
            // A digest collision can name the same row twice in one pass, and
            // an entry settled by an earlier candidate is not asked again.
            if decided[at as usize] || !self.is_alive(row) {
                continue;
            }
            if !seg.is_at(&mut dirs, row, e.id.source, &e.path) {
                continue;
            }
            decided[at as usize] = true;
            if seg.same_meta(row, &e.meta, e.is_dir) {
                out.push((at, row));
            }
        }
        Ok(())
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
            // Validated once when the segment was opened, so this cannot fail
            // in a way `assemble` would not already have refused.
            porder: self.porder.as_deref().and_then(PathOrder::open),
            norder: self.norder.as_deref().and_then(NameOrder::open),
            eorder: self.eorder.as_deref().and_then(ExtensionOrder::open),
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
        // Every route to a dead row comes through here, so one counter covers
        // `kill_paths`, the sweep and a commit that replaces a row.
        self.deaths += u64::from(was);
        was
    }

    /// How many rows have been retired since this segment was opened.
    pub fn deaths(&self) -> u64 {
        self.deaths
    }

    /// The row holding this path, if the segment has it and it is live. The id
    /// table's candidates are confirmed against the row's directory and name,
    /// so a digest collision costs a comparison and never a wrong answer.
    pub fn find(&self, source: SourceId, path: &str) -> Result<Option<usize>> {
        // The identity table first, the view only if there is something to
        // confirm: almost every probe finds nothing, and opening the view
        // parses four headers.
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

    /// Kill the rows holding any of these paths, `wanted` sorted by
    /// [`IdMap::key_of`]. Returns how many died. Two strategies: a merge is
    /// `O(rows)` however few paths are wanted — 44 to 74 ms of held write lock
    /// for a watcher's three — so below `wanted × log rows < rows` it is a
    /// binary search each, which is what the table is sorted for.
    pub fn kill_paths(&mut self, wanted: &[(u32, SourceId, &str)]) -> Result<u64> {
        if wanted.is_empty() || self.rows == 0 {
            return Ok(0);
        }
        let victims: Vec<usize> = {
            let ids = self.ids()?;
            let mut pairs: Vec<(usize, usize)> = Vec::new();
            // `ids.len().ilog2()` is one probe, so this is where probing every
            // path stops being cheaper than walking the table.
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
                        // A run of equal hashes each side: both are tiny, so
                        // the cross product is cheap.
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

    /// A copy of the live bits, to be written once the lock is released. 262 KB
    /// at two million rows against a 13 ms `fsync` a segment, several a commit.
    pub fn alive_snapshot(&self) -> (u64, Vec<u8>) {
        (self.number, self.alive.clone())
    }

    /// Write bits taken by [`Live::alive_snapshot`], with no lock held. A crash
    /// in between leaves the older bitmap, so removed rows come back until the
    /// next sweep — the same as a crash before the write.
    pub fn write_alive(dir: &Path, number: u64, bits: &[u8]) -> Result<()> {
        replace_synced(&part_path(dir, number, "alive"), bits)
    }

    pub fn save_alive(&self, dir: &Path) -> Result<()> {
        // Replaced, not overwritten: read whole at open, so a half-written one
        // makes every row a coin flip between alive and dead.
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

    /// Both ways in must reach the same *reader* — same rows, names and
    /// directory table — or a search answers differently before a commit.
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
            "a segment is nine files and all nine go — the path order included, \
             or an index that has been folded once leaks one per fold"
        );
    }
}
