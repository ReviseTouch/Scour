//! CPU and wall time for one native-index commit.
//!
//! Pass an index, a source id and paths already in it: replacing existing rows
//! exercises both fixed parts of a desktop commit, publishing a segment and
//! rewriting the touched alive bitmap, without timing startup or opening.

use std::time::{Duration, Instant};

use scour_core::{Change, Entry, EntryId, Index, Meta, SourceId};
use scour_index_native::NativeIndex;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args();
    let _program = args.next();
    let dir = args
        .next()
        .ok_or("usage: commit_cost <index> <source-id> <path>...")?;
    let source = SourceId(args.next().ok_or("missing source id")?.parse::<u32>()?);
    let paths: Vec<String> = args.collect();
    if paths.is_empty() {
        return Err("at least one path is required".into());
    }

    let index = NativeIndex::open_or_create(std::path::Path::new(&dir))?;
    let changes: Vec<Change> = paths
        .iter()
        .map(|path| entry(source, path).map(Change::Upsert))
        .collect::<Result<_, _>>()?;

    let wall = Instant::now();
    let cpu = thread_cpu();
    index.apply(&mut changes.into_iter())?;
    let apply_wall = wall.elapsed();
    let apply_cpu = thread_cpu().saturating_sub(cpu);

    let wall = Instant::now();
    let cpu = thread_cpu();
    index.commit()?;
    let commit_wall = wall.elapsed();
    let commit_cpu = thread_cpu().saturating_sub(cpu);
    println!(
        "rows {} · apply {:.3} ms wall / {:.3} ms CPU · commit {:.3} ms wall / {:.3} ms CPU",
        paths.len(),
        millis(apply_wall),
        millis(apply_cpu),
        millis(commit_wall),
        millis(commit_cpu),
    );
    Ok(())
}

fn entry(source: SourceId, path: &str) -> std::io::Result<Entry> {
    let metadata = std::fs::symlink_metadata(path)?;
    let is_dir = metadata.is_dir();
    Ok(Entry {
        id: EntryId::path_hash(source, path),
        path: path.to_owned(),
        is_dir,
        meta: Meta::from_std(&metadata, is_dir),
    })
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

#[cfg(target_os = "linux")]
fn thread_cpu() -> Duration {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) } == 0 {
        Duration::new(time.tv_sec as u64, time.tv_nsec as u32)
    } else {
        Duration::ZERO
    }
}

#[cfg(not(target_os = "linux"))]
fn thread_cpu() -> Duration {
    Duration::ZERO
}
