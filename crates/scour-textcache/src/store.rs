//! The cache: a table in memory, the block being filled in memory, full
//! blocks waiting for their compression in memory, finished blocks on disk,
//! and the last few of those uncompressed.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

use crate::block;
use crate::key::{Kept, Key};
use crate::table::{self, Row, State, Table};

/// A block is sealed and compressed once it holds this much text: 4 MiB
/// compresses 6.60x at level 19 where 1 MiB gives 6.06x.
const BLOCK: usize = 4 << 20;
/// Blocks kept uncompressed for the next document that is in one of them.
const HOT: usize = 8;
/// Past this share of dead text the blocks are written again.
const DEAD_SHARE: f64 = 0.30;
/// Not worth writing again below this much text in all.
const COMPACT_FLOOR: u64 = 4 << 20;

/// What the cache holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub documents: u64,
    pub no_text: u64,
    pub failed: u64,
    /// Text still pointed at, and text left behind by changed documents.
    pub live: u64,
    pub dead: u64,
    /// Compressed, on disk.
    pub disk: u64,
}

pub struct Store {
    shared: Arc<Shared>,
}

struct Shared {
    dir: PathBuf,
    max_disk: u64,
    inner: Mutex<Inner>,
    hot: Mutex<Vec<(u32, Slot)>>,
    /// One compressor at a time: the background thread and a flush would
    /// otherwise write the same block through the same temporary file.
    packer: Mutex<()>,
    /// The generation of the table last written: the table is encoded under
    /// the lock and written after it, so searches do not wait on a disk's
    /// flush, and an older encoding never lands over a newer one.
    written: Mutex<u64>,
    /// Blocks read from disk, for the tests.
    #[cfg(test)]
    reads: std::sync::atomic::AtomicUsize,
}

/// A hot block, read by the first reader that wants it; any other that wants
/// it meanwhile waits for that one. Each reader decompressing it for itself,
/// ten readers decompressed 6,586 blocks where one decompressed 680 (28 Nov).
type Slot = Arc<OnceLock<Result<Arc<Vec<u8>>, io::ErrorKind>>>;

/// A table encoded and not yet written.
struct Snapshot {
    generation: u64,
    bytes: Vec<u8>,
}

struct Inner {
    table: Table,
    pending: Vec<u8>,
    pending_block: u32,
    /// Full blocks not yet on disk, still read from here.
    sealed: HashMap<u32, Arc<Vec<u8>>>,
    /// Whether a thread is compressing the sealed blocks.
    packing: bool,
    /// Compressed size of each block on disk.
    blocks: HashMap<u32, u64>,
    dead: u64,
    dirty: bool,
    generation: u64,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("dir", &self.shared.dir)
            .finish_non_exhaustive()
    }
}

/// The file whose presence says a directory is a text cache.
const MARK: &str = "SCOUR-TEXTS";

/// `dir` as a cache: made, or empty, or marked, or holding a table of an
/// earlier cache — marked then. Anything else is somebody's.
fn claim(dir: &Path) -> io::Result<()> {
    let mark = dir.join(MARK);
    if mark.is_file() {
        return Ok(());
    }
    let empty = match std::fs::read_dir(dir) {
        Ok(mut entries) => entries.next().is_none(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => true,
        Err(e) => return Err(e),
    };
    if !empty && table::version_of(&dir.join("table")).is_none() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "{} holds files that are not a Scour text cache",
                dir.display()
            ),
        ));
    }
    std::fs::create_dir_all(dir)?;
    block::write_private(
        &mark,
        b"Scour keeps documents' text here. Safe to delete.\n",
    )
}

