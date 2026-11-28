//! The table: where each document's text is.
//!
//! **In memory** a path's bytes are kept once, in one buffer, and a row names
//! them by where they start: a map of strings took 200 to 250 bytes a
//! document, a million documents 237 MB (28 Nov). **On disk**, a base written
//! whole now and then, and a log of what changed since, appended as it
//! changes: written whole after every block, the table of a million documents
//! was 134 MB written for each 4 MiB of text.

use std::io::Read as _;
use std::path::Path;

use hashbrown::HashTable;

/// 4: a generation, the next block's number and when each block was last
/// used, before the rows. A table of 3, the rows alone, is read and written
/// again as 4.
const MAGIC: &[u8; 6] = b"SCTC4\n";
const MAGIC_3: &[u8; 6] = b"SCTC3\n";
/// The log, then the generation of the base its records follow: a log left
/// beside another base is not read.
const LOG_MAGIC: &[u8; 6] = b"SCTL1\n";

/// A table of this cache, of this version or an earlier one: what says a
/// directory is a cache's and not somebody's documents.
pub(crate) fn version_of(path: &Path) -> Option<bool> {
    let mut head = [0u8; 6];
    let mut file = std::fs::File::open(path).ok()?;
    file.read_exact(&mut head).ok()?;
    head.starts_with(b"SCTC")
        .then_some(&head == MAGIC || &head == MAGIC_3)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    Words,
    NoText,
    Failed(u32),
}

/// One document's row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Row {
    pub size: u64,
    pub mtime: i64,
    pub state: State,
    /// Where its words are: a block's number, and a range inside it.
    pub block: u32,
    pub offset: u32,
    pub len: u32,
}

#[derive(Debug, Clone, Copy)]
struct Slot {
    row: Row,
    /// Where its path starts in [`Table::paths`]; [`FREE`] when no row is here.
    at: u32,
    len: u32,
}

const FREE: u32 = u32::MAX;

/// Every row, by path.
#[derive(Default)]
pub(crate) struct Table {
    paths: Vec<u8>,
    slots: Vec<Slot>,
    index: HashTable<u32>,
    free: Vec<u32>,
    /// Bytes of `paths` no row names any more.
    holes: usize,
}

impl std::fmt::Debug for Table {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Table").field("rows", &self.len()).finish()
    }
}

/// FNV-1a: paths are this machine's own, so nobody picks them to collide.
pub(crate) fn hash(path: &[u8]) -> u64 {
    path.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
        (h ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn key<'a>(paths: &'a [u8], slot: &Slot) -> &'a [u8] {
    &paths[slot.at as usize..slot.at as usize + slot.len as usize]
}

impl Table {
    pub(crate) fn len(&self) -> usize {
        self.index.len()
    }

    pub(crate) fn get(&self, path: &str) -> Option<&Row> {
        let bytes = path.as_bytes();
        let (paths, slots) = (&self.paths, &self.slots);
        let &i = self
            .index
            .find(hash(bytes), |&i| key(paths, &slots[i as usize]) == bytes)?;
        Some(&slots[i as usize].row)
    }

    /// Whether a path this long still fits: its place is a 32-bit number.
    pub(crate) fn has_room(&self, path: &str) -> bool {
        self.paths.len() - self.holes + path.len() < FREE as usize
    }

    /// The row for `path`, in place of any before it, which is returned.
    pub(crate) fn insert(&mut self, path: &str, row: Row) -> Option<Row> {
        let bytes = path.as_bytes();
        let h = hash(bytes);
        let (paths, slots) = (&self.paths, &self.slots);
        if let Some(&i) = self
            .index
            .find(h, |&i| key(paths, &slots[i as usize]) == bytes)
        {
            return Some(std::mem::replace(&mut self.slots[i as usize].row, row));
        }
        if self.paths.len() + bytes.len() >= FREE as usize {
            self.shrink(true);
            if !self.has_room(path) {
                return None;
            }
        }
        let Table {
            paths,
            slots,
            index,
            free,
            ..
        } = self;
        let slot = Slot {
            row,
            at: paths.len() as u32,
            len: bytes.len() as u32,
        };
        paths.extend_from_slice(bytes);
        let i = match free.pop() {
            Some(i) => {
                slots[i as usize] = slot;
                i
            }
            None => {
                slots.push(slot);
                (slots.len() - 1) as u32
            }
        };
        index.insert_unique(h, i, |&j| hash(key(paths, &slots[j as usize])));
        None
    }

