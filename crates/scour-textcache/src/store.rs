//! The cache: a table in memory, the block being filled in memory, full
//! blocks waiting for their compression in memory, finished blocks on disk,
//! and the last few of those uncompressed.

use std::collections::{HashMap, HashSet};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use parking_lot::Mutex;

use crate::block;
use crate::key::{Kept, Key};
use crate::table::{self, Change, Row, State, Table};

/// A block is sealed and compressed once it holds this much text: 4 MiB
/// compresses 6.60x at level 19 where 1 MiB gives 6.06x.
const BLOCK: usize = 4 << 20;
/// Blocks kept uncompressed for the next document that is in one of them.
const HOT: usize = 8;
/// Past this share of dead text the blocks are written again.
const DEAD_SHARE: f64 = 0.30;
/// Not worth writing again below this much text in all.
const COMPACT_FLOOR: u64 = 4 << 20;
/// A block a compaction finds this much of still pointed at is kept as it
/// is: nine tenths. Only the others are written again.
const KEEP_SHARE: (u64, u64) = (9, 10);
/// A block read within this long is not given up for room. A search wider
/// than the cache would otherwise give up what it read a minute ago for what
/// it reads now, and keep nothing either way while paying for both.
const HOLD: u64 = 24 * 3600;
/// More full blocks than this waiting, and the next is compressed quickly:
/// the readers fill blocks faster than level 19 empties them.
const BEHIND: usize = 2;
/// Paths refused for want of room, remembered to count each once.
const REFUSED_PATHS: usize = 1 << 20;
/// The log is folded into a new base once it is this long and half the base.
const LOG_FLOOR: u64 = 1 << 20;
/// The log's name beside the base.
const LOG: &str = "journal";

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
    /// At its size, every block read within the last day: texts are not
    /// kept until one is older.
    pub full: bool,
    /// Documents whose text was not kept for want of room since the cache
    /// was opened, and how much text that was.
    pub refused: u64,
    pub refused_text: u64,
    /// The table in memory, about.
    pub memory: u64,
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
    /// The base and the log, one writer at a time, so the log's records land
    /// in the order they were made. Taken before `inner`, never after.
    disk: Mutex<Disk>,
    /// Blocks read from disk, for the tests.
    #[cfg(test)]
    reads: std::sync::atomic::AtomicUsize,
}

/// A hot block, read by the first reader that wants it; any other that wants
/// it meanwhile waits for that one. Each reader decompressing it for itself,
/// ten readers decompressed 6,586 blocks where one decompressed 680 (28 Nov).
type Slot = Arc<OnceLock<Result<Arc<Vec<u8>>, io::ErrorKind>>>;

#[derive(Default)]
struct Disk {
    /// The log, open for appending once something was appended.
    log: Option<std::fs::File>,
    log_len: u64,
    base_len: u64,
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
    /// Changes made and not yet in the log.
    journal: Vec<u8>,
    /// The base is written whole at the next write: after a compaction, a
    /// clearing, a log that would not write or would not read to its end.
    rewrite: bool,
    /// The base on disk, and so the log that follows it.
    generation: u64,
    /// Seconds since the epoch each block on disk was last read.
    used: HashMap<u32, u64>,
    used_changed: HashSet<u32>,
    full: bool,
    refused: HashSet<u64>,
    refused_count: u64,
    refused_text: u64,
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

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn seconds(meta: &std::fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs())
}

