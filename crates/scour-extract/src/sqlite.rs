//! A SQLite database as a book: a sheet per table, its columns named as its
//! `CREATE TABLE` names them, its first rows in the order they are stored.
//! Read page by page from the file — a header, the schema, and as much of
//! each table's B-tree as its first rows need — so a database of any size is
//! opened in milliseconds. Nothing is written, no journal or WAL is read, and
//! no SQLite library is linked: the file format is SQLite's documented one.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

use scour_flow::{Align, Block, Flow, GridCell, GridRow, Sheet};

/// Rows shown of a table, tables shown, and pages read in all.
const ROWS: usize = 500;
const TABLES: usize = scour_flow::MOST_SHEETS;
const MOST_PAGES: usize = 8_192;
/// One value's text, and one value's bytes followed across overflow pages.
const CELL_CHARS: usize = 200;
const MOST_PAYLOAD: usize = 1 << 20;

/// The database `file`, as sheets.
pub fn database(file: File) -> Result<Flow, String> {
    let mut db = Db::open(file)?;
    let schema = db.rows(1, usize::MAX)?;
    let mut blocks = Vec::new();
    for (_, row) in schema {
        // type, name, tbl_name, rootpage, sql
        let [
            Value::Text(kind),
            Value::Text(name),
            _,
            Value::Int(root),
            sql,
            ..,
        ] = row.as_slice()
        else {
            continue;
        };
        if kind != "table" || name.starts_with("sqlite_") || *root <= 0 {
            continue;
        }
        if blocks.len() >= TABLES {
            break;
        }
        let sql = match sql {
            Value::Text(s) => s.as_str(),
            _ => "",
        };
        // A table without rowids is an index B-tree: not read here.
        if sql.to_ascii_uppercase().contains("WITHOUT ROWID") {
            continue;
        }
        let columns = columns(sql);
        let alias = rowid_alias(sql);
        let rows = db
            .rows(*root as u32, ROWS + 1)
            .unwrap_or_default()
            .into_iter()
            .map(|(rowid, mut values)| {
                // An INTEGER PRIMARY KEY is the rowid, stored as NULL.
                if let Some(v) = alias.and_then(|i| values.get_mut(i))
                    && *v == Value::Null
                {
                    *v = Value::Int(rowid);
                }
                values
            })
            .collect();
        blocks.push(Block::Sheet(sheet(name, &columns, rows)));
    }
    if blocks.is_empty() {
        return Err("no tables".into());
    }
    Ok(Flow { blocks, cut: false }.bounded())
}

/// One stored value.
#[derive(Debug, Clone, PartialEq)]
enum Value {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Blob(usize),
}

struct Db {
    file: File,
    page_size: usize,
    usable: usize,
    pages: u64,
    utf16: Option<bool>,
    cache: HashMap<u32, Vec<u8>>,
}

impl Db {
    fn open(mut file: File) -> Result<Db, String> {
        let mut head = [0u8; 100];
        file.read_exact(&mut head).map_err(|e| e.to_string())?;
        if &head[..16] != b"SQLite format 3\0" {
            return Err("not a SQLite database".into());
        }
        let page_size = match u16::from_be_bytes([head[16], head[17]]) {
            1 => 65_536,
            n if n >= 512 && n.is_power_of_two() => usize::from(n),
            _ => return Err("a page size SQLite does not write".into()),
        };
        let usable = page_size - usize::from(head[20]);
        let len = file.metadata().map_err(|e| e.to_string())?.len();
        let utf16 = match u32::from_be_bytes([head[56], head[57], head[58], head[59]]) {
            2 => Some(false),
            3 => Some(true),
            _ => None,
        };
        Ok(Db {
            file,
            page_size,
            usable,
            pages: len / page_size as u64,
            utf16,
            cache: HashMap::new(),
        })
    }

    fn page(&mut self, n: u32) -> Result<&[u8], String> {
        if n == 0 || u64::from(n) > self.pages {
            return Err("a page past the end".into());
        }
        if !self.cache.contains_key(&n) {
            if self.cache.len() >= MOST_PAGES {
                return Err("too many pages".into());
            }
            let mut buf = vec![0u8; self.page_size];
            self.file
                .seek(SeekFrom::Start(u64::from(n - 1) * self.page_size as u64))
                .map_err(|e| e.to_string())?;
            self.file.read_exact(&mut buf).map_err(|e| e.to_string())?;
            self.cache.insert(n, buf);
        }
        Ok(&self.cache[&n])
    }