    pub(crate) fn remove(&mut self, path: &str) -> Option<Row> {
        let bytes = path.as_bytes();
        let (paths, slots) = (&self.paths, &self.slots);
        let entry = self
            .index
            .find_entry(hash(bytes), |&i| key(paths, &slots[i as usize]) == bytes)
            .ok()?;
        let (i, _) = entry.remove();
        Some(self.release(i))
    }

    /// A slot no row holds any more, its path's bytes a hole.
    fn release(&mut self, i: u32) -> Row {
        let slot = &mut self.slots[i as usize];
        self.holes += slot.len as usize;
        slot.at = FREE;
        self.free.push(i);
        let row = slot.row;
        self.shrink(false);
        row
    }

    /// Only the rows `keep` says yes to.
    pub(crate) fn retain(&mut self, mut keep: impl FnMut(&Row) -> bool) {
        let gone: Vec<u32> = (0..self.slots.len() as u32)
            .filter(|&i| {
                let slot = &self.slots[i as usize];
                slot.at != FREE && !keep(&slot.row)
            })
            .collect();
        for i in gone {
            let h = hash(key(&self.paths, &self.slots[i as usize]));
            if let Ok(entry) = self.index.find_entry(h, |&j| j == i) {
                entry.remove();
            }
            self.release(i);
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&[u8], &Row)> {
        self.slots
            .iter()
            .filter(|s| s.at != FREE)
            .map(|s| (key(&self.paths, s), &s.row))
    }

    pub(crate) fn values(&self) -> impl Iterator<Item = &Row> {
        self.slots.iter().filter(|s| s.at != FREE).map(|s| &s.row)
    }

    pub(crate) fn values_mut(&mut self) -> impl Iterator<Item = &mut Row> {
        self.slots
            .iter_mut()
            .filter(|s| s.at != FREE)
            .map(|s| &mut s.row)
    }

    pub(crate) fn clear(&mut self) {
        *self = Table::default();
    }

    /// What it holds in memory, about.
    pub(crate) fn memory(&self) -> u64 {
        (self.paths.capacity()
            + self.slots.capacity() * size_of::<Slot>()
            + self.index.capacity() * (size_of::<u32>() + 1)
            + self.free.capacity() * size_of::<u32>()) as u64
    }

    /// The paths written again without their holes, once they are half the
    /// buffer, or whenever `now`.
    fn shrink(&mut self, now: bool) {
        if !now && (self.holes < 1 << 20 || self.holes * 2 < self.paths.len()) {
            return;
        }
        let mut paths = Vec::with_capacity(self.paths.len() - self.holes);
        for slot in self.slots.iter_mut().filter(|s| s.at != FREE) {
            let at = paths.len() as u32;
            paths.extend_from_slice(key(&self.paths, slot));
            slot.at = at;
        }
        self.paths = paths;
        self.holes = 0;
    }
}

/// What a base holds besides the rows.
#[derive(Debug, Default)]
pub(crate) struct Base {
    pub table: Table,
    /// Text in the blocks no row points at any more.
    pub dead: u64,
    /// Which log follows it.
    pub generation: u64,
    /// No block takes a number below this one, though it was lost.
    pub next: u32,
    /// Seconds since the epoch each block was last read.
    pub used: Vec<(u32, u64)>,
    /// Read from a table of version 3: written again as 4.
    pub older: bool,
}

/// The base at `path`; empty when it is missing, torn or not a table.
pub(crate) fn load(path: &Path) -> Base {
    let mut bytes = Vec::new();
    let read = std::fs::File::open(path).and_then(|mut f| f.read_to_end(&mut bytes));
    if read.is_err() {
        return Base::default();
    }
    parse_base(&bytes).unwrap_or_default()
}

fn parse_base(bytes: &[u8]) -> Option<Base> {
    let mut base = Base::default();
    let mut b = if let Some(rest) = bytes.strip_prefix(MAGIC_3.as_slice()) {
        base.older = true;
        rest
    } else {
        bytes.strip_prefix(MAGIC.as_slice())?
    };
    if !base.older {
        base.generation = u64::from_le_bytes(take(&mut b)?);
    }
    base.dead = u64::from_le_bytes(take(&mut b)?);
    if !base.older {
        base.next = u32::from_le_bytes(take(&mut b)?);
        let used = u32::from_le_bytes(take(&mut b)?);
        for _ in 0..used {
            let block = u32::from_le_bytes(take(&mut b)?);
            base.used.push((block, u64::from_le_bytes(take(&mut b)?)));
        }
    }
    while !b.is_empty() {
        let (path, row) = row_of(&mut b)?;
        base.table.insert(path, row);
    }
    Some(base)
}

fn row_of<'a>(b: &mut &'a [u8]) -> Option<(&'a str, Row)> {
    let len = u32::from_le_bytes(take(b)?) as usize;
    let path = std::str::from_utf8(b.get(..len)?).ok()?;
    *b = &b[len..];
    let size = u64::from_le_bytes(take(b)?);
    let mtime = i64::from_le_bytes(take(b)?);
    let [tag] = take::<1>(b)?;
    let version = u32::from_le_bytes(take(b)?);
    let block = u32::from_le_bytes(take(b)?);
    let offset = u32::from_le_bytes(take(b)?);
    let text = u32::from_le_bytes(take(b)?);
    let state = match tag {
        0 => State::Words,
        1 => State::NoText,
        _ => State::Failed(version),
    };
    let row = Row {
        size,
        mtime,
        state,
        block,
        offset,
        len: text,
    };
    Some((path, row))
}

