//! The service: the only place in the workspace that names a concrete
//! implementation. `NativeIndex` and `FsSource` appear in [`wire`] and nowhere
//! else, so replacing the search engine is a change to one line here.

mod as_owner;
mod disks;
mod documents;
mod handle;
mod live;
mod resources;
#[cfg(windows)]
mod service;
mod sources;
mod wire;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use scour_config::Config;
use scour_ipc::Server;

#[derive(Debug, Clone, Parser)]
#[command(name = "scourd", about = "The Scour indexing service", version)]
pub(crate) struct Args {
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
    /// Run under the Windows Service Control Manager. See docs/WINDOWS-SERVICE.md.
    #[arg(long)]
    service: bool,
    /// The one account, by SID, that may talk to this service besides SYSTEM
    /// and the administrators. Windows only: the pipe's access list.
    #[arg(long, value_name = "SID")]
    owner: Option<String>,
    /// Keep the settings here rather than beside the index: the person's own
    /// folder when the service runs as SYSTEM with its index in ProgramData.
    #[arg(long, value_name = "DIR")]
    state: Option<std::path::PathBuf>,
    /// The index here rather than where the configuration says.
    #[arg(long, value_name = "DIR")]
    index: Option<std::path::PathBuf>,
}

/// What stops a running service from outside the accept loop: Ctrl-C, SIGTERM,
/// or the Service Control Manager's stop. Set once `run` is serving.
static STOP: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>> = std::sync::OnceLock::new();

/// Stop the service as Ctrl-C would. Nothing before it is serving.
pub(crate) fn stop_now() {
    if let Some(stop) = STOP.get() {
        stop();
    }
}

/// The pipe's access list for a service run as SYSTEM: full control for SYSTEM
/// and the administrators, read and write for the one owner, nobody else.
fn access_for(owner: &str) -> String {
    format!("D:P(A;;GA;;;SY)(A;;GA;;;BA)(A;;GRGW;;;{owner})")
}

/// The exit status for "another scourd has this index": `EX_TEMPFAIL`, which
/// `SuccessExitStatus=` in `scourd.service` names.
const BUSY: i32 = 75;

/// The exit status for "start me again", after `Restart`: one no
/// `SuccessExitStatus=` names, so a unit's `Restart=on-failure` brings it back.
const AGAIN: i32 = 76;

/// `Restart` was asked: once the index is committed, come back. See [`again`].
static RESTART: AtomicBool = AtomicBool::new(false);

