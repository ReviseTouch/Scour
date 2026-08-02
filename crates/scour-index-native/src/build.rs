//! Turning entries into the four files.
//!
//! One pass to intern directories and collect names, one sort, one pass to
//! write the columns. The sort is the whole design being established: rows come
//! out newest-first, and everything downstream — early exit, the narrow `mtime`
//! blocks — is a consequence of it.

use scour_core::{Entry, Key};

use crate::columns::{ColumnWriter, Field};
use crate::dirs::DirWriter;
use crate::names::NameWriter;

/// The four blobs a segment consists of.
#[derive(Debug, Default, Clone)]
pub struct SegmentBytes {
    pub names: Vec<u8>,
    pub cols: Vec<u8>,
    pub dirs: Vec<u8>,
    pub alive: Vec<u8>,
}

impl SegmentBytes {
    pub fn total(&self) -> usize {
        self.names.len() + self.cols.len() + self.dirs.len() + self.alive.len()
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

    let mut dirs = DirWriter::new();
    let mut names = NameWriter::new();
    let mut provisional = Vec::with_capacity(order.len());
    for e in &order {
        provisional.push(dirs.intern(e.parent()));
        names.push(e.name());
    }
    let (dir_bytes, remap) = dirs.finish();

    let mut cols = ColumnWriter::new();
    for (i, e) in order.iter().enumerate() {
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
    }

    SegmentBytes {
        names: names.finish(),
        cols: cols.finish(),
        dirs: dir_bytes,
        // Every row starts alive. A removal clears a bit; nothing is rewritten.
        alive: vec![0xff; order.len().div_ceil(8)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::columns::ColumnBlocks;
    use crate::dirs::DirTable;
    use crate::names::NameArena;
    use crate::search::Segment;
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
            cols: ColumnBlocks::open(&b.cols).expect("cols"),
            dirs: DirTable::open(&b.dirs).expect("dirs"),
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
            cols: ColumnBlocks::open(&b.cols).expect("cols"),
            dirs: DirTable::open(&b.dirs).expect("dirs"),
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
