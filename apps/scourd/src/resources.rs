//! What the service holds on this machine, for the panel a face's status bar
//! opens, and the clearing of it. Read from `/proc` on Linux; elsewhere the
//! memory, processor and program figures are zero and a face leaves them out.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use scour_proto::{Cpu, Held, HeldKind, Memory, Resources, Running};

static STARTED: OnceLock<Instant> = OnceLock::new();

/// Called once, early: what `uptime_ms` counts from.
pub fn mark_start() {
    let _ = STARTED.set(Instant::now());
}

/// A walked folder's size is kept this long: a panel open asks every two
/// seconds, and a browser profile is thousands of files.
const SIZES_FOR: Duration = Duration::from_secs(10);

/// The answer, from what the engine and the documents say and what is on disk.
pub fn collect(
    stats: &scour_core::IndexStats,
    texts: Option<&crate::documents::Held>,
    state_dir: &Path,
) -> Resources {
    let mut files = vec![Held {
        kind: HeldKind::Index,
        path: stats.dir.clone(),
        bytes: stats.bytes_on_disk,
        items: stats.entries,
        parts: stats.segments,
        ..Held::default()
    }];
    if let Some(t) = texts.filter(|t| t.on && !t.dir.is_empty()) {
        files.push(Held {
            kind: HeldKind::Texts,
            path: t.dir.clone(),
            bytes: t.stats.disk,
            items: t.stats.documents,
            limit: u64::from(t.max_mb) << 20,
            ..Held::default()
        });
    }
    let previews = scour_preview::office::cache_root();
    files.push(Held {
        kind: HeldKind::Previews,
        bytes: size_of(&previews),
        path: previews.to_string_lossy().into_owned(),
        ..Held::default()
    });
    let page = page_profiles();
    if page.iter().any(|p| p.is_dir()) {
        files.push(Held {
            kind: HeldKind::Page,
            path: page[0].to_string_lossy().into_owned(),
            bytes: page.iter().map(|p| size_of(p)).sum(),
            busy: if page_open(&page) { "open" } else { "" }.into(),
            ..Held::default()
        });
    }
    files.push(Held {
        kind: HeldKind::State,
        path: state_dir.to_string_lossy().into_owned(),
        bytes: size_of(state_dir),
        busy: "kept".into(),
        ..Held::default()
    });
    Resources {
        version: env!("CARGO_PKG_VERSION").into(),
        uptime_ms: STARTED.get().map_or(0, |t| t.elapsed().as_millis() as u64),
        files,
        unsorted: stats.unsorted_entries,
        memory: sys::memory(),
        cpu: sys::cpu(),
        running: sys::running(),
        restartable: cfg!(unix),
    }
}

/// Empty the previews or the page's profile. The index and the texts have
/// requests of their own, which the caller routes to.
pub fn clear(what: HeldKind) -> Result<(), scour_core::Error> {
    let gone = |dir: &Path| -> Result<(), scour_core::Error> {
        match std::fs::read_dir(dir) {
            Ok(entries) => {
                for e in entries.flatten() {
                    let p = e.path();
                    let done = if e.file_type().is_ok_and(|t| t.is_dir()) {
                        std::fs::remove_dir_all(&p)
                    } else {
                        std::fs::remove_file(&p)
                    };
                    done.map_err(|e| scour_core::Error::io(&e, &p.to_string_lossy()))?;
                }
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(scour_core::Error::io(&e, &dir.to_string_lossy())),
        }
    };
    let done = match what {
        HeldKind::Previews => gone(&scour_preview::office::cache_root()),
        HeldKind::Page => {
            let page = page_profiles();
            // A browser writing into a profile while it is taken apart is a
            // profile that will not open again.
            if page_open(&page) {
                return Err(scour_core::Error::unsupported(
                    "emptying the page's profile while the page is open: close it first",
                ));
            }
            page.iter().try_for_each(|p| gone(p))
        }
        HeldKind::State => Err(scour_core::Error::unsupported(
            "emptying the settings: they are kept",
        )),
        HeldKind::Index | HeldKind::Texts => Err(scour_core::Error::unsupported(
            "this through `clear`: the service routes it",
        )),
    };
    forget_sizes();
    done
}

/// The profiles `scour-app` gives the page's browser: Chromium's, Firefox's.
fn page_profiles() -> Vec<PathBuf> {
    let data = scour_config::data_dir();
    vec![data.join("app"), data.join("app-firefox")]
}

/// A browser holds one of them: Chromium's `SingletonLock` link, Firefox's `lock`.
fn page_open(profiles: &[PathBuf]) -> bool {
    profiles.iter().any(|p| {
        ["SingletonLock", "lock", ".parentlock"]
            .iter()
            .any(|f| std::fs::symlink_metadata(p.join(f)).is_ok())
    })
}

type Sizes = Vec<(PathBuf, Instant, u64)>;

fn sizes() -> &'static Mutex<Sizes> {
    static SIZES: OnceLock<Mutex<Sizes>> = OnceLock::new();
    SIZES.get_or_init(|| Mutex::new(Vec::new()))
}

