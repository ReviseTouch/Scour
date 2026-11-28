//! What drawing a document costs, and what comes of it: each file given is
//! read as the extractor reads it, and its blocks are counted by kind.
//!
//!     cargo run --release -p scour-doctext --example flow_cost -- [--show] FILE...
//!
//! With `--show`, each flow is printed too, a block a line: a heading's
//! level, a list's mark, a paragraph's indent, `|` between a table's cells.
//! With `--as=EXT`, every file is read as one with that extension: a way
//! to feed a reader text it was not written for, to see that it holds.

use scour_flow::Block;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let show = args.iter().any(|a| a == "--show");
    let read_as = args.iter().find_map(|a| a.strip_prefix("--as="));
    for path in args.iter().filter(|a| !a.starts_with("--")) {
        let path = std::path::Path::new(&path);
        let ext =
            read_as.unwrap_or_else(|| path.extension().and_then(|e| e.to_str()).unwrap_or(""));
        let Ok(bytes) = std::fs::read(path) else {
            println!("{}: unreadable", path.display());
            continue;
        };
        let start = std::time::Instant::now();
        // A reader that panics is named, and the others go on.
        let Ok(flow) = std::panic::catch_unwind(|| scour_doctext::flow(&bytes, ext)) else {
            println!("PANIC  {}", path.display());
            continue;
        };
        let took = start.elapsed();
        match flow {
            Ok(flow) => {
                let mut kinds = std::collections::BTreeMap::<&str, usize>::new();
                for block in &flow.blocks {
                    let kind = match block {
                        Block::Para(p) if p.heading > 0 => "heading",
                        Block::Para(_) => "para",
                        Block::Table(_) => "table",
                        Block::Picture(_) => "picture",
                        Block::Break => "break",
                        Block::Sheet(_) => "sheet",
                        Block::Slide(_) => "slide",
                    };
                    *kinds.entry(kind).or_default() += 1;
                }
                let chars: usize = flow.blocks.iter().map(words).sum();
                println!(
                    "{:>9.2} ms  {:>9} B  {:>8} chars  {:?}{}  {}",
                    took.as_secs_f64() * 1e3,
                    bytes.len(),
                    chars,
                    kinds,
                    if flow.cut { "  (cut)" } else { "" },
                    path.display()
                );
                if show {
                    for block in &flow.blocks {
                        print(block, 0);
                    }
                }
            }
            Err(e) => println!(
                "{:>9.2} ms  failed: {e}  {}",
                took.as_secs_f64() * 1e3,
                path.display()
            ),
        }
    }
}

/// A block as a line of text, `depth` slides in.
fn print(block: &Block, depth: usize) {
    let pad = "  ".repeat(depth);
    match block {
        Block::Para(p) => {
            let look: String = p
                .runs
                .iter()
                .map(|r| {
                    let mut t = r.text.clone();
                    if r.code {
                        t = format!("`{t}`");
                    }
                    if r.bold {
                        t = format!("**{t}**");
                    }
                    if r.italic {
                        t = format!("_{t}_");
                    }
                    t
                })
                .collect();
            let head = if p.heading > 0 {
                format!("H{} ", p.heading)
            } else {
                String::new()
            };
            let mark = if p.mark.is_empty() {
                String::new()
            } else {
                format!("{} ", p.mark)
            };
            println!("{pad}{:>4.1}| {head}{mark}{look}", p.indent);
        }
        Block::Table(t) => {
            for r in &t.rows {
                let cells: Vec<String> = r
                    .cells
                    .iter()
                    .map(|c| {
                        c.paras
                            .iter()
                            .map(|p| p.text())
                            .collect::<Vec<_>>()
                            .join(" / ")
                    })
                    .collect();
                println!(
                    "{pad}  T{}| {}",
                    if r.head { "h" } else { " " },
                    cells.join(" | ")
                );
            }
        }
        Block::Picture(p) => println!("{pad}  P | {} {} B", p.mime, p.data.len()),
        Block::Break => println!("{pad}  --"),
        Block::Sheet(s) => println!("{pad}  S | {} rows", s.rows.len()),
        Block::Slide(s) => {
            println!("{pad}  slide");
            for b in &s.blocks {
                print(b, depth + 1);
            }
        }
    }
}

/// The characters a block shows.
fn words(block: &Block) -> usize {
    match block {
        Block::Para(p) => p.text().chars().count(),
        Block::Table(t) => t
            .rows
            .iter()
            .flat_map(|r| &r.cells)
            .flat_map(|c| &c.paras)
            .map(|p| p.text().chars().count())
            .sum(),
        Block::Sheet(s) => s
            .rows
            .iter()
            .flat_map(|r| &r.cells)
            .map(|c| c.text.chars().count())
            .sum(),
        Block::Slide(s) => s.blocks.iter().map(words).sum(),
        Block::Picture(_) | Block::Break => 0,
    }
}