    /// The first `most` rows of the table B-tree rooted at `root`, in order,
    /// each with its rowid.
    fn rows(&mut self, root: u32, most: usize) -> Result<Vec<(i64, Vec<Value>)>, String> {
        let mut out = Vec::new();
        let mut stack = vec![root];
        let mut seen = 0usize;
        // Depth first, children left to right: the order rows are stored in.
        while let Some(n) = stack.pop() {
            seen += 1;
            if out.len() >= most || seen > MOST_PAGES {
                break;
            }
            let page = self.page(n)?.to_vec();
            let at = if n == 1 { 100 } else { 0 };
            let kind = *page.get(at).ok_or("cut short")?;
            let cells = usize::from(be16(&page, at + 3)?);
            let header = if matches!(kind, 0x05 | 0x02) { 12 } else { 8 };
            let pointer = |i: usize| -> Result<usize, String> {
                Ok(usize::from(be16(&page, at + header + 2 * i)?))
            };
            match kind {
                0x05 => {
                    let mut children = Vec::with_capacity(cells + 1);
                    for i in 0..cells {
                        children.push(be32(&page, pointer(i)?)?);
                    }
                    children.push(be32(&page, at + 8)?);
                    stack.extend(children.into_iter().rev());
                }
                0x0d => {
                    for i in 0..cells {
                        if out.len() >= most {
                            break;
                        }
                        let mut p = pointer(i)?;
                        let (size, n) = varint(page.get(p..).ok_or("cut short")?);
                        p += n;
                        let (rowid, n) = varint(page.get(p..).ok_or("cut short")?);
                        p += n;
                        let payload = self.payload(&page, p, size as usize)?;
                        out.push((rowid as i64, self.record(&payload)));
                    }
                }
                _ => return Err("not a table's page".into()),
            }
        }
        Ok(out)
    }

    /// A cell's payload of `size` bytes from `at`, its overflow pages
    /// followed as far as [`MOST_PAYLOAD`].
    fn payload(&mut self, page: &[u8], at: usize, size: usize) -> Result<Vec<u8>, String> {
        let u = self.usable;
        let x = u - 35;
        let local = if size <= x {
            size
        } else {
            let m = (u - 12) * 32 / 255 - 23;
            let k = m + (size - m) % (u - 4);
            if k <= x { k } else { m }
        };
        let mut out = page.get(at..at + local).ok_or("cut short")?.to_vec();
        if local < size {
            let mut next = be32(page, at + local)?;
            while next != 0 && out.len() < size.min(MOST_PAYLOAD) {
                let over = self.page(next)?;
                let take = (u - 4).min(size - out.len());
                out.extend_from_slice(over.get(4..4 + take).ok_or("cut short")?);
                next = be32(over, 0)?;
            }
        }
        Ok(out)
    }

    /// A record's values, as its header's serial types say.
    fn record(&self, payload: &[u8]) -> Vec<Value> {
        let (header_len, mut h) = varint(payload);
        let header_len = (header_len as usize).min(payload.len());
        let mut body = header_len;
        let mut values = Vec::new();
        while h < header_len {
            let (serial, n) = varint(&payload[h..]);
            h += n.max(1);
            let len = match serial {
                0 | 8 | 9 => 0,
                1 => 1,
                2 => 2,
                3 => 3,
                4 => 4,
                5 => 6,
                6 | 7 => 8,
                n if n >= 12 => ((n - 12) / 2) as usize,
                _ => 0,
            };
            let bytes = payload.get(body..body + len).unwrap_or_default();
            body += len;
            values.push(match serial {
                0 => Value::Null,
                8 => Value::Int(0),
                9 => Value::Int(1),
                1..=6 => Value::Int(int(bytes)),
                7 => Value::Real(f64::from_be_bytes(bytes.try_into().unwrap_or([0; 8]))),
                n if n >= 13 && n % 2 == 1 => Value::Text(self.text(bytes)),
                n if n >= 12 => Value::Blob(len),
                _ => Value::Null,
            });
        }
        values
    }

    fn text(&self, bytes: &[u8]) -> String {
        match self.utf16 {
            None => String::from_utf8_lossy(bytes).into_owned(),
            Some(big) => {
                let units: Vec<u16> = bytes
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| {
                        if big {
                            u16::from_be_bytes(*c)
                        } else {
                            u16::from_le_bytes(*c)
                        }
                    })
                    .collect();
                String::from_utf16_lossy(&units)
            }
        }
    }
}

