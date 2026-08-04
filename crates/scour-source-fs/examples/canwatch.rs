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

use std::time::Instant;

use notify::{RecursiveMode, Watcher};

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
        let mut watcher = match notify::recommended_watcher(|_| {}) {
            Ok(w) => w,
            Err(e) => {
                println!("{p}: no watcher at all: {e}");
                continue;
            }
        };
        match watcher.watch(std::path::Path::new(p), RecursiveMode::Recursive) {
            Ok(()) => println!("{p}: watched, {:.1?} to install", t.elapsed()),
            Err(e) => println!("{p}: REFUSED after {:.1?} — {e}", t.elapsed()),
        }
    }
}
