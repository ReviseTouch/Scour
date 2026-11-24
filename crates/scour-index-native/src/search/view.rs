//! A segment as a search reads it: its columns, and where each row's name and
//! folder are.

use crate::columns::{BLOCK, ColumnBlocks, Field};
use crate::dirs::DirTable;
use crate::extension_order::ExtensionOrder;
use crate::name_order::NameOrder;
use crate::names::NameArena;
use crate::order::PathOrder;
use crate::spelling::Spelling;
use crate::trigram::TrigramIndex;
use scour_core::{Entry, EntryId, Hit, Kind, Meta, SourceId};

/// The files a search reads, opened together.
#[derive(Debug, Clone, Copy)]
pub struct Segment<'a> {
    /// Names as the filesystem spells them. Read for the rows that are shown.
    pub names: Spelling<'a>,
    /// The same names, folded when the segment was written. **This is what a
    /// search walks**: folding at query time was three quarters of the inner loop.
    pub folded: NameArena<'a>,
    pub cols: ColumnBlocks<'a>,
    pub dirs: DirTable<'a>,
    pub tri: TrigramIndex<'a>,
    /// The rows in ascending path order, which lets a page ordered by path be a
    /// read of two hundred rows instead of a key built for every match. `None`
    /// falls back to building a key a match. See [`crate::order`].
    pub porder: Option<PathOrder<'a>>,
    /// The rows in ascending folded-name order. Missing on legacy segments;
    /// those use the keyed full walk and produce the same answer.
    pub norder: Option<NameOrder<'a>>,
    /// The rows in ascending folded-extension order. Missing on legacy
    /// segments, which fall back to the keyed full walk.
    pub eorder: Option<ExtensionOrder<'a>>,
    /// One bit a row, set when the row is still live.
    pub alive: &'a [u8],
}

/// Either persisted text order; both have the same grouped-row operations.
/// The extension order is read a chunk at a time, and the chunk is kept.
pub(super) enum GroupedPositions<'a> {
    Name(NameOrder<'a>),
    Extension(ExtensionOrder<'a>, crate::extension_order::Chunk),
}

impl GroupedPositions<'_> {
    pub(super) fn rows(&self) -> usize {
        match self {
            GroupedPositions::Name(order) => order.rows(),
            GroupedPositions::Extension(order, _) => order.rows(),
        }
    }

    pub(super) fn at(&mut self, i: usize) -> Option<u32> {
        match self {
            GroupedPositions::Name(order) => order.at(i),
            GroupedPositions::Extension(order, chunk) => chunk.at(order, i),
        }
    }

    pub(super) fn group_at_or_before(&self, i: usize) -> Option<usize> {
        match self {
            GroupedPositions::Name(order) => order.group_at_or_before(i),
            GroupedPositions::Extension(order, _) => order.group_at_or_before(i),
        }
    }
}

impl<'a> Segment<'a> {
    pub fn rows(&self) -> usize {
        self.cols.rows().min(self.names.rows())
    }

    /// Is any row of this block still live? Four bytes of the bitmap, read once
    /// instead of thirty-two times: a segment a sweep emptied was otherwise
    /// walked in full on every query, 1,204,270 rows to return nothing.
    pub fn block_alive(&self, block: usize) -> bool {
        let from = block * BLOCK / 8;
        let to = (from + BLOCK / 8).min(self.alive.len());
        match self.alive.get(from..to) {
            Some(b) => b.iter().any(|&x| x != 0),
            // An absent bitmap means nothing has been deleted yet.
            None => self.alive.is_empty(),
        }
    }

    pub fn is_alive(&self, row: usize) -> bool {
        match self.alive.get(row / 8) {
            Some(byte) => byte & (1 << (row % 8)) != 0,
            // An absent bitmap means nothing has been deleted yet.
            None => self.alive.is_empty(),
        }
    }

    pub(crate) fn num(&self, field: Field, row: usize) -> i64 {
        self.cols.get(field, row).unwrap_or(0)
    }

    /// One column of one row, for callers outside the walk — faceting, stats.
    pub fn num_of(&self, field: Field, row: usize) -> i64 {
        self.num(field, row)
    }

    /// Which directory a row lives in, as a number into [`Segment::dirs`].
    pub fn dir_id(&self, row: usize) -> u32 {
        self.num(Field::DirId, row) as u32
    }

    /// The full path of a row: its directory joined to its name.
    pub fn path(&self, row: usize, name: &str) -> String {
        let dir = self
            .dirs
            .get(self.num(Field::DirId, row) as u32)
            .unwrap_or_default();
        match dir.as_str() {
            "" => name.to_owned(),
            "/" => format!("/{name}"),
            d => format!("{d}/{name}"),
        }
    }

    pub fn source_of(&self, row: usize) -> SourceId {
        SourceId(self.num(Field::Source, row) as u32)
    }

