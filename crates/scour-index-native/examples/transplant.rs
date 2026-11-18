//! Carry an index's rows from one on-disk format to another, without a walk.
//!
//! Two builds that cannot read each other's files are compared on the same
//! rows this way: the old build dumps, the new one loads and rebuilds.
//!
//!   cargo run --release -p scour-index-native --example transplant -- dump /tmp/idx/native /tmp/rows.bin
//!   cargo run --release -p scour-index-native --example transplant -- load /tmp/rows.bin /tmp/new/native
//!   cargo run --release -p scour-index-native --example transplant -- again /tmp/rows.bin /tmp/new/native

use std::io::{BufReader, BufWriter, Read, Write};
use std::time::Instant;

use scour_core::{Change, Entry, EntryId, Index, Maintenance, Meta, SourceId};
use scour_index_native::NativeIndex;

/// Source, directory flag, ten numbers, path length.
const HEAD: usize = 4 + 1 + 80 + 4;

fn dump(index_dir: &str, file: &str) {
    let index = NativeIndex::open_or_create(std::path::Path::new(index_dir)).expect("open");
    let mut out = BufWriter::new(std::fs::File::create(file).expect("create"));
    let mut rows = 0u64;
    index
        .for_each_segment(&mut |_, seg| {
            for row in 0..seg.rows() {
                if !seg.is_alive(row) {
                    continue;
                }
                let Some(e) = seg.entry(row) else {
                    continue;
                };
                let m = &e.meta;
                let mut head = Vec::with_capacity(HEAD);
                head.extend_from_slice(&e.id.source.0.to_le_bytes());
                head.push(u8::from(e.is_dir));
                for n in [
                    m.size, m.mtime, m.ctime, m.atime, m.mode, m.uid, m.gid, m.disk, m.items,
                    m.links,
                ] {
                    head.extend_from_slice(&n.to_le_bytes());
                }
                head.extend_from_slice(&(e.path.len() as u32).to_le_bytes());
                out.write_all(&head).expect("write");
                out.write_all(e.path.as_bytes()).expect("write");
                rows += 1;
            }
        })
        .expect("segments");
    out.flush().expect("flush");
    println!("dumped {rows} rows");
}

/// `load` into an empty index and rebuild it. Into one that holds the rows
/// already: `again` inside a pass, as a walk that finds nothing changed, and
/// `replace` outside one, where every row is written and its old one retired.
fn load(file: &str, index_dir: &str, mode: &str) {
    let index = NativeIndex::open_or_create(std::path::Path::new(index_dir)).expect("open");
    if mode == "again" {
        index.begin_generation().expect("generation");
    }
    let mut input = BufReader::new(std::fs::File::open(file).expect("open dump"));
    let began = Instant::now();
    let mut rows = 0u64;
    let mut batch: Vec<Change> = Vec::new();
    let mut head = [0u8; HEAD];
    while input.read_exact(&mut head).is_ok() {
        let source = SourceId(u32::from_le_bytes(head[0..4].try_into().expect("4")));
        let num = |i: usize| i64::from_le_bytes(head[5 + i * 8..13 + i * 8].try_into().expect("8"));
        let len = u32::from_le_bytes(head[85..89].try_into().expect("4")) as usize;
        let mut path = vec![0u8; len];
        input.read_exact(&mut path).expect("path");
        let path = String::from_utf8(path).expect("utf-8");
        batch.push(Change::Upsert(Entry {
            id: EntryId::path_hash(source, &path),
            path,
            is_dir: head[4] != 0,
            meta: Meta {
                size: num(0),
                mtime: num(1),
                ctime: num(2),
                atime: num(3),
                mode: num(4),
                uid: num(5),
                gid: num(6),
                disk: num(7),
                items: num(8),
                links: num(9),
            },
        }));
        rows += 1;
        if batch.len() == 500_000 {
            index.apply(&mut batch.drain(..)).expect("apply");
            index.commit().expect("commit");
        }
    }
    index.apply(&mut batch.drain(..)).expect("apply");
    index.commit().expect("commit");
    println!("loaded {rows} rows in {:.2?}", began.elapsed());
    if mode != "load" {
        return;
    }
    let began = Instant::now();
    index.maintain(Maintenance::Rebuild).expect("rebuild");
    println!("rebuilt in {:.2?}", began.elapsed());
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["dump", index, file] => dump(index, file),
        [mode @ ("load" | "again" | "replace"), file, index] => load(file, index, mode),
        _ => {
            panic!("transplant dump <index> <file> | transplant load|again|replace <file> <index>")
        }
    }
}
