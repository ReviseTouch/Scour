//! What a commit costs, written down when asked, and memory given back after.

use super::*;
use std::path::Path;

/// Give freed memory back to the operating system. A fold builds a whole segment
/// in memory and drops it, and glibc keeps the freed arena in its own pools
/// rather than returning it.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub(super) fn trim_allocator() {
    unsafe extern "C" {
        fn malloc_trim(pad: usize) -> i32;
    }
    unsafe {
        malloc_trim(0);
    }
}

/// The process heap, which Rust allocates from on Windows, keeps freed blocks
/// committed; compacting it decommits what is free.
#[cfg(windows)]
pub(super) fn trim_allocator() {
    use windows_sys::Win32::System::Memory::{GetProcessHeap, HeapCompact};
    // SAFETY: the process's own heap, which lives as long as the process.
    unsafe {
        HeapCompact(GetProcessHeap(), 0);
    }
}

#[cfg(not(any(all(target_os = "linux", target_env = "gnu"), windows)))]
pub(super) fn trim_allocator() {}

pub(super) fn trim_builder_allocator() {
    #[cfg(feature = "memory-trace")]
    if std::env::var_os("SCOUR_NO_BUILDER_TRIM").is_some() {
        return;
    }
    trim_allocator();
}

#[cfg(feature = "memory-trace")]
fn entry_storage(entries: &Vec<Entry>) -> usize {
    entries.capacity() * std::mem::size_of::<Entry>()
        + entries
            .iter()
            .map(|entry| entry.path.capacity())
            .sum::<usize>()
}

#[cfg(feature = "memory-trace")]
fn segment_storage(bytes: &crate::build::SegmentBytes) -> usize {
    bytes.spell.capacity()
        + bytes.fnames.capacity()
        + bytes.cols.capacity()
        + bytes.dirs.capacity()
        + bytes.ids.capacity()
        + bytes.tri_dict.capacity()
        + bytes.tri_post.capacity()
        + bytes.alive.capacity()
        + bytes.porder.capacity()
        + bytes.norder.capacity()
        + bytes.eorder.capacity()
}

#[cfg(feature = "memory-trace")]
#[derive(Clone, Copy)]
struct CommitStamp {
    wall: Instant,
    cpu: std::time::Duration,
}

#[cfg(feature = "memory-trace")]
impl CommitStamp {
    fn now() -> CommitStamp {
        CommitStamp {
            wall: Instant::now(),
            cpu: thread_cpu(),
        }
    }

    fn since(self, earlier: CommitStamp) -> (f64, f64) {
        (
            self.wall.duration_since(earlier.wall).as_secs_f64() * 1_000.0,
            self.cpu.saturating_sub(earlier.cpu).as_secs_f64() * 1_000.0,
        )
    }
}

#[cfg(all(feature = "memory-trace", target_os = "linux"))]
fn thread_cpu() -> std::time::Duration {
    let mut time = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) } == 0 {
        std::time::Duration::new(time.tv_sec as u64, time.tv_nsec as u32)
    } else {
        std::time::Duration::ZERO
    }
}

#[cfg(all(feature = "memory-trace", not(target_os = "linux")))]
fn thread_cpu() -> std::time::Duration {
    std::time::Duration::ZERO
}

#[cfg(feature = "memory-trace")]
fn trace_commit(
    rows: usize,
    start: CommitStamp,
    settled: CommitStamp,
    prepared: CommitStamp,
    alive: CommitStamp,
    built: CommitStamp,
    written: CommitStamp,
    published: CommitStamp,
) {
    if std::env::var_os("SCOUR_COMMIT_TRACE").is_none() {
        return;
    }
    let phase = |a: CommitStamp, b: CommitStamp| {
        let (wall, cpu) = b.since(a);
        format!("{wall:.2}/{cpu:.2}")
    };
    eprintln!(
        "scourd: commit {rows} rows, wall/CPU ms: settle {} prepare {} alive {} build {} files {} manifest {} total {}",
        phase(start, settled),
        phase(settled, prepared),
        phase(prepared, alive),
        phase(alive, built),
        phase(built, written),
        phase(written, published),
        phase(start, published),
    );
}