fn forget_sizes() {
    sizes().lock().unwrap_or_else(|p| p.into_inner()).clear();
}

/// The bytes of every file under `dir`, links not followed; 0 for none.
fn size_of(dir: &Path) -> u64 {
    {
        let kept = sizes().lock().unwrap_or_else(|p| p.into_inner());
        if let Some((_, _, n)) = kept
            .iter()
            .find(|(p, at, _)| p == dir && at.elapsed() < SIZES_FOR)
        {
            return *n;
        }
    }
    let n = walk(dir);
    let mut kept = sizes().lock().unwrap_or_else(|p| p.into_inner());
    kept.retain(|(p, _, _)| p != dir);
    kept.push((dir.to_path_buf(), Instant::now(), n));
    n
}

fn walk(dir: &Path) -> u64 {
    let mut total = 0;
    let mut todo = vec![dir.to_path_buf()];
    while let Some(d) = todo.pop() {
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in entries.flatten() {
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() {
                todo.push(e.path());
            } else if t.is_file()
                && let Ok(m) = e.metadata()
            {
                total += m.len();
            }
        }
    }
    total
}

#[cfg(target_os = "linux")]
mod sys {
    use super::*;

    fn read(path: &str) -> String {
        std::fs::read_to_string(path).unwrap_or_default()
    }

    /// A `/proc/*/status` line's number of kilobytes, as bytes.
    fn kb(status: &str, key: &str) -> u64 {
        status
            .lines()
            .find_map(|l| l.strip_prefix(key))
            .and_then(|v| v.split_whitespace().next())
            .and_then(|n| n.parse::<u64>().ok())
            .map_or(0, |n| n << 10)
    }

    fn ticks() -> u64 {
        // SAFETY: `sysconf` reads a constant of the running system.
        let t = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
        if t > 0 { t as u64 } else { 100 }
    }

    pub fn memory() -> Memory {
        let me = read("/proc/self/status");
        let all = read("/proc/meminfo");
        Memory {
            service: kb(&me, "VmRSS:"),
            peak: kb(&me, "VmHWM:"),
            own: kb(&me, "RssAnon:") + kb(&me, "RssShmem:") + kb(&me, "VmSwap:"),
            mapped: kb(&me, "RssFile:"),
            total: kb(&all, "MemTotal:"),
            available: kb(&all, "MemAvailable:"),
        }
    }

