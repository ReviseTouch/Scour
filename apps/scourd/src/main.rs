//! The service: the only place in the workspace that names a concrete
//! implementation. `NativeIndex` and `FsSource` appear in [`wire`] and nowhere
//! else, so replacing the search engine is a change to one line here.

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

/// How many heaps glibc may keep. See [`cap_allocator_arenas`].
#[cfg(all(target_os = "linux", target_env = "gnu"))]
const ARENAS: libc::c_int = 2;

/// Cap how many heaps the allocator keeps, before any thread asks for one:
/// glibc gives a thread its own arena, eight per core, and memory freed into
/// one never returns to the kernel. Over a 743,000-entry scan, 160 arenas held
/// 164 MiB in 0.865 s and 2 held 31 MiB in 1.262 s — the second is paid once,
/// the memory every second the machine is on. Set before anything spawns, since
/// a held arena is not given back; `MALLOC_ARENA_MAX` wins over it.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn cap_allocator_arenas() {
    if std::env::var_os("MALLOC_ARENA_MAX").is_some() {
        return;
    }
    // SAFETY: a plain setter on the allocator's own parameters, called before
    // any thread but this one exists.
    unsafe {
        libc::mallopt(libc::M_ARENA_MAX, ARENAS);
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn cap_allocator_arenas() {}

fn main() -> Result<()> {
    cap_allocator_arenas();
    let args = Args::parse();
    let (config, problem) = match &args.config {
        Some(p) => (Config::load_from(p)?, None),
        None => Config::load_or_default(),
    };
    if let Some(e) = problem {
        // Fatal: falling back to the default source list drops every source
        // but the home directory, and the engine forgets the rows of a source
        // it no longer has. A service that will not start is the cheaper fault.
        return Err(e.into());
    }

    let addr = args.socket.clone().unwrap_or_else(|| config.socket());
    let engine = wire::build(&config)?;

    #[cfg(feature = "memory-trace")]
    if let Some(path) = std::env::var_os("SCOUR_MEMORY_RESCAN") {
        return trace_rescan(&engine, path.to_string_lossy().into_owned());
    }
    #[cfg(feature = "memory-trace")]
    if let Some(seconds) = std::env::var_os("SCOUR_MEMORY_IDLE_SECS") {
        let seconds = seconds.to_string_lossy().parse::<u64>().unwrap_or(60);
        return trace_idle(&engine, Duration::from_secs(seconds));
    }

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

    // A cold index is scanned without being asked, or a fresh install answers
    // every question with nothing.
    let want_scan = config.scan.on_start || engine.status().cold;
    if args.scan_only {
        engine.rescan(None)?;
        // Nothing is watching in this mode: the process finishes a walk and
        // leaves.
        wait_for_scan(&engine);
        engine.shutdown();
        return Ok(());
    }

    let server = Server::bind(&addr)?;
    let stop = Arc::new(AtomicBool::new(false));
    scour_core::note!(
        "scourd: listening on {addr} · {} sources · watching starting",
        engine.sources().len()
    );

    let engine = Arc::new(engine);

    // Watching starts on its own thread: installing 342,000 inotify watches
    // took 15.1 s here and no query needs them. The baseline walk goes after
    // them, on the same thread — walking first leaves a window covered by
    // neither the snapshot nor the watch.
    {
        let engine = Arc::clone(&engine);
        std::thread::spawn(move || {
            // One call, because the order inside it is the invariant that a
            // test can hold on to. See `Engine::cover_then_walk`.
            match engine.cover_then_walk(want_scan) {
                Ok((n, skipped)) if skipped.is_empty() => {
                    scour_core::note!("scourd: watching {n} source(s)");
                }
                Ok((n, skipped)) => {
                    // Named, so it can be acted on, but briefly: one unreadable
                    // tree produced 191 paths, and the shared prefix is the
                    // useful part of them.
                    scour_core::note!(
                        "scourd: watching {n} source(s); {} subtree(s) unreadable, under {}",
                        skipped.len(),
                        common_prefix(&skipped)
                    );
                }
                Err(e) => scour_core::note!("scourd: the first walk could not start: {e}"),
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

    // Beside the index rather than in the config file, which is hand-edited.
    // Beside *this* index, not the default one: everything the config points
    // at moves together, or `--config` is not the isolation it claims to be.
    let kept = handle::Kept::open(wire::state_dir(&config), config.clone());
    let handler_engine = Arc::clone(&engine);
    let handler_stop = Arc::clone(&stop);
    let wake_addr = addr.clone();
    server.serve(
        move |req, emit| {
            if matches!(req, scour_proto::Request::Shutdown {}) {
                handler_stop.store(true, Ordering::Relaxed);
                // The accept loop only reads the flag when a connection
                // arrives, so one is made. From another thread and after a
                // moment, so this reply is written before the tear-down.
                let addr = wake_addr.clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(50));
                    let _ = scour_ipc::Client::connect(&addr);
                });
            }
            handle::dispatch(&handler_engine, &kept, req, emit)
        },
        Arc::clone(&stop),
    );
    engine.shutdown();
    Ok(())
}

#[cfg(feature = "memory-trace")]
fn trace_rescan(engine: &scour_engine::Engine, path: String) -> Result<()> {
    engine.rescan(Some(path))?;
    let began = std::time::Instant::now();
    let after = std::env::var("SCOUR_MEMORY_AFTER_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(10);
    let mut saw_scan = false;
    let mut finished_at = None;
    println!(
        "ms rss_kb anonymous_kb swap_kb heap_arena_kb heap_mmap_kb \
         cpu_ticks scanning entries segments"
    );
    loop {
        let status = engine.status();
        let stats = engine.stats()?;
        saw_scan |= status.scanning;
        if saw_scan && !status.scanning && finished_at.is_none() {
            finished_at = Some(std::time::Instant::now());
            if std::env::var_os("SCOUR_MEMORY_TRIM").is_some() {
                #[cfg(all(target_os = "linux", target_env = "gnu"))]
                unsafe {
                    libc::malloc_trim(0);
                }
            }
        }
        let (rss, anonymous) = proc_rollup();
        let (swap, ticks) = proc_status();
        let (heap_arena, heap_mmap) = allocator_kb();
        println!(
            "{} {rss} {anonymous} {swap} {heap_arena} {heap_mmap} {ticks} {} {} {}",
            began.elapsed().as_millis(),
            status.scanning,
            status.entries,
            stats.segments,
        );
        if finished_at.is_some_and(|at| at.elapsed() >= Duration::from_secs(after)) {
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    engine.shutdown();
    Ok(())
}

#[cfg(feature = "memory-trace")]
fn trace_idle(engine: &scour_engine::Engine, duration: Duration) -> Result<()> {
    let (_, before) = proc_status();
    std::thread::sleep(duration);
    let (_, after) = proc_status();
    let ticks = after.saturating_sub(before);
    let hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as f64;
    let cpu = ticks as f64 / hz;
    println!(
        "idle {:.0} s: {ticks} CPU ticks = {:.3} s = {:.3}% of one core",
        duration.as_secs_f64(),
        cpu,
        cpu / duration.as_secs_f64() * 100.0,
    );
    engine.shutdown();
    Ok(())
}

#[cfg(all(feature = "memory-trace", target_os = "linux", target_env = "gnu"))]
fn allocator_kb() -> (usize, usize) {
    let info = unsafe { libc::mallinfo2() };
    (info.uordblks / 1024, info.hblkhd / 1024)
}

#[cfg(all(
    feature = "memory-trace",
    not(all(target_os = "linux", target_env = "gnu"))
))]
fn allocator_kb() -> (usize, usize) {
    (0, 0)
}

#[cfg(feature = "memory-trace")]
fn proc_rollup() -> (u64, u64) {
    let text = std::fs::read_to_string("/proc/self/smaps_rollup").unwrap_or_default();
    (proc_kb(&text, "Rss:"), proc_kb(&text, "Anonymous:"))
}

#[cfg(feature = "memory-trace")]
fn proc_status() -> (u64, u64) {
    let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let stat = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let ticks = stat
        .rsplit_once(") ")
        .map(|(_, fields)| fields.split_whitespace().collect::<Vec<_>>())
        .and_then(|fields| {
            let user = fields.get(11)?.parse::<u64>().ok()?;
            let system = fields.get(12)?.parse::<u64>().ok()?;
            Some(user + system)
        })
        .unwrap_or(0);
    (proc_kb(&status, "VmSwap:"), ticks)
}

#[cfg(feature = "memory-trace")]
fn proc_kb(text: &str, key: &str) -> u64 {
    text.lines()
        .find(|line| line.starts_with(key))
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// The deepest directory every one of these paths is inside: what turns a list
/// of 191 skipped subtrees into one line somebody reads.
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
    // Stop at the last component every path shares.
    match best.join("/") {
        p if p.is_empty() => "/".into(),
        p => p,
    }
}

fn wait_for_scan(engine: &scour_engine::Engine) {
    // Wait for it to begin before waiting for it to end: `rescan` queues a job
    // and returns, so "still scanning?" is answered no and the index is partial.
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
        // last batch is applied.
        if idle >= 2 {
            let st = engine.status();
            scour_core::note!(
                "scourd: {} entries · {:.1} MB · {} ms",
                st.entries,
                st.index_bytes as f64 / 1_048_576.0,
                st.last_scan_ms
            );
            return;
        }
    }
}
