//! The cache: a table in memory, the block being filled in memory, finished
//! blocks on disk, and the last few of those uncompressed.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use crate::block;
use crate::key::{Kept, Key};
use crate::table::{self, Row, State, Table};

/// A block is closed and compressed once it holds this much text.
const BLOCK: usize = 1 << 20;
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
    dir: PathBuf,
    max_disk: u64,
    inner: Mutex<Inner>,
    hot: Mutex<Vec<(u32, Arc<Vec<u8>>)>>,
}

struct Inner {
    table: Table,
    pending: Vec<u8>,
    pending_block: u32,
    /// Compressed size of each block on disk.
    blocks: HashMap<u32, u64>,
    dead: u64,
    dirty: bool,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("dir", &self.dir)
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
        Ok(Store {
            dir: dir.to_path_buf(),
            max_disk,
            inner: Mutex::new(Inner {
                table,
                pending: Vec::new(),
                pending_block: next,
                blocks,
                dead,
                dirty: false,
            }),
            hot: Mutex::new(Vec::new()),
        })
    }

    /// What is kept for this file as it is now. A text taken when the file
    /// was another size or time is not returned; nor is a failure under
    /// another `version` of the readers.
    pub fn get(&self, key: &Key, version: u32) -> Option<Kept> {
        let (row, pending) = {
            let inner = self.inner.lock();
            let row = *inner.table.get(&key.path)?;
            let same = Key {
                path: key.path.clone(),
                size: row.size,
                mtime: row.mtime,
            };
            if !same.same_file(key) {
                return None;
            }
            let pending = (row.state == State::Words && row.block == inner.pending_block)
                .then(|| text_at(&inner.pending, &row));
            (row, pending)
        };
        match row.state {
            State::NoText => Some(Kept::NoText),
            State::Failed(v) if v == version => Some(Kept::Failed { version }),
            State::Failed(_) => None,
            State::Words => {
                let text = match pending {
                    Some(text) => text?,
                    None => text_at(&self.block(row.block).ok()?, &row)?,
                };
                Some(Kept::Words(text))
            }
        }
    }

    /// Keep what this file gave, in place of anything kept for it before.
    pub fn put(&self, key: &Key, kept: &Kept) -> io::Result<()> {
        let mut inner = self.inner.lock();
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
            self.flush_locked(&mut inner)?;
        }
        Ok(())
    }

    /// Write the block being filled, and the table.
    pub fn flush(&self) -> io::Result<()> {
        let mut inner = self.inner.lock();
        self.flush_locked(&mut inner)
    }

    fn flush_locked(&self, inner: &mut Inner) -> io::Result<()> {
        if !inner.pending.is_empty() {
            let n = inner.pending_block;
            let packed = block::write(&self.dir, n, &inner.pending)?;
            inner.blocks.insert(n, packed);
            inner.pending.clear();
            inner.pending_block = n + 1;
            inner.dirty = true;
        }
        self.evict(inner);
        if inner.dirty {
            table::save(&self.dir.join("table"), &inner.table, inner.dead)?;
            inner.dirty = false;
        }
        Ok(())
    }

    /// The oldest blocks go, and their documents with them, until it fits.
    fn evict(&self, inner: &mut Inner) {
        while self.max_disk > 0 && inner.blocks.values().sum::<u64>() > self.max_disk {
            let Some(&oldest) = inner.blocks.keys().min() else {
                break;
            };
            self.drop_block(inner, oldest);
        }
    }

    fn drop_block(&self, inner: &mut Inner, n: u32) {
        inner.blocks.remove(&n);
        let _ = std::fs::remove_file(block::path_of(&self.dir, n));
        inner
            .table
            .retain(|_, row| !(row.state == State::Words && row.block == n));
        self.hot.lock().retain(|(b, _)| *b != n);
        inner.dirty = true;
    }

    /// Write every live text into new blocks when dead text is over its share.
    /// True when it did.
    pub fn compact(&self) -> io::Result<bool> {
        let mut inner = self.inner.lock();
        let live: u64 = inner.table.values().map(|r| u64::from(r.len)).sum();
        let total = live + inner.dead;
        if total < COMPACT_FLOOR || (inner.dead as f64) < DEAD_SHARE * total as f64 {
            return Ok(false);
        }
        self.flush_locked(&mut inner)?;
        let old: Vec<u32> = inner.blocks.keys().copied().collect();
        let rows: Vec<(String, Row)> = inner
            .table
            .iter()
            .filter(|(_, r)| r.state == State::Words)
            .map(|(p, r)| (p.clone(), *r))
            .collect();
        for (path, row) in rows {
            let Some(text) = self.block(row.block).ok().and_then(|b| text_at(&b, &row)) else {
                inner.table.remove(&path);
                continue;
            };
            let new = Row {
                block: inner.pending_block,
                offset: inner.pending.len() as u32,
                ..row
            };
            inner.pending.extend_from_slice(text.as_bytes());
            inner.table.insert(path, new);
            if inner.pending.len() >= BLOCK {
                self.flush_locked(&mut inner)?;
            }
        }
        inner.dirty = true;
        self.flush_locked(&mut inner)?;
        for n in old {
            inner.blocks.remove(&n);
            let _ = std::fs::remove_file(block::path_of(&self.dir, n));
        }
        inner.dead = 0;
        self.hot.lock().clear();
        Ok(true)
    }

    /// Forget everything.
    pub fn clear(&self) -> io::Result<()> {
        let mut inner = self.inner.lock();
        for n in inner.blocks.keys() {
            let _ = std::fs::remove_file(block::path_of(&self.dir, *n));
        }
        inner.blocks.clear();
        inner.table.clear();
        inner.pending.clear();
        inner.dead = 0;
        self.hot.lock().clear();
        table::save(&self.dir.join("table"), &inner.table, 0)
    }

    pub fn stats(&self) -> Stats {
        let inner = self.inner.lock();
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

impl Drop for Store {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

fn text_at(block: &[u8], row: &Row) -> Option<String> {
    let (from, len) = (row.offset as usize, row.len as usize);
    let bytes = block.get(from..from.checked_add(len)?)?;
    String::from_utf8(bytes.to_vec()).ok()
}
