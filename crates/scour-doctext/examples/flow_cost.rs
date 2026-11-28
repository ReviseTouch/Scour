//! What drawing a document costs, and what comes of it: each file given is
//! read as the extractor reads it, and its blocks are counted by kind.
//!
//!     cargo run --release -p scour-doctext --example flow_cost -- FILE...

use scour_flow::Block;

fn main() {
    for path in std::env::args().skip(1) {
        let path = std::path::Path::new(&path);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let Ok(bytes) = std::fs::read(path) else {
            println!("{}: unreadable", path.display());
            continue;
        };
        let start = std::time::Instant::now();
        let flow = scour_doctext::flow(&bytes, ext);
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
                println!(
                    "{:>9.2} ms  {:>9} B  {:?}{}  {}",
                    took.as_secs_f64() * 1e3,
                    bytes.len(),
                    kinds,
                    if flow.cut { "  (cut)" } else { "" },
                    path.display()
                );
            }
            Err(e) => println!(
                "{:>9.2} ms  failed: {e}  {}",
                took.as_secs_f64() * 1e3,
                path.display()
            ),
        }
    }
}
