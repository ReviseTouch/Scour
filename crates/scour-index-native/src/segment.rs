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

use memmap2::Mmap;
use scour_core::{Entry, EntryId, Error, Result};

use crate::build::SegmentBytes;
use crate::columns::ColumnBlocks;
use crate::dirs::DirTable;
use crate::ids::IdMap;
use crate::names::NameArena;
use crate::search::Segment;
use crate::trigram::TrigramIndex;

/// The pieces a segment is made of, and the extension each is stored under.
const PARTS: [&str; 6] = ["names", "cols", "dirs", "ids", "tgrams", "tpost"];

fn part_path(dir: &Path, number: u64, ext: &str) -> PathBuf {
    dir.join(format!("seg-{number:08}.{ext}"))
}

/// An opened segment: six mapped files and one bitmap that is not.
#[derive(Debug)]
pub struct Live {
    pub number: u64,
    /// The reconciliation pass this segment was written during.
    ///
    /// Kept per segment rather than per row because [`crate::NativeIndex`]
    /// flushes when a generation begins, so a segment never spans two.
    pub generation: u64,
    maps: Vec<Mmap>,
    alive: Vec<u8>,
    rows: usize,
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
        ];
        for (ext, blob) in PARTS.iter().zip(blobs) {
            let p = part_path(dir, number, ext);
            std::fs::write(&p, blob).map_err(|e| Error::io(&e, &p.to_string_lossy()))?;
        }
        let p = part_path(dir, number, "alive");
        std::fs::write(&p, &bytes.alive).map_err(|e| Error::io(&e, &p.to_string_lossy()))?;
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
            maps.push(m);
        }
        let p = part_path(dir, number, "alive");
        let alive = std::fs::read(&p).map_err(|e| Error::io(&e, &p.to_string_lossy()))?;

        let rows = NameArena::open(&maps[0])
            .ok_or_else(|| Error::IndexCorrupt {
                detail: format!("seg-{number:08}.names is unreadable"),
            })?
            .rows();
        Ok(Live {
            number,
            generation,
            maps,
            alive,
            rows,
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

    /// The row holding this identity, if the segment has it and it is live.
    ///
    /// The candidate list from the id table is confirmed against the identity
    /// actually stored in the columns, so a digest collision costs one extra
    /// column read and cannot produce a wrong answer.
    pub fn find(&self, id: &EntryId) -> Result<Option<usize>> {
        let seg = self.view()?;
        for row in self.ids()?.candidates(id) {
            let row = row as usize;
            if self.is_alive(row) && seg.entry_id(row) == *id {
                return Ok(Some(row));
            }
        }
        Ok(None)
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

    pub fn save_alive(&self, dir: &Path) -> Result<()> {
        let p = part_path(dir, self.number, "alive");
        std::fs::write(&p, &self.alive).map_err(|e| Error::io(&e, &p.to_string_lossy()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::build::build;
    use scour_core::{Meta, SourceId};

    fn entry(path: &str, mtime: i64, ino: u64) -> Entry {
        Entry {
            id: EntryId::inode(SourceId(0), 66_310, ino),
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
    fn an_entry_is_found_by_its_identity_and_not_by_luck() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let entries: Vec<Entry> = (0..500)
            .map(|i| entry(&format!("/a/f{i}.rs"), 1000 - i, i as u64))
            .collect();
        let seg = Live::write(tmp.path(), 1, 0, &build(&entries)).expect("write");
        for e in &entries {
            let row = seg.find(&e.id).expect("find").expect("present");
            assert_eq!(
                seg.view().expect("view").entry(row).expect("row").path,
                e.path
            );
        }
        assert_eq!(
            seg.find(&EntryId::inode(SourceId(0), 66_310, 99_999))
                .expect("find"),
            None
        );
    }

    #[test]
    fn a_killed_row_stops_being_found_and_the_bit_persists() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let entries = vec![entry("/a/one.rs", 300, 1), entry("/a/two.rs", 200, 2)];
        let mut seg = Live::write(tmp.path(), 1, 0, &build(&entries)).expect("write");
        let row = seg.find(&entries[1].id).expect("find").expect("present");
        assert!(seg.kill(row));
        assert!(!seg.kill(row), "killing twice is not two removals");
        assert_eq!(seg.find(&entries[1].id).expect("find"), None);
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
