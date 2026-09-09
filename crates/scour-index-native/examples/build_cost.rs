//! Isolated directory-table and segment build costs, without filesystem I/O.
//!
//! `cargo run --release -p scour-index-native --example build_cost -- dirs 300000`
//! `cargo run --release -p scour-index-native --example build_cost -- segment 1000000`
//! RSS is the whole process peak (Linux KiB), not a live-allocation counter.

use scour_core::{Entry, EntryId, Meta, SourceId};
use scour_index_native::{DirWriter, build_sorted};
use std::time::Instant;

#[cfg(target_os = "linux")]
fn usage() -> (f64, i64) {
    let mut result = std::mem::MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage initializes the supplied rusage on success.
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, result.as_mut_ptr()) },
        0
    );
    let result = unsafe { result.assume_init() };
    let cpu = (result.ru_utime.tv_sec + result.ru_stime.tv_sec) as f64
        + (result.ru_utime.tv_usec + result.ru_stime.tv_usec) as f64 / 1_000_000.0;
    (cpu, result.ru_maxrss)
}

#[cfg(not(target_os = "linux"))]
fn usage() -> (f64, i64) {
    (0.0, 0)
}

fn parent(i: usize) -> String {
    format!(
        "/benchmark/home/projects/{i:08}/workspace/dependencies/generated/source/modules/implementation/resources"
    )
}

fn fingerprint(bytes: &[u8], state: &mut u64) {
    for &b in bytes {
        *state = (*state ^ u64::from(b)).wrapping_mul(1_099_511_628_211);
    }
}

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "dirs".into());
    let count: usize = std::env::args()
        .nth(2)
        .and_then(|n| n.parse().ok())
        .unwrap_or(300_000);
    let initial_cpu = usage().0;
    let start = Instant::now();
    let mut digest = 14_695_981_039_346_656_037u64;
    let (wall, cpu, rss, bytes) = match mode.as_str() {
        "dirs" => {
            let mut writer = DirWriter::new();
            for i in (0..count).rev() {
                let path = parent(i);
                let id = writer.intern(&path);
                assert_eq!(writer.intern(&path), id);
            }
            let (bytes, remap) = writer.finish();
            let wall = start.elapsed().as_secs_f64();
            let (cpu, rss) = usage();
            fingerprint(&bytes, &mut digest);
            for id in remap {
                fingerprint(&id.to_le_bytes(), &mut digest);
            }
            (wall, cpu, rss, bytes.len())
        }
        "segment" => {
            let bytes = build_sorted(&mut |emit| {
                for i in 0..count {
                    let path = format!("{}/{i:08}-İSTANBUL-rapor-Σημείωση.rs", parent(i / 10));
                    emit(&Entry {
                        id: EntryId::path_hash(SourceId(0), &path),
                        path,
                        is_dir: false,
                        meta: Meta {
                            mtime: 1_785_000_000,
                            size: (i % 10000) as i64,
                            ..Meta::UNKNOWN
                        },
                    });
                }
            });
            let wall = start.elapsed().as_secs_f64();
            let (cpu, rss) = usage();
            for part in [
                &bytes.names,
                &bytes.fnames,
                &bytes.cols,
                &bytes.dirs,
                &bytes.ids,
                &bytes.tri_dict,
                &bytes.tri_post,
                &bytes.alive,
                &bytes.porder,
                &bytes.norder,
                &bytes.eorder,
            ] {
                fingerprint(part, &mut digest);
            }
            (wall, cpu, rss, bytes.total())
        }
        _ => panic!("mode must be dirs or segment"),
    };
    println!(
        "mode={mode} count={count} wall_ms={:.3} cpu_ms={:.3} peak_rss_kib={rss} bytes={bytes} fingerprint={digest:016x}",
        wall * 1000.0,
        (cpu - initial_cpu) * 1000.0
    );
}