/// After the index is committed, when `Restart` was asked: started again by a
/// system unit, for a status it does not count as success; otherwise by a copy
/// of this process, which waits for this one to be gone before it opens the
/// index. Faces finding nothing in the gap wait, told by the marker.
fn again() {
    if !RESTART.load(Ordering::Relaxed) {
        return;
    }
    if under_a_unit() {
        std::process::exit(AGAIN);
    }
    #[cfg(unix)]
    if let Ok(exe) = std::env::current_exe() {
        use std::os::unix::process::CommandExt;
        // A binary replaced while running is still named for what replaced it.
        let exe = std::path::PathBuf::from(
            exe.to_string_lossy()
                .trim_end_matches(" (deleted)")
                .to_string(),
        );
        let mut again = std::process::Command::new(exe);
        again
            .args(std::env::args_os().skip(1))
            .env("SCOUR_RESTART_AFTER", std::process::id().to_string());
        // SAFETY: `setsid` only, in the child before it execs: a session of
        // its own, as a face starts one, so it outlives this one.
        unsafe {
            again.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
        if let Err(e) = again.spawn() {
            eprintln!("scourd: could not start again: {e}");
        }
    }
    std::process::exit(0);
}

/// Run by a systemd unit of Scour's own: the last step of this process's
/// cgroup is `scour….service`. Not `INVOCATION_ID`, which every program of a
/// desktop session inherits — a window has it, and so would a service the
/// window started, which no unit would bring back.
fn under_a_unit() -> bool {
    #[cfg(target_os = "linux")]
    {
        std::fs::read_to_string("/proc/self/cgroup").is_ok_and(|c| unit_of(&c))
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// See [`under_a_unit`]; the cgroup file's text.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn unit_of(cgroup: &str) -> bool {
    cgroup.lines().any(|l| {
        let last = l.rsplit('/').next().unwrap_or_default();
        last.starts_with("scour") && last.ends_with(".service")
    })
}

/// Started by [`again`]: the one before has the index until it is gone.
#[cfg(unix)]
fn after_the_last() {
    let Some(pid) = std::env::var("SCOUR_RESTART_AFTER")
        .ok()
        .and_then(|p| p.parse::<i32>().ok())
    else {
        return;
    };
    // SAFETY: nothing else runs yet; and a child of ours must not wait too.
    unsafe { std::env::remove_var("SCOUR_RESTART_AFTER") };
    let until = std::time::Instant::now() + Duration::from_secs(20);
    // SAFETY: signal 0 only asks whether the process is there.
    while unsafe { libc::kill(pid, 0) } == 0 && std::time::Instant::now() < until {
        std::thread::sleep(Duration::from_millis(50));
    }
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
    #[cfg(target_os = "linux")]
    scour_source_fs::keep_from_children();
    cap_allocator_arenas();
    let args = Args::parse();
    #[cfg(windows)]
    if args.service {
        return service::dispatch(args);
    }
    run(args)
}

/// Everything after the arguments: the same for a console start and a service.
pub(crate) fn run(args: Args) -> Result<()> {
    resources::mark_start();
    #[cfg(unix)]
    after_the_last();
    if let Some(owner) = &args.owner {
        as_owner::set_owner(owner);
    }
    let (mut config, problem) = match &args.config {
        Some(p) => (Config::load_from(p)?, None),
        None => Config::load_or_default(),
    };
    if let Some(e) = problem {
        // Fatal: falling back to the default source list drops every source
        // but the home directory, and the engine forgets the rows of a source
        // it no longer has. A service that will not start is the cheaper fault.
        return Err(e.into());
    }
    if let Some(dir) = &args.index {
        config.index.dir = dir.clone();
    }

    let addr = args.socket.clone().unwrap_or_else(|| config.socket());
    // Beside the index rather than in the config file, which is hand-edited.
    // Beside *this* index, not the default one: everything the config points
    // at moves together, or `--config` is not the isolation it claims to be.
    // Before the engine: what a person said about the disks decides its sources.
    let state = args
        .state
        .clone()
        .unwrap_or_else(|| wire::state_dir(&config));
    let kept = handle::Kept::open(state, config.clone());
    let plan = kept.plan();
    documents::start(
        &kept.settings.lock().unwrap_or_else(|p| p.into_inner()),
        &kept.dir,
    );
    let index = match wire::open(&config) {
        Ok(index) => index,
        // Somebody already serves this index — a face started one, or the
        // system unit did. Not a failure to retry: `scourd.service` counts
        // this status as success, so it stops instead of restarting every
        // five seconds for as long as the other one lives.
        Err(e) if matches!(e.downcast_ref(), Some(scour_core::Error::IndexBusy { .. })) => {
            eprintln!("scourd: {e:#}");
            std::process::exit(BUSY);
        }
        Err(e) => return Err(e),
    };
    let engine = wire::engine(&config, Arc::clone(&index), &plan.sources, 0);
    engine.set_reading(kept.reading());
    wire::tune(&kept.settings.lock().unwrap_or_else(|p| p.into_inner()));

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

    let access = args.owner.as_deref().map(access_for);
    let server = Server::bind_for(&addr, access.as_deref())?;
    // Back: faces need not wait any longer.
    let _ = std::fs::remove_file(scour_launch::restart_marker(&addr));
    let stop = Arc::new(AtomicBool::new(false));
    scour_core::note!(
        "scourd: listening on {addr} · {} sources · watching starting",
        engine.sources().len()
    );

    let live = Arc::new(live::Live::new(engine, index, plan));
    let kept = Arc::new(kept);
    let walk = if want_scan {
        live::Walk::All
    } else {
        live::Walk::Nothing
    };
    live::start(live.engine(), walk);
    live::follow_disks(Arc::clone(&live), Arc::clone(&kept));
    documents::ahead::start(Arc::clone(&live), Arc::clone(&kept));
    {
        // Ctrl-C, and systemd's SIGTERM, have to reach the accept loop, which is
        // blocked in `accept`. Setting the flag and connecting once wakes it.
        let (stop, addr) = (Arc::clone(&stop), addr.clone());
        let live = Arc::clone(&live);
        let _ = STOP.set(Box::new(move || {
            stop.store(true, Ordering::Relaxed);
            live.shutdown();
            // Nothing else wakes `accept`. With the socket's file gone — a
            // runtime directory cleared under a running service — the connect
            // fails and the loop would wait for ever; the index is committed
            // by now, so the process leaves instead.
            if scour_ipc::Client::connect(&addr).is_err() {
                std::process::exit(0);
            }
        }));
        let _ = ctrlc::set_handler(stop_now);
    }

    let handler_live = Arc::clone(&live);
    let handler_stop = Arc::clone(&stop);
    let wake_addr = addr.clone();
    let serve = move || {
        server.serve(
            move |req, emit| {
                let restart = cfg!(unix) && matches!(req, scour_proto::Request::Restart {});
                if restart {
                    RESTART.store(true, Ordering::Relaxed);
                    // Written while the socket still answers: a face that
                    // finds it gone has already been told to wait.
                    let _ = std::fs::write(scour_launch::restart_marker(&wake_addr), b"");
                }
                if restart || matches!(req, scour_proto::Request::Shutdown {}) {
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
                handle::dispatch(&handler_live, &kept, req, emit)
            },
            Arc::clone(&stop),
        );
        live.shutdown();
        if let Some(documents) = documents::shared() {
            documents.flush();
        }
    };
    // A Mac's key is held on the main thread, where Carbon delivers it, so
    // the accept loop takes another; when that ends, so does the process.
    #[cfg(target_os = "macos")]
    {
        let serving = std::thread::spawn(move || {
            serve();
            again();
            std::process::exit(0);
        });
        hold_key();
        let _ = serving.join();
    }
    #[cfg(not(target_os = "macos"))]
    {
        serve();
        again();
    }
    Ok(())
}

/// Hold the key that opens Scour, for as long as this process lives: nothing
/// else of Scour's stays running, and a Mac keeps no list of keys a program
/// may add to. Pressed, it opens the face somebody last switched to. Returns
/// at once where there is no session to hold a key in.
#[cfg(target_os = "macos")]
fn hold_key() {
    let keys = scour_hotkey::Hotkey::detect();
    let Some(file) = keys.tools().kept.clone() else {
        return;
    };
    let launcher = keys.command();
    let window = keys
        .tools()
        .exe
        .as_deref()
        .and_then(std::path::Path::parent)
        .map(|dir| dir.join("scour-gui"));
    scour_hotkey::listen(&file, move || {
        let started = std::process::Command::new(&launcher).spawn().or_else(|e| {
            // No launcher beside this program: the window is what there is.
            window
                .as_ref()
                .map_or(Err(e), |gui| std::process::Command::new(gui).spawn())
        });
        match started {
            // Waited for, on a thread of its own, or it stays a zombie.
            Ok(mut child) => drop(std::thread::spawn(move || child.wait())),
            Err(e) => scour_core::note!("scourd: the key was pressed and nothing opened: {e}"),
        }
    });
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
pub(crate) fn common_prefix(paths: &[String]) -> String {
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

#[cfg(test)]
mod restart_tests {
    use super::unit_of;

    #[test]
    fn only_scour_s_own_unit_restarts_it() {
        assert!(unit_of("0::/system.slice/scour.service\n"));
        assert!(unit_of(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/scourd.service"
        ));
        assert!(unit_of(
            "0::/system.slice/system-scour.slice/scour@hasan.service"
        ));
        // A desktop session's programs, and whatever they start.
        assert!(!unit_of(
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome-scour\\x2dopen-2785589.scope"
        ));
        assert!(!unit_of("0::/user.slice/user-1000.slice/user@1000.service"));
    }
}
