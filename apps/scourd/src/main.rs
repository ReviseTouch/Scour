//! The service.
//!
//! This is the only file in the workspace that names a concrete
//! implementation. `TantivyIndex` and `FsSource` appear in [`wire`] and
//! nowhere else; everything above them was written against traits and cannot
//! tell what it was given. Replacing the search engine is a change to one
//! line here.

mod handle;
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

    let watching = engine.start_watching().unwrap_or(0);
    // A cold index is scanned without being asked. The alternative is a
    // freshly installed service that answers every question with nothing until
    // someone discovers there is a command for it.
    if config.scan.on_start || engine.status().cold {
        engine.rescan(None)?;
    }
    if args.scan_only {
        wait_for_scan(&engine);
        engine.shutdown();
        return Ok(());
    }

    let server = Server::bind(&addr)?;
    let stop = Arc::new(AtomicBool::new(false));
    eprintln!(
        "scourd: listening on {addr} · {} sources · watching {watching}",
        engine.sources().len()
    );

    let engine = Arc::new(engine);
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

fn wait_for_scan(engine: &scour_engine::Engine) {
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
