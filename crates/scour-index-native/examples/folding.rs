//! What folding a name costs, by what is in the name.
//!
//! `cargo run --release -p scour-index-native --example folding`
//!
//! `Folded::fold_bytes` has a vectorised path for wholly ASCII names and a
//! per-character one otherwise, so one `ş` can move a whole row.

use std::time::Instant;

use scour_index_native::Folded;

/// Names shaped like the ones on the volume this was measured against.
fn corpus(turkish: bool) -> Vec<Vec<u8>> {
    let ascii = [
        "main.rs",
        "config.toml",
        "README.md",
        "annual-report-2024.pdf",
        "invoice_september_final.docx",
        "screenshot from 2026-01-14 09-32-11.png",
    ];
    let tr = [
        "değişiklik.rs",
        "yapılandırma.toml",
        "OKUBENİ.md",
        "yıllık-rapor-2024.pdf",
        "fatura_eylül_son.docx",
        "ekran görüntüsü 2026-01-14 09-32-11.png",
    ];
    let from = if turkish { &tr } else { &ascii };
    // Fifty thousand of them, so that the loop is measuring folding and not a
    // handful of cache misses.
    (0..50_000)
        .map(|i| from[i % from.len()].as_bytes().to_vec())
        .collect()
}

fn run(label: &str, names: &[Vec<u8>], rounds: usize) {
    let mut fold = Folded::new();
    // Warm, so the first round's page faults are not the measurement.
    let mut sink = 0usize;
    for n in names {
        sink += fold.fold_bytes(n).len();
    }
    let t = Instant::now();
    for _ in 0..rounds {
        for n in names {
            sink += fold.fold_bytes(n).len();
        }
    }
    let each = t.elapsed().as_nanos() as f64 / (rounds * names.len()) as f64;
    let bytes: usize = names.iter().map(Vec::len).sum::<usize>() / names.len();
    println!("  {label:22} {each:6.1} ns a name   ({bytes} bytes average)   [{sink}]");
}

fn main() {
    let rounds = 40;
    println!("folding a name, {rounds} rounds over 50,000 names\n");
    let ascii = corpus(false);
    let turkish = corpus(true);
    run("ascii", &ascii, rounds);
    run("turkish", &turkish, rounds);

    // And the mix that matters: one non-ASCII character in an otherwise English
    // name, which is the shape most real corpora have.
    let mut mixed = ascii.clone();
    for (i, n) in mixed.iter_mut().enumerate() {
        if i % 10 == 0 {
            n.extend_from_slice("ş".as_bytes());
        }
    }
    run("ascii, one in ten not", &mixed, rounds);
}