    /// The identity of a row, which is its source and its path. Derived rather
    /// than stored: identity columns that can disagree with the name left 267
    /// rows at one path. See [`crate::ids`].
    pub fn entry_id(&self, row: usize, name: &str) -> EntryId {
        EntryId::path_hash(self.source_of(row), &self.path(row, name))
    }

    /// A path split the way a row stores it: the directory, then the name.
    pub fn split_path(path: &str) -> (&str, &str) {
        match path.rsplit_once('/') {
            Some(("", name)) => ("/", name),
            Some((parent, name)) => (parent, name),
            None => ("", path),
        }
    }

    /// Is this row the entry at `path`, exactly? The confirmation behind every
    /// probe of the id table, whose key is half a digest. `dirs` is not optional:
    /// decoding a directory per confirmation measured 2.7 µs an entry against 1.1.
    pub fn is_at(
        &self,
        dirs: &mut std::collections::HashMap<u32, String>,
        row: usize,
        source: SourceId,
        path: &str,
    ) -> bool {
        self.is_at_reading(dirs, None, row, source, path)
    }

    /// [`Segment::is_at`] for rows asked in increasing order, reading names on
    /// from the last one: a name from its block start was a third of a
    /// startup's recognising of unchanged rows.
    pub(crate) fn is_at_reading(
        &self,
        dirs: &mut std::collections::HashMap<u32, String>,
        reader: Option<&mut crate::spelling::Reader>,
        row: usize,
        source: SourceId,
        path: &str,
    ) -> bool {
        // **A rejection, not a guarantee.** The probe key mixes the source in, so
        // another source's row for the same path is never a candidate here; the
        // two comparisons below are what make a collision harmless.
        if self.source_of(row) != source {
            return false;
        }
        let (parent, name) = Segment::split_path(path);
        // The name first: a byte comparison against the arena, and it rejects
        // almost every candidate a digest collision produces. The reader says
        // "" where `get` says nothing, so an empty name keeps asking `get`.
        let same_name = match reader {
            Some(r) if !name.is_empty() => r.at(&self.names, row) == name,
            _ => self.names.get(row).as_deref() == Some(name),
        };
        if !same_name {
            return false;
        }
        let dir = self.num(Field::DirId, row) as u32;
        let known = dirs
            .entry(dir)
            .or_insert_with(|| self.dirs.get(dir).unwrap_or_default());
        known == parent
    }

    /// Does this row already say exactly what an entry says? Asked on the view
    /// [`Segment::is_at`] opened — one view per entry measured a rescan of two
    /// million untouched rows at 6.65 s against 3.04. `atime` moves on a read.
    pub fn same_meta(&self, row: usize, meta: &Meta, is_dir: bool) -> bool {
        self.num(Field::Size, row) == meta.size
            && self.num(Field::Mtime, row) == meta.mtime
            && self.num(Field::Ctime, row) == meta.ctime
            && self.num(Field::Mode, row) == meta.mode
            && self.num(Field::Uid, row) == meta.uid
            && self.num(Field::Gid, row) == meta.gid
            && (self.num(Field::IsDir, row) != 0) == is_dir
    }

    pub(crate) fn hit(&self, row: usize, name: &str) -> Hit {
        let path = self.path(row, name);
        Hit {
            // From the path that has just been built, rather than by building
            // it a second time.
            id: EntryId::path_hash(self.source_of(row), &path),
            // Filled by the engine, which is the layer that knows whether
            // anybody asked for it.
            under: None,
            // And these by the content reading, the only one that read the file.
            found: Vec::new(),
            is_dir: self.num(Field::IsDir, row) != 0,
            kind: Kind::from_u8(self.num(Field::Kind, row) as u8).unwrap_or(Kind::File),
            meta: Meta {
                size: self.num(Field::Size, row),
                mtime: self.num(Field::Mtime, row),
                ctime: self.num(Field::Ctime, row),
                atime: self.num(Field::Atime, row),
                mode: self.num(Field::Mode, row),
                uid: self.num(Field::Uid, row),
                gid: self.num(Field::Gid, row),
                disk: self.num(Field::Disk, row),
                items: self.num(Field::Items, row),
                links: self.num(Field::Links, row).max(1),
            },
            path,
        }
    }

    /// Reconstruct a whole entry, for rebuilding or merging.
    pub fn entry(&self, row: usize) -> Option<Entry> {
        let name = self.names.get(row)?;
        Some(self.entry_named(row, &name))
    }

    /// [`Segment::entry`] for a caller that has read the row's spelling.
    pub(crate) fn entry_named(&self, row: usize, name: &str) -> Entry {
        let h = self.hit(row, name);
        Entry {
            id: h.id,
            path: h.path,
            is_dir: h.is_dir,
            meta: h.meta,
        }
    }
}
