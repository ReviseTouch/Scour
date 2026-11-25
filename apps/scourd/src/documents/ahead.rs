//! Documents read before anybody asks, when the setting says so: only after
//! nobody has searched for a while and the machine itself is quiet, through
//! the same reading a search does, at the lowest priority and a slow pace.

use std::sync::Arc;
use std::time::Duration;

use scour_grep::Needle;

/// How often it looks whether it may go on.
const EVERY: Duration = Duration::from_secs(300);
/// Nobody searched for this long.
const IDLE: Duration = Duration::from_secs(300);
/// One round: then it looks again before going on.
const ROUND: Duration = Duration::from_secs(60);
/// Its pace. On a 5,400 rpm disk a small read beside it took 11.4 ms at
/// 4 MiB/s, as with nothing reading, and 13.3 ms at 16; the lowest I/O
/// priority alone changed nothing there.
const PACE: u64 = 4 << 20;
/// The documents it reads ahead: those `scour-grep` hands over.
const QUERY: &str = "ext:pdf;doc;dot;xls;xlt;ppt;pps;pot;eml";

/// `SCOUR_READ_AHEAD_SECS` shortens both waits and reads whatever the load,
/// for trying it out on a machine that is never quiet.
fn waits() -> (Duration, Duration, bool) {
    match std::env::var("SCOUR_READ_AHEAD_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
    {
        Some(secs) => (Duration::from_secs(secs), Duration::from_secs(secs), false),
        None => (EVERY, IDLE, true),
    }
}

/// Runs for the life of the service; costs one wake every five minutes.
pub fn start(live: Arc<crate::live::Live>, kept: Arc<crate::handle::Kept>) {
    let _ = std::thread::Builder::new()
        .name("scourd-read-ahead".into())
        .spawn(move || {
            let (every, idle, mind_load) = waits();
            let mut from = 0u32;
            loop {
                std::thread::sleep(every);
                let on = {
                    let s = kept.settings.lock().unwrap_or_else(|p| p.into_inner());
                    s.texts_ahead && !s.texts_off
                };
                if !on || super::shared().is_none() {
                    continue;
                }
                let engine = live.engine();
                if engine.idle_for() < idle || (mind_load && machine_busy()) {
                    continue;
                }
                from = round(&engine, from);
            }
        });
}

/// Read on from `from` for one round; where to go on from next time.
fn round(engine: &scour_engine::Engine, from: u32) -> u32 {
    let ask = scour_engine::GrepAsk {
        // Never found: every document is read to its end, and kept.
        needle: Needle::Text("\u{1}scour-read-ahead\u{1}".into()),
        exact_case: true,
        binary: false,
        markup: false,
        sort: scour_core::SortKey::Modified,
        descending: true,
        offset: from,
        top: 1,
        hits: 0,
        read_budget: 0,
        max_size: 64 << 20,
        timeout: ROUND,
        rate: PACE,
        background: true,
    };
    let enter: scour_grep::Enter = Arc::new(crate::as_owner::for_a_read);
    match engine.grep(QUERY, &ask, Some(enter)) {
        // Stopped short: go on from there. Read to the end: from the newest again.
        Ok(done) => done.next.unwrap_or(0),
        Err(_) => 0,
    }
}

/// Whether something else is keeping the machine busy: the load over the
/// last minute above a quarter of its cores. Linux only; elsewhere, never.
fn machine_busy() -> bool {
    #[cfg(target_os = "linux")]
    {
        let cores = std::thread::available_parallelism().map_or(1, |n| n.get()) as f64;
        let load = std::fs::read_to_string("/proc/loadavg")
            .ok()
            .and_then(|s| s.split_whitespace().next()?.parse::<f64>().ok());
        if let Some(load) = load {
            return load > cores / 4.0;
        }
    }
    false
}