fn put_row(out: &mut Vec<u8>, path: &[u8], row: &Row) {
    out.extend_from_slice(&(path.len() as u32).to_le_bytes());
    out.extend_from_slice(path);
    out.extend_from_slice(&row.size.to_le_bytes());
    out.extend_from_slice(&row.mtime.to_le_bytes());
    let (tag, version) = match row.state {
        State::Words => (0u8, 0),
        State::NoText => (1, 0),
        State::Failed(v) => (2, v),
    };
    out.push(tag);
    out.extend_from_slice(&version.to_le_bytes());
    out.extend_from_slice(&row.block.to_le_bytes());
    out.extend_from_slice(&row.offset.to_le_bytes());
    out.extend_from_slice(&row.len.to_le_bytes());
}

fn take<const N: usize>(b: &mut &[u8]) -> Option<[u8; N]> {
    let (head, rest) = b.split_first_chunk::<N>()?;
    *b = rest;
    Some(*head)
}

/// The base's bytes, made under the lock.
pub(crate) fn encode(
    table: &Table,
    dead: u64,
    generation: u64,
    next: u32,
    used: &[(u32, u64)],
) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        MAGIC.len() + 24 + 12 * used.len() + table.paths.len() + 37 * table.len(),
    );
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&generation.to_le_bytes());
    out.extend_from_slice(&dead.to_le_bytes());
    out.extend_from_slice(&next.to_le_bytes());
    out.extend_from_slice(&(used.len() as u32).to_le_bytes());
    for (block, at) in used {
        out.extend_from_slice(&block.to_le_bytes());
        out.extend_from_slice(&at.to_le_bytes());
    }
    for (path, row) in table.iter() {
        put_row(&mut out, path, row);
    }
    out
}

