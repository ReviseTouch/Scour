//! The cache: a table in memory, the block being filled in memory, full
//! blocks waiting for their compression in memory, finished blocks on disk,
//! and the last few of those uncompressed.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

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
    hot: Mutex<Vec<(u32, Arc<Vec<u8>>)>>,
    /// One compressor at a time: the background thread and a flush would
    /// otherwise write the same block through the same temporary file.
    packer: Mutex<()>,
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
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("dir", &self.shared.dir)
            .finish_non_exhaustive()
    }
}

impl Store {
    /// The cache in `dir`, made if missing; `max_disk` bytes compressed at
    /// most, the oldest blocks given up first. Zero is no limit.
    pub fn open(dir: &Path, max_disk: u64) -> io::Result<Store> {
        block::private_dir(dir)?;
        let mut blocks = HashMap::new();
        for entry in std::fs::read_dir(dir)?.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(n) = name
                .strip_suffix(".zst")
                .and_then(|n| n.parse::<u32>().ok())
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
        };
        Ok(Store {
            shared: Arc::new(Shared {
                dir: dir.to_path_buf(),
                max_disk,
                inner: Mutex::new(inner),
                hot: Mutex::new(Vec::new()),
                packer: Mutex::new(()),
            }),
        })
    }

    /// What is kept for this file as it is now. A text taken when the file
    /// was another size or time is not returned; nor is a failure under
    /// another `version` of the readers.
    pub fn get(&self, key: &Key, version: u32) -> Option<Kept> {
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
                    None => text_at(&self.shared.block(row.block).ok()?, &row)?,
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
        let snapshot: Vec<(String, Row)> = {
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
        self.shared.save(&mut inner)?;
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
        self.shared.hot.lock().clear();
        table::save(&self.shared.dir.join("table"), &inner.table, 0)
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
                let mut inner = self.inner.lock();
                return self.save(&mut inner);
            };
            let packed = block::write(&self.dir, n, &text)?;
            let mut inner = self.inner.lock();
            inner.blocks.insert(n, packed);
            inner.sealed.remove(&n);
            inner.dirty = true;
            self.save(&mut inner)?;
        }
    }

    /// Fit under the ceiling, oldest blocks first, then write the table if
    /// anything changed.
    fn save(&self, inner: &mut Inner) -> io::Result<()> {
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
        if inner.dirty {
            table::save(&self.dir.join("table"), &inner.table, inner.dead)?;
            inner.dirty = false;
        }
        Ok(())
    }

    /// A finished block, uncompressed: from the hot few, or read and kept.
    fn block(&self, n: u32) -> io::Result<Arc<Vec<u8>>> {
        if let Some((_, b)) = self.hot.lock().iter().find(|(id, _)| *id == n) {
            return Ok(Arc::clone(b));
        }
        let text = Arc::new(block::read(&self.dir, n)?);
        let mut hot = self.hot.lock();
        if hot.len() >= HOT {
            hot.remove(0);
        }
        hot.push((n, Arc::clone(&text)));
        Ok(text)
    }
}

fn text_at(block: &[u8], row: &Row) -> Option<String> {
    let (from, len) = (row.offset as usize, row.len as usize);
    let bytes = block.get(from..from.checked_add(len)?)?;
    String::from_utf8(bytes.to_vec()).ok()
}