    /// Fields 14 and 15 of `/proc/*/stat`, after the name in parentheses,
    /// which may itself hold spaces; and field 22, when it started.
    fn stat_fields(stat: &str) -> Vec<u64> {
        stat.rsplit_once(')')
            .map(|(_, rest)| {
                rest.split_whitespace()
                    .map(|f| f.parse().unwrap_or(0))
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn cpu() -> Cpu {
        let me = stat_fields(&read("/proc/self/stat"));
        // After the name: state is [0], utime [11], stime [12].
        let used = me.get(11).copied().unwrap_or(0) + me.get(12).copied().unwrap_or(0);
        let line = read("/proc/stat");
        let first: Vec<u64> = line
            .lines()
            .next()
            .unwrap_or_default()
            .split_whitespace()
            .skip(1)
            .map(|f| f.parse().unwrap_or(0))
            .collect();
        let total: u64 = first.iter().take(8).sum();
        // Idle and waiting for the disk are not work.
        let idle = first.get(3).copied().unwrap_or(0) + first.get(4).copied().unwrap_or(0);
        let load = read("/proc/loadavg")
            .split_whitespace()
            .next()
            .and_then(|l| l.parse::<f64>().ok())
            .unwrap_or(0.0);
        Cpu {
            service_ms: used * 1000 / ticks(),
            busy: total.saturating_sub(idle),
            total,
            threads: std::thread::available_parallelism().map_or(0, |n| n.get() as u32),
            load_x100: (load * 100.0).round() as u32,
        }
    }

    /// Scour's programs of this account, by the name the kernel keeps.
    pub fn running() -> Vec<Running> {
        const KINDS: [(&str, &str); 5] = [
            ("scour-gui", "window"),
            ("scour-web", "page"),
            ("scour-tui", "terminal"),
            ("scour-mcp", "mcp"),
            ("scour-extractor", "extractor"),
        ];
        // SAFETY: `getuid` cannot fail.
        let uid = unsafe { libc::getuid() }.to_string();
        let since_boot = read("/proc/uptime")
            .split_whitespace()
            .next()
            .and_then(|u| u.parse::<f64>().ok())
            .unwrap_or(0.0);
        let mut out: Vec<Running> = Vec::new();
        let Ok(procs) = std::fs::read_dir("/proc") else {
            return out;
        };
        for p in procs.flatten() {
            let name = p.file_name();
            let Some(pid) = name
                .to_str()
                .filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
            else {
                continue;
            };
            let status = read(&format!("/proc/{pid}/status"));
            let Some(comm) = status.lines().find_map(|l| l.strip_prefix("Name:")) else {
                continue;
            };
            let Some((_, kind)) = KINDS.iter().find(|(n, _)| *n == comm.trim()) else {
                continue;
            };
            let mine = status
                .lines()
                .find_map(|l| l.strip_prefix("Uid:"))
                .and_then(|u| u.split_whitespace().next())
                .is_some_and(|u| u == uid);
            if !mine {
                continue;
            }
            let started = stat_fields(&read(&format!("/proc/{pid}/stat")))
                .get(19)
                .copied()
                .unwrap_or(0);
            let age = (since_boot - started as f64 / ticks() as f64).max(0.0) as u64;
            let rss = kb(&status, "VmRSS:");
            match out.iter_mut().find(|r| r.kind == *kind) {
                Some(r) => {
                    r.count += 1;
                    r.bytes += rss;
                    r.oldest_s = r.oldest_s.max(age);
                }
                None => out.push(Running {
                    kind: (*kind).into(),
                    count: 1,
                    bytes: rss,
                    oldest_s: age,
                }),
            }
        }
        let order = |k: &str| KINDS.iter().position(|(_, x)| *x == k).unwrap_or(9);
        out.sort_by_key(|r| order(&r.kind));
        out
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_name_with_spaces_and_parentheses_does_not_shift_the_fields() {
            let f =
                stat_fields("42 (a (b) c) S 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20");
            assert_eq!(f.get(11), Some(&11));
            assert_eq!(f.get(19), Some(&19));
        }

        #[test]
        fn this_process_is_measured() {
            let m = memory();
            assert!(m.service > 0 && m.peak >= m.service && m.total >= m.available);
            // Resident is the two halves, less what is swapped out.
            assert!(m.own > 0 && m.mapped > 0 && m.own + m.mapped >= m.service);
            let c = cpu();
            assert!(c.total >= c.busy && c.threads > 0);
        }
    }
}

#[cfg(not(target_os = "linux"))]
mod sys {
    use super::*;

    pub fn memory() -> Memory {
        Memory::default()
    }

    pub fn cpu() -> Cpu {
        Cpu {
            threads: std::thread::available_parallelism().map_or(0, |n| n.get() as u32),
            ..Cpu::default()
        }
    }

    pub fn running() -> Vec<Running> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_is_its_files_and_is_emptied_whole() {
        let dir = tempfile::tempdir().expect("temp");
        std::fs::create_dir_all(dir.path().join("a/b")).expect("dirs");
        std::fs::write(dir.path().join("a/x"), [0u8; 100]).expect("x");
        std::fs::write(dir.path().join("a/b/y"), [0u8; 23]).expect("y");
        assert_eq!(walk(dir.path()), 123);
        assert_eq!(walk(&dir.path().join("none")), 0);
    }

    #[test]
    fn an_open_profile_is_seen_by_its_lock() {
        let dir = tempfile::tempdir().expect("temp");
        let p = vec![dir.path().join("app"), dir.path().join("app-firefox")];
        std::fs::create_dir_all(&p[0]).expect("profile");
        assert!(!page_open(&p));
        std::fs::write(p[0].join("SingletonLock"), b"").expect("lock");
        assert!(page_open(&p));
    }
}
