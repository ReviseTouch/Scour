//! What listing an archive costs, and how many entries it gives: each file
//! given is listed as the service lists it for the preview.
//!
//!     cargo run --release -p scour-extract --example list_cost -- FILE...

fn main() {
    for path in std::env::args().skip(1) {
        let start = std::time::Instant::now();
        let listed = std::fs::File::open(&path)
            .map_err(|e| e.to_string())
            .and_then(scour_extract::listing);
        let took = start.elapsed().as_secs_f64() * 1e3;
        match listed {
            Ok(flow) => {
                let (rows, cut) = match flow.blocks.first() {
                    Some(scour_flow::Block::Sheet(s)) => (s.rows.len().saturating_sub(1), s.cut),
                    _ => (0, false),
                };
                println!(
                    "{took:>9.2} ms  {rows:>6} entries{}  {path}",
                    if cut { " (cut)" } else { "" }
                );
            }
            Err(e) => println!("{took:>9.2} ms  failed: {e}  {path}"),
        }
    }
}
