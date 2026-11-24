//! The table: where each document's text is. Kept in memory, written whole
//! beside the blocks after every block, replaced by rename so a crash leaves
//! the old one or the new one.

use std::collections::HashMap;
use std::io::{self, Read as _};
use std::path::Path;

const MAGIC: &[u8; 6] = b"SCTC2\n";

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

pub(crate) type Table = HashMap<String, Row>;

/// The table, and how much text in the blocks no row points at any more.
pub(crate) fn load(path: &Path) -> (Table, u64) {
    let mut bytes = Vec::new();
    let read = std::fs::File::open(path).and_then(|mut f| f.read_to_end(&mut bytes));
    if read.is_err() || !bytes.starts_with(MAGIC) {
        return (Table::new(), 0);
    }
    let mut rest = &bytes[MAGIC.len()..];
    let Some(dead) = take::<8>(&mut rest).map(u64::from_le_bytes) else {
        return (Table::new(), 0);
    };
    parse(rest).map_or((Table::new(), 0), |t| (t, dead))
}

fn parse(mut b: &[u8]) -> Option<Table> {
    let mut table = Table::new();
    while !b.is_empty() {
        let len = u32::from_le_bytes(take(&mut b)?) as usize;
        let path = String::from_utf8(b.get(..len)?.to_vec()).ok()?;
        b = &b[len..];
        let size = u64::from_le_bytes(take(&mut b)?);
        let mtime = i64::from_le_bytes(take(&mut b)?);
        let [tag] = take::<1>(&mut b)?;
        let version = u32::from_le_bytes(take(&mut b)?);
        let block = u32::from_le_bytes(take(&mut b)?);
        let offset = u32::from_le_bytes(take(&mut b)?);
        let text = u32::from_le_bytes(take(&mut b)?);
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
        table.insert(path, row);
    }
    Some(table)
}

fn take<const N: usize>(b: &mut &[u8]) -> Option<[u8; N]> {
    let (head, rest) = b.split_first_chunk::<N>()?;
    *b = rest;
    Some(*head)
}

pub(crate) fn save(path: &Path, table: &Table, dead: u64) -> io::Result<()> {
    let mut out = Vec::with_capacity(64 * table.len() + MAGIC.len() + 8);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&dead.to_le_bytes());
    for (name, row) in table {
        out.extend_from_slice(&(name.len() as u32).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
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
    crate::block::write_private(path, &out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_table_reads_back_as_written_and_a_torn_one_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("table");
        let mut table = Table::new();
        let row = Row {
            size: 9,
            mtime: -3,
            state: State::Failed(7),
            block: 2,
            offset: 5,
            len: 0,
        };
        table.insert("/a/ş.pdf".into(), row);
        table.insert(
            "/b".into(),
            Row {
                state: State::Words,
                len: 4,
                ..row
            },
        );
        save(&path, &table, 11).unwrap();
        assert_eq!(load(&path), (table, 11));
        let mut torn = std::fs::read(&path).unwrap();
        torn.truncate(torn.len() - 2);
        std::fs::write(&path, torn).unwrap();
        assert!(load(&path).0.is_empty());
    }
}
