//! What one recursive watch holds in userspace.
//! Diagnostic rather than test: allocator and `/proc` readings before and after
//! `Source::watch`, the handle held until Enter so the second is the steady state.

use std::io::Read;

use scour_core::{Change, ChangeSink, ScanOptions, Source, SourceId};
use scour_source_fs::FsSource;

#[derive(Debug)]
struct Discard;

impl ChangeSink for Discard {
    fn emit(&self, _change: Change) {}
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn heap_bytes() -> (usize, usize) {
    let info = unsafe { libc::mallinfo2() };
    (info.uordblks, info.hblkhd)
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn heap_bytes() -> (usize, usize) {
    (0, 0)
}

fn report(label: &str) {
    let (arena, mapped) = heap_bytes();
    println!(
        "{label}: allocator live arena {:.1} MiB + mapped {:.1} MiB",
        arena as f64 / 1_048_576.0,
        mapped as f64 / 1_048_576.0,
    );
    if let Ok(smaps) = std::fs::read_to_string("/proc/self/smaps_rollup") {
        for line in smaps.lines().filter(|line| {
            line.starts_with("Rss:")
                || line.starts_with("Anonymous:")
                || line.starts_with("Private_Dirty:")
        }) {
            println!("  {line}");
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::args_os()
        .nth(1)
        .ok_or("usage: watch_memory <root>")?;
    let source = FsSource::new(SourceId(0), "probe", vec![root.into()]);
    report("before watch");
    let watch = source.watch(&ScanOptions::default(), Box::new(Discard))?;
    report("watch installed");
    println!("press Enter to stop");
    let _ = std::io::stdin().read(&mut [0u8]);
    watch.stop();
    report("watch dropped");
    Ok(())
}