/// A big-endian integer of 1 to 8 bytes, its sign carried.
fn int(bytes: &[u8]) -> i64 {
    let mut n: i64 = if bytes.first().is_some_and(|b| b & 0x80 != 0) {
        -1
    } else {
        0
    };
    for &b in bytes {
        n = (n << 8) | i64::from(b);
    }
    n
}

/// SQLite's variable-length integer: up to nine bytes, the last whole.
fn varint(b: &[u8]) -> (u64, usize) {
    let mut n = 0u64;
    for (i, &byte) in b.iter().enumerate().take(9) {
        if i == 8 {
            return ((n << 8) | u64::from(byte), 9);
        }
        n = (n << 7) | u64::from(byte & 0x7f);
        if byte & 0x80 == 0 {
            return (n, i + 1);
        }
    }
    (n, b.len().min(9))
}

fn be16(b: &[u8], at: usize) -> Result<u16, String> {
    let s = b.get(at..at + 2).ok_or("cut short")?;
    Ok(u16::from_be_bytes([s[0], s[1]]))
}

fn be32(b: &[u8], at: usize) -> Result<u32, String> {
    let s = b.get(at..at + 4).ok_or("cut short")?;
    Ok(u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
}

/// A table's columns, as its `CREATE TABLE` names them: the first word of
/// each part between its outer parentheses that is not a constraint.
fn columns(sql: &str) -> Vec<String> {
    let Some(open) = sql.find('(') else {
        return Vec::new();
    };
    let body = &sql[open + 1..];
    let mut parts = Vec::new();
    let (mut depth, mut start, mut quote) = (0i32, 0usize, None::<char>);
    for (i, c) in body.char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'' | '`' | '[') => quote = Some(if c == '[' { ']' } else { c }),
            (None, '(') => depth += 1,
            (None, ')') if depth == 0 => {
                parts.push(&body[start..i]);
                break;
            }
            (None, ')') => depth -= 1,
            (None, ',') if depth == 0 => {
                parts.push(&body[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    parts
        .into_iter()
        .filter_map(|p| {
            let p = p.trim();
            let first = p.split_whitespace().next()?.to_ascii_uppercase();
            if matches!(
                first.as_str(),
                "PRIMARY" | "UNIQUE" | "CHECK" | "FOREIGN" | "CONSTRAINT"
            ) {
                return None;
            }
            let name = match p.chars().next()? {
                q @ ('"' | '`' | '[') => {
                    let close = if q == '[' { ']' } else { q };
                    p[1..].split(close).next().unwrap_or("").to_owned()
                }
                _ => p.split_whitespace().next().unwrap_or("").to_owned(),
            };
            Some(name)
        })
        .collect()
}

/// Which column, if any, is the rowid by another name: declared exactly
/// `INTEGER PRIMARY KEY`.
fn rowid_alias(sql: &str) -> Option<usize> {
    let upper = sql.to_ascii_uppercase();
    let parts = columns(&upper);
    let body = &upper[upper.find('(')? + 1..];
    parts.iter().position(|name| {
        body.split(',').any(|part| {
            let words: Vec<&str> = part.split_whitespace().collect();
            words.first().map(|w| w.trim_matches(['"', '`', '[', ']'])) == Some(name.as_str())
                && words.get(1) == Some(&"INTEGER")
                && words.get(2) == Some(&"PRIMARY")
                && words.get(3) == Some(&"KEY")
        })
    })
}

fn sheet(name: &str, columns: &[String], rows: Vec<Vec<Value>>) -> Sheet {
    let cut = rows.len() > ROWS;
    let width = rows
        .iter()
        .map(Vec::len)
        .max()
        .unwrap_or(0)
        .max(columns.len());
    let mut grid = vec![GridRow {
        cells: (0..width)
            .map(|i| GridCell {
                text: columns.get(i).cloned().unwrap_or_default(),
                bold: true,
                shade: Some([0xf6, 0xf8, 0xfa]),
                ..GridCell::default()
            })
            .collect(),
    }];
    let mut chars = vec![4usize; width];
    for (i, c) in columns.iter().enumerate() {
        chars[i] = chars[i].max(c.chars().count());
    }
    for row in rows.into_iter().take(ROWS) {
        let cells: Vec<GridCell> = row
            .into_iter()
            .enumerate()
            .map(|(i, v)| {
                let (text, right, quiet) = match v {
                    Value::Null => ("NULL".to_owned(), false, true),
                    Value::Int(n) => (n.to_string(), true, false),
                    Value::Real(f) => (f.to_string(), true, false),
                    Value::Text(t) => {
                        let line = t.lines().next().unwrap_or("");
                        (line.chars().take(CELL_CHARS).collect(), false, false)
                    }
                    Value::Blob(n) => (format!("BLOB {n} B"), false, true),
                };
                if let Some(c) = chars.get_mut(i) {
                    *c = (*c).max(text.chars().count().min(40));
                }
                GridCell {
                    text,
                    align: right.then_some(Align::Right),
                    color: quiet.then_some([0x8c, 0x95, 0x9f]),
                    ..GridCell::default()
                }
            })
            .collect();
        grid.push(GridRow { cells });
    }
    Sheet {
        name: name.to_owned(),
        widths: chars.iter().map(|&c| c as f32 * 0.6 + 1.0).collect(),
        rows: grid,
        frozen: true,
        cut,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tables_columns_from_its_create() {
        assert_eq!(
            columns(
                "CREATE TABLE \"kişi\" (id INTEGER PRIMARY KEY, \"ad soyad\" TEXT NOT NULL, \
                 [yaş] INT CHECK (yaş > 0), not_null TEXT DEFAULT (lower('A,B')), \
                 PRIMARY KEY (id), FOREIGN KEY (x) REFERENCES y(z))"
            ),
            ["id", "ad soyad", "yaş", "not_null"]
        );
    }

    #[test]
    fn numbers_and_lengths_as_sqlite_writes_them() {
        assert_eq!(varint(&[0x7f]), (127, 1));
        assert_eq!(varint(&[0x81, 0x00]), (128, 2));
        assert_eq!(int(&[0xff]), -1);
        assert_eq!(int(&[0x01, 0x00]), 256);
        assert_eq!(int(&[0xff, 0xff, 0xfe]), -2);
    }

    /// A database written by `sqlite3`, when it is on the machine: the test
    /// reads what SQLite wrote, rather than what this file thinks it writes.
    #[test]
    fn a_database_sqlite_wrote_reads_back() {
        let Ok(sqlite) = which("sqlite3") else { return };
        let dir = std::env::temp_dir().join(format!("scour-sqlite-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.db");
        let _ = std::fs::remove_file(&path);
        let mut long = String::new();
        for _ in 0..3000 {
            long.push_str("uzun ");
        }
        let script = format!(
            "CREATE TABLE kisi (id INTEGER PRIMARY KEY, ad TEXT, puan REAL, foto BLOB);\
             INSERT INTO kisi VALUES (1, 'Ayşe', 4.5, x'00ff'), (2, NULL, -3, NULL), (3, '{long}', 0, NULL);\
             CREATE TABLE bos (x);\
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i < 2000) \
             INSERT INTO bos SELECT i FROM n;"
        );
        let ok = std::process::Command::new(sqlite)
            .arg(&path)
            .arg(script)
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return;
        }
        let flow = database(File::open(&path).unwrap()).unwrap();
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
        let sheets: Vec<&Sheet> = flow
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Sheet(s) => Some(s),
                _ => None,
            })
            .collect();
        assert_eq!(sheets[0].name, "kisi");
        let text = |r: usize, c: usize| sheets[0].rows[r].cells[c].text.clone();
        assert_eq!([text(0, 0), text(0, 1), text(0, 2)], ["id", "ad", "puan"]);
        // An INTEGER PRIMARY KEY is the rowid: stored as NULL, shown as itself.
        assert_eq!(
            [text(1, 0), text(1, 1), text(1, 2), text(1, 3)],
            ["1", "Ayşe", "4.5", "BLOB 2 B"]
        );
        assert_eq!(text(2, 1), "NULL");
        assert_eq!(text(2, 2), "-3");
        // Spilled onto overflow pages, read back.
        assert!(text(3, 1).starts_with("uzun uzun") && text(3, 1).chars().count() == CELL_CHARS);
        // Across interior pages, in order, cut at the rows shown.
        assert_eq!(sheets[1].rows.len(), ROWS + 1);
        assert_eq!(sheets[1].rows[ROWS].cells[0].text, ROWS.to_string());
        assert!(sheets[1].cut);
    }

    fn which(name: &str) -> Result<std::path::PathBuf, ()> {
        std::env::var_os("PATH")
            .and_then(|p| {
                std::env::split_paths(&p)
                    .map(|d| d.join(name))
                    .find(|p| p.is_file())
            })
            .ok_or(())
    }
}
