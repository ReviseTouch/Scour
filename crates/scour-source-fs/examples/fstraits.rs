//! What each path's filesystem promises. `cargo run -p scour-source-fs --example fstraits -- <paths…>`
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths = if args.is_empty() {
        vec!["/".into(), "/tmp".into(), "/proc".into()]
    } else {
        args
    };
    for p in paths {
        let t = scour_source_fs::fs::traits_of(std::path::Path::new(&p));
        println!(
            "{p:14} stable_ids={:<5} case_sensitive={}",
            t.stable_ids, t.case_sensitive
        );
    }
}
