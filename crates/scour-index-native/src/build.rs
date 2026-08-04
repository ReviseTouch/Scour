//! Turning entries into the files of a segment.
//!
//! One pass to intern directories and collect names, one sort, one pass to
//! write the columns. The sort is the whole design being established: rows come
//! out newest-first, and everything downstream — early exit, the narrow `mtime`
//! blocks — is a consequence of it.

use scour_core::{Entry, Key};

use crate::columns::{ColumnWriter, Field};
use crate::dirs::DirWriter;
use crate::ids::IdWriter;
use crate::names::NameWriter;
use crate::trigram::TrigramWriter;

/// The blobs a segment consists of.
#[derive(Debug, Default, Clone)]
pub struct SegmentBytes {
    pub names: Vec<u8>,
    /// The same names, folded once here so no query ever folds them again.
    pub fnames: Vec<u8>,
    pub cols: Vec<u8>,
    pub dirs: Vec<u8>,
    pub ids: Vec<u8>,
    /// Which blocks hold which trigrams — the filter that keeps a selective
    /// term from walking the whole segment.
    pub tri_dict: Vec<u8>,
    pub tri_post: Vec<u8>,
    pub alive: Vec<u8>,
}

impl SegmentBytes {
    pub fn total(&self) -> usize {
        self.names.len()
            + self.cols.len()
            + self.dirs.len()
            + self.ids.len()
            + self.tri_dict.len()
            + self.tri_post.len()
            + self.alive.len()
    }
}

/// Build a segment from entries, in any order.
///
/// Rows come out ordered by modification time, newest first, with the path
/// breaking ties so that two runs over the same input produce byte-identical
/// files — which is what makes a merge verifiable and a rebuild reproducible.
pub fn build(entries: &[Entry]) -> SegmentBytes {
    let mut order: Vec<&Entry> = entries.iter().collect();
    order.sort_unstable_by(|a, b| b.meta.mtime.cmp(&a.meta.mtime).then(a.path.cmp(&b.path)));
    build_sorted(&mut |emit: &mut dyn FnMut(&Entry)| {
        for e in &order {
            emit(e);
        }
    })
}

/// Build a segment from entries already in the stored order.
///
/// `pass` is invoked **twice** and must produce the same entries in the same
/// order both times: once to intern directories and collect names, once to
/// write the columns. Two passes rather than one buffer because the buffer is
/// what is being avoided — a rebuild folds every entry in the index, and
/// holding a million of them as `Entry` values costs a quarter of a gigabyte
/// for as long as it takes.
///
/// The order is not checked. Producing entries out of order does not corrupt
/// anything, it only costs the early exit: the rows will simply not be in the
/// order a search assumes they are.
pub fn build_sorted(pass: &mut dyn FnMut(&mut dyn FnMut(&Entry))) -> SegmentBytes {
    let mut dirs = DirWriter::new();
    let mut names = NameWriter::new();
    let mut tri = TrigramWriter::new();
    let mut provisional: Vec<u32> = Vec::new();
    pass(&mut |e: &Entry| {
        provisional.push(dirs.intern(e.parent()));
        let name = e.name();
        names.push(name);
        tri.push(name.as_bytes());
    });
    let (dir_bytes, remap) = dirs.finish();

    let mut cols = ColumnWriter::new();
    let mut ids = IdWriter::new();
    let mut rows = 0usize;
    pass(&mut |e: &Entry| {
        let i = rows;
        rows += 1;
        ids.push(&e.id, i as u32);
        let mut r = [0i64; Field::ALL.len()];
        r[Field::DirId.index()] = remap[provisional[i] as usize] as i64;
        r[Field::Size.index()] = e.meta.size;
        r[Field::Mtime.index()] = e.meta.mtime;
        r[Field::Ctime.index()] = e.meta.ctime;
        r[Field::Atime.index()] = e.meta.atime;
        r[Field::Mode.index()] = e.meta.mode;
        r[Field::Uid.index()] = e.meta.uid;
        r[Field::Gid.index()] = e.meta.gid;
        r[Field::Disk.index()] = e.meta.disk;
        r[Field::Items.index()] = e.meta.items;
        r[Field::Kind.index()] = e.kind().as_u8() as i64;
        r[Field::IsDir.index()] = i64::from(e.is_dir);
        r[Field::Source.index()] = e.id.source.0 as i64;
        match &e.id.key {
            Key::Inode { dev, ino } => {
                r[Field::KeyKind.index()] = 1;
                r[Field::KeyA.index()] = *dev as i64;
                r[Field::KeyB.index()] = *ino as i64;
            }
            Key::PathHash(h) => {
                r[Field::KeyKind.index()] = 2;
                r[Field::KeyA.index()] = *h as i64;
            }
            // An opaque key cannot live in two numbers. Nothing produces one
            // yet; when a cloud source does, it gets its own side arena rather
            // than a silently truncated column.
            Key::Opaque(_) => r[Field::KeyKind.index()] = 0,
        }
        cols.push(r);
    });

    let (tri_dict, tri_post) = tri.finish();
    SegmentBytes {
        fnames: names.finish_folded(),
        names: names.finish(),
        cols: cols.finish(),
        dirs: dir_bytes,
        ids: ids.finish(),
        tri_dict,
        tri_post,
        // Every row starts alive. A removal clears a bit; nothing is rewritten.
        alive: alive_bits(rows),
    }
}

