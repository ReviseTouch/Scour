//! The service.
//!
//! This is the only file in the workspace that names a concrete
//! implementation. `NativeIndex` and `FsSource` appear in [`wire`] and
//! nowhere else; everything above them was written against traits and cannot
//! tell what it was given. Replacing the search engine is a change to one
//! line here.

mod handle;
mod sources;
mod wire;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use scour_config::Config;
use scour_ipc::Server;

#[derive(Debug, Parser)]
#[command(name = "scourd", about = "The Scour indexing service", version)]
struct Args {
    /// Read settings from this file instead of the usual place.
    #[arg(long, value_name = "FILE")]
    config: Option<std::path::PathBuf>,
    /// Listen here instead of the platform default.
    #[arg(long, value_name = "PATH")]
    socket: Option<String>,
    /// Scan every source before serving, and exit when it is done.
    #[arg(long)]
    scan_only: bool,
    /// Report what would be done, then exit.
    #[arg(long)]
    check: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let (config, problem) = match &args.config {
        Some(p) => (Config::load_from(p)?, None),
        None => Config::load_or_default(),
    };
    if let Some(e) = &problem {
        // Not fatal: the service runs on defaults and says why, which beats
        // refusing to start because of one stray character in a settings file.
        eprintln!("scourd: {e}");
    }

    let addr = args.socket.clone().unwrap_or_else(|| config.socket());
    let engine = wire::build(&config)?;

    if args.check {
        println!("socket:  {addr}");
        println!("index:   {}", config.index.dir.display());
        for s in engine.sources() {
            println!("source:  {} {:?} {:?}", s.name, s.roots, s.caps);
        }
        let st = engine.status();
        println!("entries: {}  cold: {}", st.entries, st.cold);
        return Ok(());
    }

    // A cold index is scanned without being asked. The alternative is a
    // freshly installed service that answers every question with nothing until
    // someone discovers there is a command for it.
    let want_scan = config.scan.on_start || engine.status().cold;
    if args.scan_only {
        engine.rescan(None)?;
        // Nothing is watching in this mode and nothing should be: the process
        // exists to finish a walk and leave.
        wait_for_scan(&engine);
        engine.shutdown();
        return Ok(());
    }

    let server = Server::bind(&addr)?;
    let stop = Arc::new(AtomicBool::new(false));
    eprintln!(
        "scourd: listening on {addr} · {} sources · watching starting",
        engine.sources().len()
    );

    let engine = Arc::new(engine);

    // Watching starts on a thread of its own, and that is not tidiness.
    //
    // A recursive watch on Linux is one inotify watch per directory, installed
    // one at a time: 342,000 of them took **15.1 seconds** here, during which
    // the socket did not exist and every client — including a window spawned
    // on demand — sat waiting for a service that was already running. Nothing
    // about answering a query needs the watches to be in place, so nothing
    // waits for them. `scour status` reports the count when it lands.
    //
    // **And the baseline walk goes after them, on the same thread.** A walk is
    // a snapshot and a watch is everything after it; running the walk first
    // leaves the window between them covered by neither, which is exactly the
    // race that was found and fixed for subtrees a watcher discovers — and it
    // was still here, on the biggest walk of all. Anything that changes during
    // the walk now arrives as a queued event and is replayed after the sweep.
    // The cost is that a cold index fills a few seconds later than it used to.
    {
        let engine = Arc::clone(&engine);
        std::thread::spawn(move || {
            if let Ok(n) = engine.start_watching() {
                let skipped = engine.unwatched();
                if skipped.is_empty() {
                    eprintln!("scourd: watching {n} source(s)");
                } else {
                    // Named, not merely counted — "live updates are partial"
                    // is not something anyone can act on and a path is. But
                    // named *briefly*: one unreadable directory tree here
                    // produced 191 of them, and a log line that long is one
                    // nobody reads. The shared prefix is the useful part.
                    eprintln!(
                        "scourd: watching {n} source(s); {} subtree(s) unreadable, under {}",
                        skipped.len(),
                        common_prefix(&skipped)
                    );
                }
            }
            // Now that anything happening is being reported, find out what is
            // there. A change during this walk is queued behind it and applied
            // when it finishes.
            if want_scan && let Err(e) = engine.rescan(None) {
                eprintln!("scourd: the first walk could not start: {e}");
            }
        });
    }
    {
        // Ctrl-C has to reach the accept loop, which is blocked in `accept`.
        // Setting the flag and connecting once wakes it.
        let (stop, addr) = (Arc::clone(&stop), addr.clone());
        let engine = Arc::clone(&engine);
        let _ = ctrlc::set_handler(move || {
            stop.store(true, Ordering::Relaxed);
            engine.shutdown();
            let _ = scour_ipc::Client::connect(&addr);
        });
    }

    let handler_engine = Arc::clone(&engine);
    let handler_stop = Arc::clone(&stop);
    let wake_addr = addr.clone();
    server.serve(
        move |req| {
            if matches!(req, scour_proto::Request::Shutdown {}) {
                handler_stop.store(true, Ordering::Relaxed);
                // Setting the flag is not enough: the accept loop is blocked
                // inside `accept` and only looks at it when a connection
                // arrives. Without this the service answered `shutdown` and
                // then kept running until something else happened to connect —
                // which, on an idle machine, is never.
                //
                // From another thread, and after a moment, so this request's
                // own reply is written before the loop is torn down.
                let addr = wake_addr.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(50));
                    let _ = scour_ipc::Client::connect(&addr);
                });
            }
            handle::dispatch(&handler_engine, req)
        },
        Arc::clone(&stop),
    );
    engine.shutdown();
    Ok(())
}

/// The deepest directory every one of these paths is inside.
///
/// What makes a list of 191 skipped subtrees into one line somebody reads:
/// they were all under `~/.local/share/waydroid/data`, and that is the whole
/// of what a person needs in order to decide whether to care.
fn common_prefix(paths: &[String]) -> String {
    let Some(first) = paths.first() else {
        return String::new();
    };
    let mut best: Vec<&str> = first.split('/').collect();
    for p in &paths[1..] {
        let parts: Vec<&str> = p.split('/').collect();
        let keep = best.iter().zip(&parts).take_while(|(a, b)| a == b).count();
        best.truncate(keep);
    }
    // A prefix that reaches a file rather than its directory says less than it
    // looks like it does, so stop at the last component every path shares.
    match best.join("/") {
        p if p.is_empty() => "/".into(),
        p => p,
    }
}

fn wait_for_scan(engine: &scour_engine::Engine) {
    // Wait for it to *begin* before waiting for it to end. `rescan` queues a
    // job and returns, so a loop that starts by asking "still scanning?" is
    // told no and leaves with a third of an index — which is exactly what a
    // block-size measurement got, three times, before anyone noticed.
    let began = std::time::Instant::now();
    while !engine.status().scanning && began.elapsed() < std::time::Duration::from_secs(10) {
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let mut idle = 0;
    loop {
        std::thread::sleep(std::time::Duration::from_millis(200));
        let st = engine.status();
        if st.scanning || st.pending > 0 {
            idle = 0;
            continue;
        }
        idle += 1;
        // Two quiet ticks: a scan reports itself finished a moment before the
        // last batch has been applied.
        if idle >= 2 {
            let st = engine.status();
            eprintln!(
                "scourd: {} entries · {:.1} MB · {} ms",
                st.entries,
                st.index_bytes as f64 / 1_048_576.0,
                st.last_scan_ms
            );
            return;
        }
    }
}