/// What the log says happened, in the order it happened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Change {
    /// A row put, in place of any before it.
    Put(String, Row),
    /// A row taken out, nothing in its place.
    Remove(String),
    /// A block given up, with every row in it.
    Drop(u32),
    /// When a block was last read.
    Used(u32, u64),
    /// The rows of one block's number moved to another's, before any block
    /// was written under the first.
    Move(u32, u32),
}

/// A log's first bytes.
pub(crate) fn log_head(generation: u64) -> Vec<u8> {
    let mut out = LOG_MAGIC.to_vec();
    out.extend_from_slice(&generation.to_le_bytes());
    out
}

/// A change as the log writes it: a tag, what it says, and a checksum of
/// both, so a record torn by a crash ends the log rather than being read.
pub(crate) fn record(out: &mut Vec<u8>, change: &Change) {
    let start = out.len();
    match change {
        Change::Put(path, row) => {
            out.push(1);
            put_row(out, path.as_bytes(), row);
        }
        Change::Remove(path) => {
            out.push(2);
            out.extend_from_slice(&(path.len() as u32).to_le_bytes());
            out.extend_from_slice(path.as_bytes());
        }
        Change::Drop(block) => {
            out.push(3);
            out.extend_from_slice(&block.to_le_bytes());
        }
        Change::Used(block, at) => {
            out.push(4);
            out.extend_from_slice(&block.to_le_bytes());
            out.extend_from_slice(&at.to_le_bytes());
        }
        Change::Move(from, to) => {
            out.push(5);
            out.extend_from_slice(&from.to_le_bytes());
            out.extend_from_slice(&to.to_le_bytes());
        }
    }
    let sum = hash(&out[start..]) as u32;
    out.extend_from_slice(&sum.to_le_bytes());
}

/// The changes a log holds after the base of `generation`, up to the first
/// that is torn, and whether it was read to its end. None for a log of
/// another base, or none at all.
pub(crate) fn replay(path: &Path, generation: u64) -> Option<(Vec<Change>, bool)> {
    let bytes = std::fs::read(path).ok()?;
    let mut b = bytes.strip_prefix(LOG_MAGIC.as_slice())?;
    if u64::from_le_bytes(take(&mut b)?) != generation {
        return None;
    }
    let mut changes = Vec::new();
    while !b.is_empty() {
        let mut r = b;
        let Some(change) = change_of(&mut r) else {
            break;
        };
        let body = b.len() - r.len();
        let Some(sum) = take::<4>(&mut r).map(u32::from_le_bytes) else {
            break;
        };
        if sum != hash(&b[..body]) as u32 {
            break;
        }
        changes.push(change);
        b = r;
    }
    Some((changes, b.is_empty()))
}