/// Remove segment files the manifest does not name. A crash between the first of
/// a segment's files and the last leaves a partial set nothing will open: **182
/// orphan segments against 55 real ones**, counted by `bytes_on_disk`. Safe by
/// construction — the manifest is written before any file is unlinked.
pub(super) fn sweep_orphans(dir: &Path, meta: &Meta) {
    let named: std::collections::HashSet<u64> = meta.segments.iter().map(|s| s.number).collect();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Some(number) = name
            .strip_prefix("seg-")
            .and_then(|r| r.split('.').next())
            .and_then(|n| n.parse::<u64>().ok())
        else {
            continue;
        };
        // Only what is already behind the manifest's next number: a segment
        // being written right now is not worth racing.
        if !named.contains(&number) && number < meta.next_segment {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Close a scan's compaction cohort before ordinary changes resume, or every
/// later watcher commit keeps the scan's stamp and compaction sees body and
/// trickle as one group: four one-row commits rewrote a 100,000-row segment,
/// **477 ms of CPU**, once a minute on an idle machine.
pub(super) fn close_generation(inner: &mut Inner, generation: u64) {
    if inner.open == Some(generation) {
        inner.open = None;
        inner.generation = inner.generation.saturating_add(1);
        inner.meta_dirty = true;
    }
}

impl NativeIndex {
    /// The body of [`Index::commit`].
    pub(super) fn commit_now(&self) -> Result<()> {
        #[cfg(feature = "memory-trace")]
        let trace_start = CommitStamp::now();
        // The bookkeeping half — kills, subtree removals, the manifest — must be
        // inside the lock because it edits rows other threads read.
        let held = Instant::now();
        // Everything that was being built is on disk and in the list before this
        // returns: the engine announces a revision on the strength of it.
        self.settle()?;
        #[cfg(feature = "memory-trace")]
        let trace_settled = CommitStamp::now();
        let mut pending = self.flush_prepare(&mut self.inner.write(), false, true)?;
        #[cfg(feature = "memory-trace")]
        let trace_prepared = CommitStamp::now();
        // How long a search could have been waiting, printed rather than guessed
        // at.
        if std::env::var_os("SCOUR_LOCK_TRACE").is_some() && held.elapsed().as_millis() > 20 {
            eprintln!(
                "scourd: commit held the index for {:.0?} ({} staged)",
                held.elapsed(),
                pending.staged.as_ref().map_or(0, |(_, _, v)| v.len())
            );
        }
        // Both of the expensive halves, now that the lock is gone. **Every
        // failure from here hands the rows back**: they are out of the staging
        // buffer and this is the only copy.
        if let Err(e) = pending.write_alive(&self.dir, &self.disk_bytes) {
            Self::restore(&mut self.inner.write(), pending);
            return Err(e);
        }
        #[cfg(feature = "memory-trace")]
        let trace_alive = CommitStamp::now();
        let Some((number, generation, staged)) = pending.staged.take() else {
            #[cfg(feature = "memory-trace")]
            trace_commit(
                0,
                trace_start,
                trace_settled,
                trace_prepared,
                trace_alive,
                trace_alive,
                trace_alive,
                trace_alive,
            );
            return self.persist_unwritten();
        };
        #[cfg(feature = "memory-trace")]
        let trace_rows = staged.len();
        let bytes = build(&staged);
        #[cfg(feature = "memory-trace")]
        let trace_built = CommitStamp::now();
        let live = match self
            .disk_bytes
            .changing(|| Live::write(&self.dir, number, generation, &bytes))
        {
            Ok(live) => live,
            Err(e) => {
                pending.staged = Some((number, generation, staged));
                Self::restore(&mut self.inner.write(), pending);
                return Err(e);
            }
        };
        #[cfg(feature = "memory-trace")]
        let trace_written = CommitStamp::now();
        drop(staged);
        drop(bytes);

        let mut inner = self.inner.write();
        inner.next_segment = inner.next_segment.max(number + 1);
        inner.segments.push(live);
        inner.segments.sort_by_key(|s| s.number);
        inner.meta_dirty = true;
        let saved = self.save_meta(&mut inner);
        drop(inner);
        #[cfg(feature = "memory-trace")]
        trace_commit(
            trace_rows,
            trace_start,
            trace_settled,
            trace_prepared,
            trace_alive,
            trace_built,
            trace_written,
            CommitStamp::now(),
        );
        saved?;
        self.persist_unwritten()
    }
}
