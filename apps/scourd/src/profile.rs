//! Research only: sample the whole process's CPU and write folded stacks, one
//! file a window, so where the service spends its time can be summed offline.
//! `SCOUR_PPROF_DIR=<dir>` turns it on, `SCOUR_PPROF_SECS` sets the window.

use std::io::Write;
use std::time::Duration;

pub fn start() {
    let Some(dir) = std::env::var_os("SCOUR_PPROF_DIR") else { return };
    let dir = std::path::PathBuf::from(dir);
    let secs = std::env::var("SCOUR_PPROF_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(300u64);
    let _ = std::fs::create_dir_all(&dir);
    std::thread::Builder::new()
        .name("scour-pprof".into())
        .spawn(move || {
            for n in 0u32.. {
                let guard = match pprof::ProfilerGuardBuilder::default()
                    .frequency(199)
                    .blocklist(&["libc", "libgcc", "pthread", "vdso"])
                    .build()
                {
                    Ok(g) => g,
                    Err(e) => {
                        eprintln!("scourd: profiler: {e}");
                        return;
                    }
                };
                std::thread::sleep(Duration::from_secs(secs));
                let Ok(report) = guard.report().build() else { continue };
                drop(guard);
                let path = dir.join(format!("window-{n:03}.folded"));
                let Ok(mut f) = std::fs::File::create(&path) else { continue };
                for (frames, count) in report.data.iter() {
                    let mut line = frames.thread_name.clone();
                    for frame in frames.frames.iter().rev() {
                        for sym in frame.iter().rev() {
                            line.push(';');
                            line.push_str(&sym.name());
                        }
                    }
                    let _ = writeln!(f, "{line} {count}");
                }
            }
        })
        .ok();
}
