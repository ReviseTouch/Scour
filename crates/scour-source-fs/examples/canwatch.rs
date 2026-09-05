//! Can this directory be watched, and if not, what does the system say?
//!
//! `cargo run --release -p scour-source-fs --example canwatch <path>...`
//!
//! Exists because the answer was being thrown away twice. `watch::start`
//! returns "no root could be watched" without the reason, `start_watching`
//! discards even that, and the status line then reports `watching 0` — which
//! tells a user that live updates are off and nothing about why, or whether it
//! is something they can change.
//!
//! Prints how long the watch took to install as well as whether it worked: on
//! Linux a recursive watch is one inotify watch per directory, so the cost is
//! proportional to the tree and is worth knowing before it is paid at start-up.
//!
//! And then it **proves the watch works**, which is a different question from
//! whether it was accepted. `watch()` returning `Ok` means the kernel took the
//! request; it does not mean events arrive. Network mounts, FUSE, and anything
//! whose changes happen on the far side of the wire accept a watch and report
//! nothing — so this writes a file under the path and waits to be told about
//! it. A volume that answers `watched` and then `no events` is one that has to
//! be rescanned on a timer instead.

//! **On Linux this asks a question the program no longer has.** The inotify
//! fallback is gone — one watch a directory out of a budget shared with the
//! whole session was what stopped other programs from starting, twice — so
//! there is one mechanism here now and it is a fanotify mark. Whether *that*
//! works is answered by `scour features` and by `scour-watch` itself, not by
//! installing a recursive watch and timing it. The measurement below is kept
//! for the platforms that still reach the kernel through `notify`.

#[cfg(not(target_os = "linux"))]
use std::sync::mpsc;
#[cfg(not(target_os = "linux"))]
use std::time::{Duration, Instant};

#[cfg(not(target_os = "linux"))]
use notify::{RecursiveMode, Watcher};

#[cfg(not(target_os = "linux"))]
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths = if args.is_empty() {
        vec![std::env::var("HOME").unwrap_or_else(|_| "/".into())]
    } else {
        args
    };

    #[cfg(target_os = "linux")]
    {
        let read = |p: &str| {
            std::fs::read_to_string(p)
                .unwrap_or_default()
                .trim()
                .to_owned()
        };
        println!(
            "inotify: max_user_watches={} max_user_instances={}",
            read("/proc/sys/fs/inotify/max_user_watches"),
            read("/proc/sys/fs/inotify/max_user_instances"),
        );
    }

    for p in &paths {
        let t = Instant::now();
        let (tx, rx) = mpsc::channel();
        let mut watcher = match notify::recommended_watcher(move |res| {
            let _ = tx.send(res);
        }) {
            Ok(w) => w,
            Err(e) => {
                println!("{p}: no watcher at all: {e}");
                continue;
            }
        };
        match watcher.watch(std::path::Path::new(p), RecursiveMode::Recursive) {
            Ok(()) => println!("{p}: watched, {:.1?} to install", t.elapsed()),
            Err(e) => {
                println!("{p}: REFUSED after {:.1?} — {e}", t.elapsed());
                continue;
            }
        }
        println!("   {}", proof(&rx, std::path::Path::new(p)));
    }
}

/// Write something under a watched path and see whether the watcher notices.
///
/// The file is created inside a directory of its own and both are removed
/// afterwards, so a volume being probed is left as it was found.
#[cfg(not(target_os = "linux"))]
fn proof(rx: &mpsc::Receiver<notify::Result<notify::Event>>, root: &std::path::Path) -> String {
    let dir = root.join(".scour-watch-probe");
    let file = dir.join("probe.txt");
    if std::fs::create_dir_all(&dir).is_err() {
        return "cannot test: the path is not writable from here".into();
    }
    let wrote = std::fs::write(&file, b"probe").is_ok();
    let mut seen = false;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(Ok(ev)) if ev.paths.iter().any(|q| q.starts_with(&dir)) => {
                seen = true;
                break;
            }
            Ok(_) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_dir(&dir);
    match (wrote, seen) {
        (false, _) => "cannot test: the path is not writable from here".into(),
        (true, true) => "events arrive — live updates will work here".into(),
        (true, false) => "**no events in 5s** — the watch was accepted but reports nothing; \
             this volume has to be rescanned on a timer"
            .into(),
    }
}

/// On Linux the answer is not measured here; see the note at the top.
#[cfg(target_os = "linux")]
fn main() {
    eprintln!(
        "canwatch: not the question on Linux — the only watching mechanism here is a\n\
         fanotify mark, and installing per-directory inotify watches to time them is\n\
         exactly what was removed. Ask instead:\n\
         \n\
         \x20   sudo scour-watch -- scourd\n\
         \n\
         Whether a descriptor actually arrived is said by scourd's own start-up\n\
         line on stderr, and by nothing else — no command reports it.\n"
    );
    std::process::exit(2);
}
