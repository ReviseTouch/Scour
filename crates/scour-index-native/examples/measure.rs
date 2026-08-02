//! What the layout actually costs, on a tree shaped like a real one.
//!
//! `cargo run --release -p scour-index-native --example measure`
//!
//! Not a test — it prints bytes per entry so that a claim in the module docs
//! can be checked rather than repeated.

use scour_index_native::{ColumnWriter, DirWriter, Field};
use scour_mock::{MockOptions, generate};

fn main() {
    let fs = generate(&MockOptions {
        files: 500_000,
        ..Default::default()
    });
    let n = fs.entries.len();

    // Rows newest-first: the order the whole design rests on.
    let mut order: Vec<&scour_core::Entry> = fs.entries.iter().collect();
    order.sort_unstable_by(|a, b| b.meta.mtime.cmp(&a.meta.mtime).then(a.path.cmp(&b.path)));

    let mut dirs = DirWriter::new();
    let mut raw_paths = 0usize;
    let mut names = Vec::new();
    let mut provisional = Vec::with_capacity(n);
    for e in &order {
        raw_paths += e.path.len();
        provisional.push(dirs.intern(e.parent()));
        names.extend_from_slice(e.name().as_bytes());
        names.push(0);
    }
    let n_dirs = dirs.len();
    let (dir_bytes, remap) = dirs.finish();

    let mut cols = ColumnWriter::new();
    for (i, e) in order.iter().enumerate() {
        let mut r = [0i64; 16];
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
        match &e.id.key {
            scour_core::Key::Inode { dev, ino } => {
                r[Field::KeyKind.index()] = 1;
                r[Field::KeyA.index()] = *dev as i64;
                r[Field::KeyB.index()] = *ino as i64;
            }
            _ => r[Field::KeyKind.index()] = 2,
        }
        r[Field::Source.index()] = e.id.source.0 as i64;
        cols.push(r);
    }
    let col_bytes = cols.finish();
    let alive = n.div_ceil(8);
    let total = dir_bytes.len() + col_bytes.len() + names.len() + alive;
    let per = |b: usize| b as f64 / n as f64;

    println!("{n} entries in {n_dirs} directories\n");
    println!("  {:<26}{:>11}{:>12}", "", "bytes", "per entry");
    for (what, b) in [
        ("raw paths (not stored)", raw_paths),
        ("dirs.dat", dir_bytes.len()),
        ("cols.dat (16 numbers)", col_bytes.len()),
        ("names.dat (uncompressed)", names.len()),
        ("alive.bits", alive),
        ("TOTAL", total),
    ] {
        println!("  {what:<26}{b:>11}{:>12.2}", per(b));
    }
    println!("\n  measured elsewhere: SQLite 548 B/entry, tantivy 181 B/entry");
    println!(
        "  at 10M entries this layout is {:.0} MB",
        per(total) * 10e6 / 1_048_576.0
    );
}