impl Store {
    /// The cache in `dir`, made if missing; `max_disk` bytes compressed at
    /// most, the oldest blocks given up first. Zero is no limit.
    ///
    /// **A directory that holds anything but a cache is refused**: blocks are
    /// known by their names, and clearing removes them, so a folder of
    /// somebody's files picked by mistake would lose those that look alike.
    pub fn open(dir: &Path, max_disk: u64) -> io::Result<Store> {
        claim(dir)?;
        block::private_dir(dir)?;
        if table::version_of(&dir.join("table")) == Some(false) {
            // An older cache's: its blocks go with its table.
            for entry in std::fs::read_dir(dir)?.flatten() {
                if block::is_block(&entry.file_name().to_string_lossy()) {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
            let _ = std::fs::remove_file(dir.join("table"));
        }
        let mut blocks = HashMap::new();
        for entry in std::fs::read_dir(dir)?.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(n) = block::is_block(&name)
                .then(|| name[..8].parse::<u32>().ok())
                .flatten()
            {
                blocks.insert(n, entry.metadata().map_or(0, |m| m.len()));
            }
        }
        let (mut table, dead) = table::load(&dir.join("table"));
        // A row whose block is not there was written before a crash.
        table.retain(|_, row| row.state != State::Words || blocks.contains_key(&row.block));
        let next = blocks.keys().max().map_or(0, |n| n + 1);
        let inner = Inner {
            table,
            pending: Vec::new(),
            pending_block: next,
            sealed: HashMap::new(),
            packing: false,
            blocks,
            dead,
            dirty: false,
            generation: 0,
        };
        Ok(Store {
            shared: Arc::new(Shared {
                dir: dir.to_path_buf(),
                max_disk,
                inner: Mutex::new(inner),
                hot: Mutex::new(Vec::new()),
                packer: Mutex::new(()),
                written: Mutex::new(0),
                #[cfg(test)]
                reads: Default::default(),
            }),
        })
    }

    /// What is kept for this file as it is now. A text taken when the file
    /// was another size or time is not returned; nor is a failure under
    /// another `version` of the readers.
    pub fn get(&self, key: &Key, version: u32) -> Option<Kept> {
        self.get_from(key, version, &mut None)
    }

    /// What is kept for each of these files, given to `each` with its index.
    /// The block of the last text is held, so keys in the order of
    /// [`Store::place`] decompress each block once, however many readers
    /// come and go through the hot few meanwhile.
    pub fn get_each(&self, keys: &[Key], version: u32, mut each: impl FnMut(usize, Option<Kept>)) {
        let mut last = None;
        for (i, key) in keys.iter().enumerate() {
            each(i, self.get_from(key, version, &mut last));
        }
    }

    /// Where the text kept for `path` lies, its block then its offset: texts
    /// read in this order are read a block at a time. None when no text is
    /// kept for it.
    pub fn place(&self, path: &str) -> Option<(u32, u32)> {
        let inner = self.shared.inner.lock();
        let row = inner.table.get(path)?;
        (row.state == State::Words).then_some((row.block, row.offset))
    }

    fn get_from(
        &self,
        key: &Key,
        version: u32,
        last: &mut Option<(u32, Arc<Vec<u8>>)>,
    ) -> Option<Kept> {
        let (row, held) = {
            let inner = self.shared.inner.lock();
            let row = *inner.table.get(&key.path)?;
            let same = Key {
                path: key.path.clone(),
                size: row.size,
                mtime: row.mtime,
            };
            if !same.same_file(key) {
                return None;
            }
            let held = (row.state == State::Words)
                .then(|| inner.in_memory(&row))
                .flatten();
            (row, held)
        };
        match row.state {
            State::NoText => Some(Kept::NoText),
            State::Failed(v) if v == version => Some(Kept::Failed { version }),
            State::Failed(_) => None,
            State::Words => {
                let text = match held {
                    Some(text) => text?,
                    None => {
                        let block = match last {
                            Some((n, block)) if *n == row.block => Arc::clone(block),
                            _ => {
                                let block = self.shared.block(row.block).ok()?;
                                *last = Some((row.block, Arc::clone(&block)));
                                block
                            }
                        };
                        text_at(&block, &row)?
                    }
                };
                Some(Kept::Words(text))
            }
        }
    }

    /// Keep what this file gave, in place of anything kept for it before.
    /// A full block is compressed by a thread of its own, so the reader that
    /// filled it does not wait.
    pub fn put(&self, key: &Key, kept: &Kept) -> io::Result<()> {
        let mut inner = self.shared.inner.lock();
        if let Some(old) = inner.table.remove(&key.path) {
            inner.dead += u64::from(old.len);
        }
        let mut row = Row {
            size: key.size,
            mtime: key.mtime,
            state: State::NoText,
            block: inner.pending_block,
            offset: 0,
            len: 0,
        };
        match kept {
            Kept::NoText => {}
            Kept::Failed { version } => row.state = State::Failed(*version),
            Kept::Words(text) => {
                row.state = State::Words;
                row.offset = inner.pending.len() as u32;
                row.len = text.len() as u32;
                inner.pending.extend_from_slice(text.as_bytes());
            }
        }
        inner.table.insert(key.path.clone(), row);
        inner.dirty = true;
        if inner.pending.len() >= BLOCK {
            inner.seal();
            if !inner.packing {
                inner.packing = true;
                let shared = Arc::clone(&self.shared);
                let started = std::thread::Builder::new()
                    .name("textcache-pack".into())
                    .spawn(move || shared.pack_in_background());
                if started.is_err() {
                    inner.packing = false;
                }
            }
        }
        Ok(())
    }

    /// Everything on disk, compressed here: the block being filled, every
    /// sealed one, and the table.
    pub fn flush(&self) -> io::Result<()> {
        self.shared.inner.lock().seal();
        self.shared.pack_sealed()
    }

    /// Write every live text into new blocks when dead text is over its share.
    /// True when it did. Compressing happens outside the lock, so searches go
    /// on meanwhile; a document changed in between keeps its newer text.
    pub fn compact(&self) -> io::Result<bool> {
        self.flush()?;
        let mut snapshot: Vec<(String, Row)> = {
            let inner = self.shared.inner.lock();
            let live: u64 = inner.table.values().map(|r| u64::from(r.len)).sum();
            let total = live + inner.dead;
            if total < COMPACT_FLOOR || (inner.dead as f64) < DEAD_SHARE * total as f64 {
                return Ok(false);
            }
            inner
                .table
                .iter()
                .filter(|(_, r)| r.state == State::Words)
                .map(|(p, r)| (p.clone(), *r))
                .collect()
        };
        // Written again in the order they were written, which is the order a
        // search reads them: in the table's order, a search afterwards
        // decompressed 12,253 blocks where it had decompressed 28 (28 Nov).
        snapshot.sort_unstable_by(|(pa, a), (pb, b)| {
            (a.block, a.offset, pa).cmp(&(b.block, b.offset, pb))
        });
        let old: Vec<u32> = self.shared.inner.lock().blocks.keys().copied().collect();
        let mut moved: Vec<(String, Row, Row)> = Vec::with_capacity(snapshot.len());
        let mut written: Vec<(u32, u64)> = Vec::new();
        let (mut fill, mut next) = (Vec::new(), self.shared.reserve());
        for (path, row) in snapshot {
            let Some(text) = self
                .shared
                .block(row.block)
                .ok()
                .and_then(|b| text_at(&b, &row))
            else {
                continue;
            };
            let new = Row {
                block: next,
                offset: fill.len() as u32,
                ..row
            };
            fill.extend_from_slice(text.as_bytes());
            moved.push((path, row, new));
            if fill.len() >= BLOCK {
                written.push((next, block::write(&self.shared.dir, next, &fill)?));
                fill.clear();
                next = self.shared.reserve();
            }
        }
        if !fill.is_empty() {
            written.push((next, block::write(&self.shared.dir, next, &fill)?));
        }
        let mut inner = self.shared.inner.lock();
        for (n, size) in written {
            inner.blocks.insert(n, size);
        }
        for (path, was, new) in moved {
            if inner.table.get(&path) == Some(&was) {
                inner.table.insert(path, new);
            }
        }
        // An old block goes once no row points into it.
        for n in old {
            let used = inner
                .table
                .values()
                .any(|r| r.state == State::Words && r.block == n);
            if !used {
                inner.blocks.remove(&n);
                let _ = std::fs::remove_file(block::path_of(&self.shared.dir, n));
            }
        }
        inner.dead = 0;
        inner.dirty = true;
        self.shared.hot.lock().clear();
        let snapshot = self.shared.snapshot(&mut inner);
        drop(inner);
        self.shared.write_out(snapshot)?;
        Ok(true)
    }

    /// Forget everything.
    pub fn clear(&self) -> io::Result<()> {
        let mut inner = self.shared.inner.lock();
        for n in inner.blocks.keys() {
            let _ = std::fs::remove_file(block::path_of(&self.shared.dir, *n));
        }
        inner.blocks.clear();
        inner.table.clear();
        inner.pending.clear();
        inner.sealed.clear();
        inner.dead = 0;
        inner.dirty = true;
        self.shared.hot.lock().clear();
        let snapshot = self.shared.snapshot(&mut inner);
        drop(inner);
        self.shared.write_out(snapshot)
    }

    pub fn stats(&self) -> Stats {
        let inner = self.shared.inner.lock();
        let mut stats = Stats {
            dead: inner.dead,
            disk: inner.blocks.values().sum(),
            ..Stats::default()
        };
        for row in inner.table.values() {
            match row.state {
                State::Words => stats.documents += 1,
                State::NoText => stats.no_text += 1,
                State::Failed(_) => stats.failed += 1,
            }
            stats.live += u64::from(row.len);
        }
        stats
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

impl Inner {
    /// The block being filled becomes a sealed one, read from memory until
    /// it is on disk.
    fn seal(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let n = self.pending_block;
        self.sealed
            .insert(n, Arc::new(std::mem::take(&mut self.pending)));
        self.pending_block = n + 1;
    }

    /// A row's text while its block is not yet on disk.
    fn in_memory(&self, row: &Row) -> Option<Option<String>> {
        if row.block == self.pending_block {
            return Some(text_at(&self.pending, row));
        }
        self.sealed.get(&row.block).map(|b| text_at(b, row))
    }
}

impl Shared {
    /// A block number nothing else takes: the block being filled moves up
    /// one, its rows with it.
    fn reserve(&self) -> u32 {
        let mut inner = self.inner.lock();
        let n = inner.pending_block;
        for row in inner.table.values_mut() {
            if row.block == n {
                row.block = n + 1;
            }
        }
        inner.pending_block = n + 1;
        n
    }

    fn pack_in_background(self: Arc<Self>) {
        if let Err(e) = self.pack_sealed() {
            eprintln!("textcache: a block could not be written: {e}");
        }
        self.inner.lock().packing = false;
    }

    /// Every sealed block compressed and written, oldest first, the lock let
    /// go while each is compressed.
    fn pack_sealed(&self) -> io::Result<()> {
        let _one = self.packer.lock();
        loop {
            let next = {
                let inner = self.inner.lock();
                inner
                    .sealed
                    .iter()
                    .map(|(n, b)| (*n, Arc::clone(b)))
                    .min_by_key(|(n, _)| *n)
            };
            let Some((n, text)) = next else {
                let snapshot = self.snapshot(&mut self.inner.lock());
                return self.write_out(snapshot);
            };
            let packed = block::write(&self.dir, n, &text)?;
            let snapshot = {
                let mut inner = self.inner.lock();
                inner.blocks.insert(n, packed);
                inner.sealed.remove(&n);
                inner.dirty = true;
                self.snapshot(&mut inner)
            };
            self.write_out(snapshot)?;
        }
    }

    /// Fit under the ceiling, oldest blocks first, then the table encoded
    /// if anything changed — to be written once the lock is let go.
    fn snapshot(&self, inner: &mut Inner) -> Option<Snapshot> {
        while self.max_disk > 0 && inner.blocks.values().sum::<u64>() > self.max_disk {
            let Some(&oldest) = inner.blocks.keys().min() else {
                break;
            };
            inner.blocks.remove(&oldest);
            let _ = std::fs::remove_file(block::path_of(&self.dir, oldest));
            inner
                .table
                .retain(|_, row| !(row.state == State::Words && row.block == oldest));
            self.hot.lock().retain(|(b, _)| *b != oldest);
            inner.dirty = true;
        }
        if !inner.dirty {
            return None;
        }
        inner.dirty = false;
        inner.generation += 1;
        Some(Snapshot {
            generation: inner.generation,
            bytes: table::encode(&inner.table, inner.dead),
        })
    }

    /// Outside the lock. A write that fails leaves the table dirty, so the
    /// next one writes it.
    fn write_out(&self, snapshot: Option<Snapshot>) -> io::Result<()> {
        let Some(s) = snapshot else { return Ok(()) };
        let mut last = self.written.lock();
        if s.generation <= *last {
            return Ok(());
        }
        match block::write_private(&self.dir.join("table"), &s.bytes) {
            Ok(()) => {
                *last = s.generation;
                Ok(())
            }
            Err(e) => {
                drop(last);
                self.inner.lock().dirty = true;
                Err(e)
            }
        }
    }

    /// A finished block, uncompressed: from the hot few, or read once and
    /// kept. A block that would not read is not kept, and is tried again.
    fn block(&self, n: u32) -> io::Result<Arc<Vec<u8>>> {
        let slot = {
            let mut hot = self.hot.lock();
            match hot.iter().find(|(id, _)| *id == n) {
                Some((_, slot)) => Arc::clone(slot),
                None => {
                    if hot.len() >= HOT {
                        hot.remove(0);
                    }
                    let slot = Slot::default();
                    hot.push((n, Arc::clone(&slot)));
                    slot
                }
            }
        };
        let read = slot.get_or_init(|| {
            #[cfg(test)]
            self.reads
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            block::read(&self.dir, n)
                .map(Arc::new)
                .map_err(|e| e.kind())
        });
        match read {
            Ok(text) => Ok(Arc::clone(text)),
            Err(kind) => {
                self.hot.lock().retain(|(_, s)| !Arc::ptr_eq(s, &slot));
                Err((*kind).into())
            }
        }
    }
}

/// Checked by simdutf8: `std`'s check, at 1.1 GB/s on Turkish text, was over
/// half of what reading every kept text took (28 Nov).
fn text_at(block: &[u8], row: &Row) -> Option<String> {
    let (from, len) = (row.offset as usize, row.len as usize);
    let bytes = block.get(from..from.checked_add(len)?)?;
    simdutf8::basic::from_utf8(bytes).ok().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(i: usize) -> Key {
        Key {
            path: format!("/d/{i:03}.pdf"),
            size: 1,
            mtime: 1,
        }
    }

    /// The paths of the kept texts, in the order they lie in the blocks.
    fn laid_out(store: &Store) -> Vec<String> {
        let inner = store.shared.inner.lock();
        let mut rows: Vec<(&String, &Row)> = inner
            .table
            .iter()
            .filter(|(_, r)| r.state == State::Words)
            .collect();
        rows.sort_by_key(|(_, r)| (r.block, r.offset));
        rows.into_iter().map(|(p, _)| p.clone()).collect()
    }

    #[test]
    fn a_compaction_keeps_the_texts_in_the_order_they_were_written() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), 0).unwrap();
        let text =
            |i: usize, round: u8| format!("{round} {i:03} ") + &"Asliye Hukuk ".repeat(1_800);
        for i in 0..300 {
            store.put(&key(i), &Kept::Words(text(i, 0))).unwrap();
        }
        // Half changed, from the last back, every other one: their new texts
        // go to the end in that order, and their old ones are dead.
        for i in (0..300).rev().step_by(2) {
            store.put(&key(i), &Kept::Words(text(i, 1))).unwrap();
        }
        store.flush().unwrap();
        let before = laid_out(&store);
        assert!(store.compact().unwrap());
        assert_eq!(laid_out(&store), before);
        for i in 0..300 {
            let want = text(i, u8::from(i % 2 == 1));
            assert_eq!(store.get(&key(i), 0), Some(Kept::Words(want)));
        }
    }