/// A bitmap with exactly `rows` bits set.
///
/// The last byte is masked rather than left full, because the bits past the end
/// are not "spare" — anything that counts live rows counts them, and an index
/// of three entries then reports eight.
fn alive_bits(rows: usize) -> Vec<u8> {
    let mut alive = vec![0xffu8; rows.div_ceil(8)];
    let spare = rows % 8;
    if spare != 0
        && let Some(last) = alive.last_mut()
    {
        *last = (1u8 << spare) - 1;
    }
    alive
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::columns::ColumnBlocks;
    use crate::dirs::DirTable;
    use crate::names::NameArena;
    use crate::search::Segment;
    use crate::trigram::TrigramIndex;
    use scour_core::{EntryId, Meta, SourceId};

    fn entry(path: &str, mtime: i64) -> Entry {
        Entry {
            id: EntryId::inode(SourceId(0), 66_310, mtime as u64),
            path: path.into(),
            is_dir: false,
            meta: Meta {
                mtime,
                size: 100,
                ..Meta::UNKNOWN
            },
        }
    }

    #[test]
    fn rows_come_out_newest_first() {
        let entries = vec![
            entry("/a/old.rs", 100),
            entry("/a/new.rs", 300),
            entry("/a/mid.rs", 200),
        ];
        let b = build(&entries);
        let arena = NameArena::open(&b.names).expect("names");
        assert_eq!(arena.get(0), Some("new.rs"));
        assert_eq!(arena.get(1), Some("mid.rs"));
        assert_eq!(arena.get(2), Some("old.rs"));
    }

    #[test]
    fn building_is_reproducible() {
        // Two runs over the same input, in different order, must produce the
        // same bytes — otherwise a merge cannot be checked against a rebuild.
        let mut a = vec![
            entry("/x/b.rs", 5),
            entry("/x/a.rs", 5),
            entry("/y/c.rs", 9),
        ];
        let first = build(&a);
        a.reverse();
        let second = build(&a);
        assert_eq!(first.names, second.names);
        assert_eq!(first.cols, second.cols);
        assert_eq!(first.dirs, second.dirs);
    }

    #[test]
    fn a_row_reconstructs_into_the_entry_it_came_from() {
        let entries = vec![entry("/home/u/Projeler/main.rs", 42)];
        let b = build(&entries);
        let seg = Segment {
            names: NameArena::open(&b.names).expect("names"),
            folded: NameArena::open(&b.fnames).expect("fnames"),
            cols: ColumnBlocks::open(&b.cols).expect("cols"),
            dirs: DirTable::open(&b.dirs).expect("dirs"),
            tri: TrigramIndex::open(&b.tri_dict, &b.tri_post).expect("tri"),
            alive: &b.alive,
        };
        let got = seg.entry(0).expect("row 0");
        assert_eq!(got.path, "/home/u/Projeler/main.rs");
        assert_eq!(
            got.id, entries[0].id,
            "identity has to survive the round trip"
        );
        assert_eq!(got.meta.mtime, 42);
    }

    #[test]
    fn a_file_at_the_root_gets_a_sane_path() {
        let b = build(&[entry("/lonely.txt", 1)]);
        let seg = Segment {
            names: NameArena::open(&b.names).expect("names"),
            folded: NameArena::open(&b.fnames).expect("fnames"),
            cols: ColumnBlocks::open(&b.cols).expect("cols"),
            dirs: DirTable::open(&b.dirs).expect("dirs"),
            tri: TrigramIndex::open(&b.tri_dict, &b.tri_post).expect("tri"),
            alive: &b.alive,
        };
        assert_eq!(seg.entry(0).expect("row").path, "/lonely.txt");
    }

    #[test]
    fn an_empty_input_produces_readable_empty_files() {
        let b = build(&[]);
        assert!(NameArena::open(&b.names).expect("names").is_empty());
        assert!(ColumnBlocks::open(&b.cols).expect("cols").is_empty());
        assert!(DirTable::open(&b.dirs).expect("dirs").is_empty());
        assert!(b.alive.is_empty());
    }
}