fn change_of(b: &mut &[u8]) -> Option<Change> {
    let [tag] = take::<1>(b)?;
    Some(match tag {
        1 => {
            let (path, row) = row_of(b)?;
            Change::Put(path.to_owned(), row)
        }
        2 => {
            let len = u32::from_le_bytes(take(b)?) as usize;
            let path = std::str::from_utf8(b.get(..len)?).ok()?.to_owned();
            *b = &b[len..];
            Change::Remove(path)
        }
        3 => Change::Drop(u32::from_le_bytes(take(b)?)),
        4 => {
            let block = u32::from_le_bytes(take(b)?);
            Change::Used(block, u64::from_le_bytes(take(b)?))
        }
        5 => {
            let from = u32::from_le_bytes(take(b)?);
            Change::Move(from, u32::from_le_bytes(take(b)?))
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(block: u32, len: u32) -> Row {
        Row {
            size: 9,
            mtime: -3,
            state: State::Words,
            block,
            offset: 5,
            len,
        }
    }

    #[test]
    fn rows_are_found_replaced_and_taken_out_by_path() {
        let mut t = Table::default();
        assert_eq!(t.insert("/a/ş.pdf", row(1, 4)), None);
        assert_eq!(t.insert("/b", row(2, 4)), None);
        assert_eq!(t.insert("/a/ş.pdf", row(3, 4)), Some(row(1, 4)));
        assert_eq!((t.len(), t.get("/a/ş.pdf")), (2, Some(&row(3, 4))));
        assert_eq!(t.remove("/b"), Some(row(2, 4)));
        assert_eq!(t.get("/b"), None);
        assert_eq!((t.remove("/b"), t.len()), (None, 1));
        // A freed slot takes the next row.
        t.insert("/c", row(4, 4));
        assert_eq!(t.slots.len(), 2);
        t.retain(|r| r.block != 3);
        let left: Vec<_> = t.iter().map(|(p, r)| (p.to_vec(), r.block)).collect();
        assert_eq!(left, [(b"/c".to_vec(), 4)]);
    }

    #[test]
    fn a_buffer_of_mostly_holes_is_written_again() {
        let mut t = Table::default();
        let long = |i: usize| format!("/{}/{i}", "x".repeat(200));
        for i in 0..20_000 {
            t.insert(&long(i), row(0, 1));
        }
        for i in 0..15_000 {
            t.remove(&long(i));
        }
        assert!(t.holes < t.paths.len(), "{} of {}", t.holes, t.paths.len());
        for i in 15_000..20_000 {
            assert!(t.get(&long(i)).is_some());
        }
    }

    #[test]
    fn a_base_reads_back_as_written_and_a_torn_one_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("table");
        let mut table = Table::default();
        table.insert(
            "/a/ş.pdf",
            Row {
                state: State::Failed(7),
                ..row(2, 0)
            },
        );
        table.insert("/b", row(2, 4));
        let bytes = encode(&table, 11, 5, 9, &[(2, 100)]);
        std::fs::write(&path, &bytes).unwrap();
        let base = load(&path);
        assert_eq!(
            (
                base.dead,
                base.generation,
                base.next,
                base.used.clone(),
                base.older
            ),
            (11, 5, 9, vec![(2, 100)], false)
        );
        let mut rows: Vec<_> = base.table.iter().map(|(p, r)| (p.to_vec(), *r)).collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));
        let mut want: Vec<_> = table.iter().map(|(p, r)| (p.to_vec(), *r)).collect();
        want.sort_by(|a, b| a.0.cmp(&b.0));
        assert_eq!(rows, want);
        std::fs::write(&path, &bytes[..bytes.len() - 2]).unwrap();
        assert_eq!(load(&path).table.len(), 0);
    }

    #[test]
    fn a_table_of_version_three_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("table");
        let mut bytes = MAGIC_3.to_vec();
        bytes.extend_from_slice(&7u64.to_le_bytes());
        put_row(&mut bytes, b"/eski.pdf", &row(3, 12));
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(version_of(&path), Some(true));
        let base = load(&path);
        assert!(base.older);
        assert_eq!(
            (base.dead, base.table.get("/eski.pdf")),
            (7, Some(&row(3, 12)))
        );
    }

    #[test]
    fn a_log_is_read_to_its_first_torn_record_and_only_after_its_own_base() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("log");
        let changes = [
            Change::Put("/a".into(), row(1, 4)),
            Change::Used(1, 77),
            Change::Move(2, 3),
            Change::Remove("/a".into()),
            Change::Drop(1),
        ];
        let mut bytes = log_head(4);
        for c in &changes {
            record(&mut bytes, c);
        }
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(replay(&path, 4).unwrap(), (changes.to_vec(), true));
        assert_eq!(replay(&path, 5), None, "another base's log");
        // Torn in the last record: the four before it stand.
        std::fs::write(&path, &bytes[..bytes.len() - 3]).unwrap();
        assert_eq!(replay(&path, 4).unwrap(), (changes[..4].to_vec(), false));
        // A byte changed inside the second: only the first stands.
        let mut bad = bytes.clone();
        let second = log_head(4).len()
            + {
                let mut one = Vec::new();
                record(&mut one, &changes[0]);
                one.len()
            }
            + 2;
        bad[second] ^= 0xFF;
        std::fs::write(&path, &bad).unwrap();
        assert_eq!(replay(&path, 4).unwrap(), (changes[..1].to_vec(), false));
    }
}