impl Store {
    /// The cache in `dir`, made if missing; `max_disk` bytes compressed at
    /// most, zero for no limit. At its size, the block read longest ago is
    /// given up for a new one, if that was more than a day ago; if not, new
    /// texts are not kept until one is.
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
            let _ = std::fs::remove_file(dir.join(LOG));
        }
        let (mut blocks, mut used) = (HashMap::new(), HashMap::new());
        for entry in std::fs::read_dir(dir)?.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(n) = block::is_block(&name)
                .then(|| name[..8].parse::<u32>().ok())
                .flatten()
            {
                let meta = entry.metadata();
                blocks.insert(n, meta.as_ref().map_or(0, |m| m.len()));
                // Until the log says when it was read: when it was written.
                used.insert(n, meta.as_ref().map_or(0, seconds));
            }
        }
        let base_path = dir.join("table");
        let base = table::load(&base_path);
        let base_len = std::fs::metadata(&base_path).map_or(0, |m| m.len());
        used.extend(base.used.iter().copied());
        let mut inner = Inner {
            table: base.table,
            pending: Vec::new(),
            pending_block: 0,
            sealed: HashMap::new(),
            packing: false,
            blocks,
            dead: base.dead,
            journal: Vec::new(),
            rewrite: base.older,
            generation: base.generation,
            used,
            used_changed: HashSet::new(),
            full: false,
            refused: HashSet::new(),
            refused_count: 0,
            refused_text: 0,
        };
        let mut next = base.next;
        let log_path = dir.join(LOG);
        let mut log_len = 0;
        match table::replay(&log_path, base.generation) {
            Some((changes, whole)) => {
                for change in &changes {
                    if let Change::Move(_, to) = change {
                        next = next.max(to + 1);
                    }
                    inner.apply(change);
                }
                log_len = std::fs::metadata(&log_path).map_or(0, |m| m.len());
                // A torn record would stand between the log and what is
                // appended after it.
                inner.rewrite |= !whole;
            }
            None => inner.rewrite = true,
        }
        // **No number is used twice.** A row whose block is not there was
        // written before a crash; its number stays taken, or a block written
        // under it later would answer for that row.
        let referenced = inner.table.values().map(|r| r.block + 1).max();
        let on_disk = inner.blocks.keys().map(|n| n + 1).max();
        inner.pending_block = next.max(referenced.unwrap_or(0)).max(on_disk.unwrap_or(0));
        let rows = inner.table.len();
        let blocks = &inner.blocks;
        inner
            .table
            .retain(|row| row.state != State::Words || blocks.contains_key(&row.block));
        inner.rewrite |= inner.table.len() != rows;
        let blocks = &inner.blocks;
        inner.used.retain(|n, _| blocks.contains_key(n));
        Ok(Store {
            shared: Arc::new(Shared {
                dir: dir.to_path_buf(),
                max_disk,
                inner: Mutex::new(inner),
                hot: Mutex::new(Vec::new()),
                packer: Mutex::new(()),
                disk: Mutex::new(Disk {
                    log: None,
                    log_len,
                    base_len,
                }),
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
            let mut inner = self.shared.inner.lock();
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
            if row.state == State::Words && held.is_none() {
                inner.touch(row.block);
            }
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
    /// filled it does not wait. A text is not kept while the cache is full:
    /// whatever was kept for the file before goes, and the text is counted.
    pub fn put(&self, key: &Key, kept: &Kept) -> io::Result<()> {
        let mut inner = self.shared.inner.lock();
        if let Kept::Words(text) = kept
            && (inner.full || !inner.table.has_room(&key.path))
        {
            if let Some(old) = inner.table.remove(&key.path) {
                inner.dead += u64::from(old.len);
                table::record(&mut inner.journal, &Change::Remove(key.path.clone()));
            }
            inner.refuse(&key.path, text.len());
            return Ok(());
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
        if let Some(old) = inner.table.insert(&key.path, row) {
            inner.dead += u64::from(old.len);
        }
        table::record(&mut inner.journal, &Change::Put(key.path.clone(), row));
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
    /// sealed one, and the table's changes.
    pub fn flush(&self) -> io::Result<()> {
        self.shared.inner.lock().seal();
        self.shared.pack_sealed()
    }

    /// Write again the blocks with dead text over their share, when dead text
    /// is over its share in all. True when it did. A block nine tenths of
    /// whose text is still pointed at is left as it is: the whole cache at
    /// level 19 was a quarter of an hour of a core a gigabyte. Compressing
    /// happens outside the lock, so searches go on meanwhile; a document
    /// changed in between keeps its newer text.
    pub fn compact(&self) -> io::Result<bool> {
        self.flush()?;
        let (snapshot, dead_before) = {
            let inner = self.shared.inner.lock();
            let live: u64 = inner.table.values().map(|r| u64::from(r.len)).sum();
            let total = live + inner.dead;
            if total < COMPACT_FLOOR || (inner.dead as f64) < DEAD_SHARE * total as f64 {
                return Ok(false);
            }
            let mut rows: Vec<(String, Row)> = inner
                .table
                .iter()
                .filter(|(_, r)| r.state == State::Words)
                .filter_map(|(p, r)| Some((String::from_utf8(p.to_vec()).ok()?, *r)))
                .collect();
            rows.sort_unstable_by(|(pa, a), (pb, b)| {
                (a.block, a.offset, pa).cmp(&(b.block, b.offset, pb))
            });
            (rows, inner.dead)
        };
        let old: Vec<u32> = self.shared.inner.lock().blocks.keys().copied().collect();
        let mut moved: Vec<(String, Row, Row)> = Vec::new();
        let mut written: Vec<(u32, u64)> = Vec::new();
        let mut kept_dead = 0u64;
        let (mut fill, mut next) = (Vec::new(), self.shared.reserve()?);
        for group in snapshot.chunk_by(|a, b| a.1.block == b.1.block) {
            let n = group[0].1.block;
            let Ok(text) = block::read(&self.shared.dir, n) else {
                continue;
            };
            let live: u64 = group.iter().map(|(_, r)| u64::from(r.len)).sum();
            let raw = text.len() as u64;
            if live * KEEP_SHARE.1 >= raw * KEEP_SHARE.0 {
                kept_dead += raw.saturating_sub(live);
                continue;
            }
            for (path, row) in group {
                let Some(words) = text_at(&text, row) else {
                    continue;
                };
                let new = Row {
                    block: next,
                    offset: fill.len() as u32,
                    ..*row
                };
                fill.extend_from_slice(words.as_bytes());
                moved.push((path.clone(), *row, new));
                if fill.len() >= BLOCK {
                    written.push((
                        next,
                        block::write(&self.shared.dir, next, &fill, block::FAST)?,
                    ));
                    fill.clear();
                    next = self.shared.reserve()?;
                }
            }
        }
        if !fill.is_empty() {
            written.push((
                next,
                block::write(&self.shared.dir, next, &fill, block::FAST)?,
            ));
        }
        let gone: Vec<u32>;
        {
            let mut inner = self.shared.inner.lock();
            let at = now();
            for (n, size) in written {
                inner.blocks.insert(n, size);
                inner.used.insert(n, at);
            }
            for (path, was, new) in moved {
                if inner.table.get(&path) == Some(&was) {
                    inner.table.insert(&path, new);
                    table::record(&mut inner.journal, &Change::Put(path, new));
                }
            }
            // An old block goes once no row points into it.
            let pointed: HashSet<u32> = inner
                .table
                .values()
                .filter(|r| r.state == State::Words)
                .map(|r| r.block)
                .collect();
            gone = old.into_iter().filter(|n| !pointed.contains(n)).collect();
            let mut hot = self.shared.hot.lock();
            for n in &gone {
                inner.blocks.remove(n);
                inner.used.remove(n);
                hot.retain(|(b, _)| b != n);
            }
            drop(hot);
            inner.dead = kept_dead + inner.dead.saturating_sub(dead_before);
            inner.rewrite = true;
        }
        // The files only once the table that no longer names them is on disk.
        self.shared.persist()?;
        for n in gone {
            let _ = std::fs::remove_file(block::path_of(&self.shared.dir, n));
        }
        Ok(true)
    }

    /// Forget everything.
    pub fn clear(&self) -> io::Result<()> {
        {
            let mut inner = self.shared.inner.lock();
            for n in inner.blocks.keys() {
                let _ = std::fs::remove_file(block::path_of(&self.shared.dir, *n));
            }
            inner.blocks.clear();
            inner.table.clear();
            inner.pending.clear();
            inner.sealed.clear();
            inner.dead = 0;
            inner.journal.clear();
            inner.used.clear();
            inner.used_changed.clear();
            inner.full = false;
            inner.refused.clear();
            inner.refused_count = 0;
            inner.refused_text = 0;
            inner.rewrite = true;
            self.shared.hot.lock().clear();
        }
        self.shared.persist()
    }

    pub fn stats(&self) -> Stats {
        let inner = self.shared.inner.lock();
        let mut stats = Stats {
            dead: inner.dead,
            disk: inner.blocks.values().sum(),
            full: inner.full,
            refused: inner.refused_count,
            refused_text: inner.refused_text,
            memory: inner.table.memory(),
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

    /// A block read now; written down when it was last written down a
    /// minute or more ago.
    fn touch(&mut self, block: u32) {
        let at = now();
        let was = self.used.entry(block).or_insert(0);
        if at.saturating_sub(*was) >= 60 {
            *was = at;
            self.used_changed.insert(block);
        }
    }

    /// A text not kept for want of room, counted once a path.
    fn refuse(&mut self, path: &str, len: usize) {
        let first = self.refused.len() >= REFUSED_PATHS
            || self.refused.insert(table::hash(path.as_bytes()));
        if first {
            self.refused_count += 1;
            self.refused_text += len as u64;
        }
    }

    /// A change read back from the log, done as it was done the first time.
    fn apply(&mut self, change: &Change) {
        match change {
            Change::Put(path, row) => {
                if let Some(old) = self.table.insert(path, *row) {
                    self.dead += u64::from(old.len);
                }
            }
            Change::Remove(path) => {
                if let Some(old) = self.table.remove(path) {
                    self.dead += u64::from(old.len);
                }
            }
            Change::Drop(n) => {
                self.table
                    .retain(|row| !(row.state == State::Words && row.block == *n));
                self.used.remove(n);
            }
            Change::Used(n, at) => {
                self.used.insert(*n, *at);
            }
            Change::Move(from, to) => {
                for row in self.table.values_mut() {
                    if row.block == *from {
                        row.block = *to;
                    }
                }
            }
        }
    }
}

enum Work {
    Base(u64, Vec<u8>),
    Append(Vec<u8>),
}

impl Shared {
    /// A block number nothing else takes: the block being filled moves up
    /// one, its rows with it — on disk before the number is written under,
    /// or after a crash its rows would point into the new block.
    fn reserve(&self) -> io::Result<u32> {
        let n = {
            let mut inner = self.inner.lock();
            let n = inner.pending_block;
            for row in inner.table.values_mut() {
                if row.block == n {
                    row.block = n + 1;
                }
            }
            table::record(&mut inner.journal, &Change::Move(n, n + 1));
            inner.pending_block = n + 1;
            n
        };
        self.persist()?;
        Ok(n)
    }

    fn pack_in_background(self: Arc<Self>) {
        if let Err(e) = self.pack_sealed() {
            eprintln!("textcache: a block could not be written: {e}");
        }
        self.inner.lock().packing = false;
    }

    /// Every sealed block compressed and written, oldest first, the lock let
    /// go while each is compressed; then the table's changes.
    fn pack_sealed(&self) -> io::Result<()> {
        let _one = self.packer.lock();
        loop {
            let next = {
                let inner = self.inner.lock();
                let waiting = inner.sealed.len();
                inner
                    .sealed
                    .iter()
                    .map(|(n, b)| (*n, Arc::clone(b)))
                    .min_by_key(|(n, _)| *n)
                    .map(|(n, b)| (n, b, waiting))
            };
            let Some((n, text, waiting)) = next else {
                return self.persist();
            };
            let level = if waiting > BEHIND {
                block::FAST
            } else {
                block::LEVEL
            };
            let packed = block::write(&self.dir, n, &text, level)?;
            {
                let mut inner = self.inner.lock();
                inner.blocks.insert(n, packed);
                inner.used.insert(n, now());
                inner.sealed.remove(&n);
            }
            self.persist()?;
        }
    }

    /// Under the ceiling: the block read longest ago given up, block after
    /// block, while one was read more than [`HOLD`] ago. Else full: texts are
    /// not kept until one is.
    fn fit(&self, inner: &mut Inner) {
        let at = now();
        loop {
            if self.max_disk == 0 || inner.blocks.values().sum::<u64>() <= self.max_disk {
                inner.full = false;
                return;
            }
            let longest = inner
                .blocks
                .keys()
                .map(|&n| (inner.used.get(&n).copied().unwrap_or(0), n))
                .min();
            let Some((_, n)) = longest.filter(|(read, _)| at.saturating_sub(*read) >= HOLD) else {
                inner.full = true;
                return;
            };
            inner.blocks.remove(&n);
            inner.used.remove(&n);
            inner.used_changed.remove(&n);
            let _ = std::fs::remove_file(block::path_of(&self.dir, n));
            inner
                .table
                .retain(|row| !(row.state == State::Words && row.block == n));
            self.hot.lock().retain(|(b, _)| *b != n);
            table::record(&mut inner.journal, &Change::Drop(n));
        }
    }

    /// The table's changes on disk: appended to the log, or the base written
    /// whole when the log has grown to half of it, or a rewrite is due.
    fn persist(&self) -> io::Result<()> {
        let mut disk = self.disk.lock();
        let work = {
            let mut inner = self.inner.lock();
            self.fit(&mut inner);
            let mut changed: Vec<u32> = inner.used_changed.drain().collect();
            changed.sort_unstable();
            for n in changed {
                if let Some(&at) = inner.used.get(&n) {
                    table::record(&mut inner.journal, &Change::Used(n, at));
                }
            }
            let long = disk.log_len + inner.journal.len() as u64 > LOG_FLOOR.max(disk.base_len / 2);
            if inner.rewrite || long {
                inner.generation += 1;
                inner.rewrite = false;
                inner.journal.clear();
                let mut used: Vec<(u32, u64)> = inner.used.iter().map(|(&n, &t)| (n, t)).collect();
                used.sort_unstable();
                let bytes = table::encode(
                    &inner.table,
                    inner.dead,
                    inner.generation,
                    inner.pending_block,
                    &used,
                );
                Work::Base(inner.generation, bytes)
            } else if inner.journal.is_empty() {
                return Ok(());
            } else {
                Work::Append(std::mem::take(&mut inner.journal))
            }
        };
        let done = match &work {
            Work::Base(generation, bytes) => self.write_base(&mut disk, *generation, bytes),
            Work::Append(bytes) => self.append(&mut disk, bytes),
        };
        if done.is_err() {
            // What is on disk is the old base and a log that may end anywhere.
            disk.log = None;
            self.inner.lock().rewrite = true;
        }
        done
    }

    /// The base, then an empty log under its generation. A crash between the
    /// two leaves the new base and the old log, which is another base's and
    /// is not read.
    fn write_base(&self, disk: &mut Disk, generation: u64, bytes: &[u8]) -> io::Result<()> {
        disk.log = None;
        block::write_private(&self.dir.join("table"), bytes)?;
        let head = table::log_head(generation);
        block::write_private(&self.dir.join(LOG), &head)?;
        disk.log_len = head.len() as u64;
        disk.base_len = bytes.len() as u64;
        Ok(())
    }

    fn append(&self, disk: &mut Disk, bytes: &[u8]) -> io::Result<()> {
        if disk.log.is_none() {
            disk.log = Some(
                std::fs::File::options()
                    .append(true)
                    .open(self.dir.join(LOG))?,
            );
        }
        let log = disk.log.as_mut().expect("just opened");
        log.write_all(bytes)?;
        log.sync_data()?;
        disk.log_len += bytes.len() as u64;
        Ok(())
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

    /// About 23 KB, the number first so each is its own.
    fn text(i: usize, round: u8) -> String {
        format!("{round} {i:03} ") + &"Asliye Hukuk ".repeat(1_800)
    }

    fn base_bytes(dir: &Path) -> Vec<u8> {
        std::fs::read(dir.join("table")).unwrap()
    }

    #[test]
    fn a_compaction_writes_again_only_the_blocks_with_dead_text_and_loses_none() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), 0).unwrap();
        for i in 0..300 {
            store.put(&key(i), &Kept::Words(text(i, 0))).unwrap();
        }
        store.flush().unwrap();
        let first = store.place(&key(0).path).unwrap().0;
        let second = store.place(&key(299).path).unwrap().0;
        assert_ne!(first, second);
        let untouched = std::fs::read(block::path_of(dir.path(), second)).unwrap();
        // Most of the first block's documents changed: their old texts dead.
        let changed: Vec<usize> = (0..300)
            .filter(|&i| store.place(&key(i).path).unwrap().0 == first)
            .skip(20)
            .collect();
        for &i in &changed {
            store.put(&key(i), &Kept::Words(text(i, 1))).unwrap();
        }
        store.flush().unwrap();
        assert!(store.compact().unwrap());
        // The first block written again elsewhere and gone; the second as it was.
        assert!(!block::path_of(dir.path(), first).exists());
        assert_eq!(
            std::fs::read(block::path_of(dir.path(), second)).unwrap(),
            untouched
        );
        assert_eq!(store.stats().dead, 0);
        drop(store);
        let again = Store::open(dir.path(), 0).unwrap();
        for i in 0..300 {
            let want = text(i, u8::from(changed.contains(&i)));
            assert_eq!(again.get(&key(i), 0), Some(Kept::Words(want)), "{i}");
        }
    }

    #[test]
    fn blocks_after_the_first_are_written_to_the_log_not_to_the_base() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), 0).unwrap();
        for i in 0..200 {
            store.put(&key(i), &Kept::Words(text(i, 0))).unwrap();
        }
        store.flush().unwrap();
        let base = base_bytes(dir.path());
        let log = std::fs::metadata(dir.path().join(LOG)).unwrap().len();
        for i in 200..400 {
            store.put(&key(i), &Kept::Words(text(i, 0))).unwrap();
        }
        store.flush().unwrap();
        assert_eq!(base_bytes(dir.path()), base, "the base was written again");
        assert!(std::fs::metadata(dir.path().join(LOG)).unwrap().len() > log);
        drop(store);
        let again = Store::open(dir.path(), 0).unwrap();
        for i in 0..400 {
            assert_eq!(again.get(&key(i), 0), Some(Kept::Words(text(i, 0))));
        }
    }

    #[test]
    fn a_full_cache_keeps_no_new_text_until_a_block_is_a_day_unread() {
        let dir = tempfile::tempdir().unwrap();
        // Text that does not compress, a block at a time: three blocks fit.
        let mut seed = 7u64;
        let mut noise = |n: usize| -> String {
            (0..n)
                .map(|_| {
                    seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                    (b'a' + (seed >> 59) as u8 % 26) as char
                })
                .collect()
        };
        let store = Store::open(dir.path(), 9 << 20).unwrap();
        let mut filled = 0;
        // Bounded: a cache that trades what it has for what comes is never full.
        while !store.stats().full && filled < 12 {
            store.put(&key(filled), &Kept::Words(noise(BLOCK))).unwrap();
            store.flush().unwrap();
            filled += 1;
        }
        let stats = store.stats();
        assert!(stats.full, "{stats:?}");
        assert!(stats.disk > 9 << 20, "nothing given up: {stats:?}");
        let (kept, refused) = (stats.documents, (stats.refused, stats.refused_text));
        assert_eq!(refused, (0, 0));
        // Full: a text is not kept, is counted once, and a scan's row is.
        let extra = noise(1000);
        store.put(&key(50), &Kept::Words(extra.clone())).unwrap();
        store.put(&key(50), &Kept::Words(extra)).unwrap();
        store.put(&key(51), &Kept::NoText).unwrap();
        let stats = store.stats();
        assert_eq!(stats.documents, kept);
        assert_eq!((stats.refused, stats.refused_text), (1, 1000));
        assert_eq!(store.get(&key(51), 0), Some(Kept::NoText));
        // A day later, the block read longest ago goes first — not the oldest.
        {
            let mut inner = store.shared.inner.lock();
            let blocks: Vec<u32> = inner.blocks.keys().copied().collect();
            for n in blocks {
                inner.used.insert(n, 1_000 + u64::from(n));
            }
            inner.used.insert(0, now());
        }
        store.flush().unwrap();
        let stats = store.stats();
        assert!(!stats.full && stats.disk <= 9 << 20, "{stats:?}");
        assert!(store.get(&key(0), 0).is_some(), "read today, kept");
        assert!(
            store.get(&key(1), 0).is_none(),
            "read longest ago, given up"
        );
        store.put(&key(60), &Kept::Words("yeni".into())).unwrap();
        assert_eq!(store.get(&key(60), 0), Some(Kept::Words("yeni".into())));
    }

    #[test]
    fn a_lost_block_s_number_is_not_used_again() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = Store::open(dir.path(), 0).unwrap();
            store.put(&key(0), &Kept::Words("eski".into())).unwrap();
            store.flush().unwrap();
        }
        // As after a crash: the row on disk, its block not.
        std::fs::remove_file(block::path_of(dir.path(), 0)).unwrap();
        {
            let store = Store::open(dir.path(), 0).unwrap();
            assert_eq!(store.get(&key(0), 0), None);
            store.put(&key(1), &Kept::Words("yeni".into())).unwrap();
            assert_eq!(store.place(&key(1).path), Some((1, 0)));
            store.flush().unwrap();
        }
        let store = Store::open(dir.path(), 0).unwrap();
        assert_eq!(store.get(&key(0), 0), None);
        assert_eq!(store.get(&key(1), 0), Some(Kept::Words("yeni".into())));
    }

    #[test]
    fn a_cache_of_the_version_before_is_read_and_written_as_this_one() {
        let dir = tempfile::tempdir().unwrap();
        block::write(dir.path(), 4, b"eski metin", block::LEVEL).unwrap();
        let mut bytes = b"SCTC3\n".to_vec();
        bytes.extend_from_slice(&0u64.to_le_bytes());
        let path = key(0).path;
        bytes.extend_from_slice(&(path.len() as u32).to_le_bytes());
        bytes.extend_from_slice(path.as_bytes());
        for field in [1u64.to_le_bytes(), 1u64.to_le_bytes()] {
            bytes.extend_from_slice(&field);
        }
        bytes.push(0);
        for field in [0u32, 4, 0, 10] {
            bytes.extend_from_slice(&field.to_le_bytes());
        }
        std::fs::write(dir.path().join("table"), &bytes).unwrap();
        {
            let store = Store::open(dir.path(), 0).unwrap();
            assert_eq!(
                store.get(&key(0), 0),
                Some(Kept::Words("eski metin".into()))
            );
            store.flush().unwrap();
        }
        assert!(base_bytes(dir.path()).starts_with(b"SCTC4\n"));
        let store = Store::open(dir.path(), 0).unwrap();
        assert_eq!(
            store.get(&key(0), 0),
            Some(Kept::Words("eski metin".into()))
        );
    }

    #[test]
    fn a_torn_log_is_read_to_the_tear_and_the_base_written_again() {
        let dir = tempfile::tempdir().unwrap();
        {
            let store = Store::open(dir.path(), 0).unwrap();
            store.put(&key(0), &Kept::Words("bir".into())).unwrap();
            store.flush().unwrap();
            store.put(&key(1), &Kept::Words("iki".into())).unwrap();
            store.flush().unwrap();
        }
        let log = dir.path().join(LOG);
        let mut bytes = std::fs::read(&log).unwrap();
        bytes.extend_from_slice(&[1, 9, 9]);
        std::fs::write(&log, &bytes).unwrap();
        let before = base_bytes(dir.path());
        {
            let store = Store::open(dir.path(), 0).unwrap();
            assert!(store.get(&key(1), 0).is_some());
            store.put(&key(2), &Kept::Words("üç".into())).unwrap();
            store.flush().unwrap();
        }
        assert_ne!(base_bytes(dir.path()), before, "written again whole");
        let store = Store::open(dir.path(), 0).unwrap();
        for (i, want) in ["bir", "iki", "üç"].into_iter().enumerate() {
            assert_eq!(store.get(&key(i), 0), Some(Kept::Words(want.into())));
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
