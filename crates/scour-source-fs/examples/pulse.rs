//! What a source's pulse says, for a path given on the command line.
//!
//! Diagnostic rather than test: whether a pulse can be read depends on the
//! filesystem and the machine, so this asks rather than asserts.
//!     cargo run -p scour-source-fs --example pulse -- /home/hasan /mnt/depo

use scour_core::{Source, SourceId};
use scour_source_fs::FsSource;

fn main() {
    let roots: Vec<String> = std::env::args().skip(1).collect();
    if roots.is_empty() {
        eprintln!("usage: pulse <path>...");
        return;
    }
    for root in &roots {
        let src = FsSource::new(SourceId(1), "probe", vec![root.into()]);
        let first = src.pulse();
        std::thread::sleep(std::time::Duration::from_millis(200));
        let again = src.pulse();
        println!("{root}: pulse {first:?} then {again:?}");
    }
}