    #[test]
    fn readers_that_want_the_same_block_wait_for_one_to_read_it() {
        let dir = tempfile::tempdir().unwrap();
        let text = |i: usize| format!("{i:03} ") + &"Asliye Hukuk Mahkemesi ".repeat(13_000);
        {
            let store = Store::open(dir.path(), 0).unwrap();
            for i in 0..30 {
                store.put(&key(i), &Kept::Words(text(i))).unwrap();
            }
            store.flush().unwrap();
        }
        let store = Store::open(dir.path(), 0).unwrap();
        let blocks = store.shared.inner.lock().blocks.len();
        assert!((2..=HOT).contains(&blocks), "{blocks}");
        let start = std::sync::Barrier::new(16);
        std::thread::scope(|s| {
            for _ in 0..16 {
                s.spawn(|| {
                    start.wait();
                    for i in 0..30 {
                        assert_eq!(store.get(&key(i), 0), Some(Kept::Words(text(i))));
                    }
                });
            }
        });
        let reads = store
            .shared
            .reads
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(reads, blocks);
    }

    #[test]
    fn a_text_s_place_is_where_it_was_put() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), 0).unwrap();
        for i in 0..3 {
            store
                .put(&key(i), &Kept::Words(format!("metin {i}")))
                .unwrap();
        }
        store.put(&key(3), &Kept::NoText).unwrap();
        let places: Vec<_> = (0..5).map(|i| store.place(&key(i).path)).collect();
        assert_eq!(
            places,
            [Some((0, 0)), Some((0, 7)), Some((0, 14)), None, None]
        );
    }

    #[test]
    fn readers_each_given_a_block_s_texts_decompress_it_once() {
        let dir = tempfile::tempdir().unwrap();
        let text = |i: usize| format!("{i:03} ") + &"Asliye Hukuk Mahkemesi ".repeat(13_000);
        {
            let store = Store::open(dir.path(), 0).unwrap();
            for i in 0..140 {
                store.put(&key(i), &Kept::Words(text(i))).unwrap();
            }
            store.flush().unwrap();
        }
        let store = Store::open(dir.path(), 0).unwrap();
        let mut runs: Vec<Vec<usize>> = Vec::new();
        for i in 0..140 {
            let (block, _) = store.place(&key(i).path).unwrap();
            if runs.len() <= block as usize {
                runs.resize(block as usize + 1, Vec::new());
            }
            runs[block as usize].push(i);
        }
        // More readers at once than blocks are kept hot.
        assert!(runs.len() > HOT, "{}", runs.len());
        let start = std::sync::Barrier::new(runs.len());
        std::thread::scope(|s| {
            for run in &runs {
                let (start, store) = (&start, &store);
                s.spawn(move || {
                    let keys: Vec<Key> = run.iter().map(|&i| key(i)).collect();
                    start.wait();
                    store.get_each(&keys, 0, |k, got| {
                        assert_eq!(got, Some(Kept::Words(text(run[k]))));
                    });
                });
            }
        });
        let reads = store
            .shared
            .reads
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(reads, runs.len());
    }

    #[test]
    fn a_block_that_would_not_read_is_read_again_once_it_can_be() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), 0).unwrap();
        store.put(&key(0), &Kept::Words("İcra".into())).unwrap();
        store.flush().unwrap();
        let path = block::path_of(dir.path(), 0);
        let packed = std::fs::read(&path).unwrap();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(store.get(&key(0), 0), None);
        std::fs::write(&path, packed).unwrap();
        assert_eq!(store.get(&key(0), 0), Some(Kept::Words("İcra".into())));
    }

    #[test]
    fn a_text_is_returned_only_whole_and_as_utf8() {
        let row = |offset, len| Row {
            size: 0,
            mtime: 0,
            state: State::Words,
            block: 0,
            offset,
            len,
        };
        let block = "aşk! ".as_bytes();
        assert_eq!(text_at(block, &row(0, 5)), Some("aşk!".into()));
        assert_eq!(text_at(block, &row(0, 2)), None, "ş cut in two");
        assert_eq!(text_at(block, &row(4, 9)), None, "past the end");
        assert_eq!(text_at(b"ab\xFEcd", &row(0, 5)), None);
    }
}
