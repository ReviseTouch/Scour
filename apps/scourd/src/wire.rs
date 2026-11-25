//! Choosing the implementations: every concrete type in Scour is named here and
//! nowhere else. If a second file needs to say `NativeIndex`, something above it
//! has stopped being written against the trait.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use scour_config::Config;
use scour_core::{Index, Source, SourceId};
use scour_engine::{Engine, EngineOptions};
use scour_index_native::NativeIndex;
use scour_source_fs::{FsSource, platform_defaults};

/// How seldom the system's disk is walked when nothing watches it. Its pulse
/// moves every two seconds — the journal alone — so walking it as often as its
/// cost allows is walking it all the time.
const SYSTEM_PATIENCE: Duration = Duration::from_secs(600);

/// The index this configuration points at, opened once for the life of the
/// process: an engine replaced when the disks change takes the same one.
pub fn open(config: &Config) -> Result<Arc<dyn Index>> {
    // One engine, so a line rather than a match; a second one is a second arm
    // here and nothing anywhere else.
    Ok(Arc::new(open_index(&index_dir(config))?))
}

/// The engine over these sources, each with the other sources' roots it
/// leaves alone — [`crate::disks::Plan::sources`]. A source no longer planned
/// loses its rows here, so the engine before this one must have stopped.
pub fn engine(
    config: &Config,
    index: Arc<dyn Index>,
    planned: &[crate::disks::Planned],
    first_revision: u64,
) -> Engine {
    // The number a source's rows carry is remembered, not counted: a position
    // in the configuration array would rebind every row when sources move.
    let names: Vec<String> = planned.iter().map(|p| p.source.name.clone()).collect();
    let assigned = crate::sources::assign(&index_dir(config), &names);
    for id in &assigned.dropped {
        match index.forget(SourceId(*id)) {
            Ok(0) => {}
            Ok(n) => eprintln!("scourd: a source is gone; {n} of its entries went with it"),
            Err(e) => eprintln!("scourd: a removed source's entries are still here: {e}"),
        }
    }

    let sources: Vec<Arc<dyn Source>> = planned
        .iter()
        .zip(&assigned.ids)
        .map(|(p, id)| {
            let s = &p.source;
            let walk_as = crate::as_owner::walk_as(&s.roots);
            // A share read as its owner is watched as its owner too, through the
            // server's change notifications; its patience is for when that fails.
            let shared = walk_as.is_some();
            Arc::new(
                FsSource::new(SourceId(*id), s.name.clone(), s.roots.clone())
                    .with_kind(s.kind.into())
                    .with_watch(s.watch)
                    .with_skip(p.skip.clone())
                    .with_walk_as(walk_as)
                    .with_journal_dir(Some(config.index.dir.clone()))
                    .with_patience(match p.every {
                        Some(every) => every,
                        None if p.system || shared => SYSTEM_PATIENCE,
                        None => Duration::ZERO,
                    }),
            ) as Arc<dyn Source>
        })
        .collect();

    let engine = Engine::new(
        sources,
        index,
        EngineOptions {
            scan: scan_options(config),
            commit_interval: Duration::from_millis(config.service.commit_interval_ms.max(50)),
            // Never shorter than the burst clock: `commit_interval` is a floor
            // on how often a segment is written at all.
            commit_idle: Duration::from_millis(
                config
                    .service
                    .commit_idle_ms
                    .max(config.service.commit_interval_ms),
            ),
            rebuild_threshold: config.index.rebuild_threshold,
            poll_interval: Duration::from_secs(config.service.poll_interval_secs.max(1)),
            reconcile_interval: Duration::from_secs(config.service.reconcile_interval_secs.max(1)),
            watched_reconcile: Duration::from_secs(
                config.service.watched_reconcile_interval_secs.max(60),
            ),
            // Raised to a window's fetch run and no further: a window records
            // a whole run of `scour_core::PAGE_ROWS` as loaded, so a lower
            // ceiling leaves rows that never arrive.
            result_limit: config.ui.result_limit.max(scour_core::PAGE_ROWS),
            first_revision,
            ..EngineOptions::default()
        },
    );
    // Whatever reads file contents reads as the person asking; see `preview`.
    engine.read_files_as(Arc::new(crate::as_owner::for_a_read));
    if let Some(documents) = crate::documents::shared() {
        engine.read_documents_with(Arc::clone(documents) as Arc<dyn scour_grep::Texts>);
    }
    engine
}

/// How queries are read, as a person set it: [`scour_settings::Settings`]'s
/// two search fields, in the engine's words. An unknown word offers.
/// What the settings say about memory and CPU, put into effect for the whole
/// process: the engine's release and patience, and how wide a search goes.
pub fn tune(kept: &scour_settings::Settings) {
    scour_engine::tune(&scour_engine::Tuning {
        release_after: kept.release_after(machine_memory()),
        fresh_within: (kept.fresh_within > 0)
            .then(|| std::time::Duration::from_secs(u64::from(kept.fresh_within))),
    });
    scour_index_native::set_search_threads(kept.search_threads as usize);
}

/// The machine's memory in bytes, or zero where it cannot be read — which
/// the settings take as not a small machine. Read once.
fn machine_memory() -> u64 {
    static BYTES: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *BYTES.get_or_init(read_machine_memory)
}

#[cfg(target_os = "linux")]
fn read_machine_memory() -> u64 {
    std::fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|line| line.strip_prefix("MemTotal:"))
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|kib| kib.parse::<u64>().ok())
        })
        .map_or(0, |kib| kib * 1024)
}

#[cfg(windows)]
fn read_machine_memory() -> u64 {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
    // SAFETY: a struct of plain numbers, its length set as the call requires.
    unsafe {
        let mut status: MEMORYSTATUSEX = std::mem::zeroed();
        status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
        if GlobalMemoryStatusEx(&mut status) != 0 {
            status.ullTotalPhys
        } else {
            0
        }
    }
}

#[cfg(target_os = "macos")]
fn read_machine_memory() -> u64 {
    std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()
        .and_then(|out| String::from_utf8(out.stdout).ok())
        .and_then(|text| text.trim().parse().ok())
        .unwrap_or(0)
}

#[cfg(not(any(target_os = "linux", windows, target_os = "macos")))]
fn read_machine_memory() -> u64 {
    0
}

pub fn reading(kept: &scour_settings::Settings) -> scour_engine::Reading {
    scour_engine::Reading {
        exact_accents: kept.accents_exact,
        typos: match kept.typos.as_str() {
            "off" => scour_engine::Typos::Off,
            "always" => scour_engine::Typos::Always,
            _ => scour_engine::Typos::Suggest,
        },
        strict_typos: kept.typos_strict,
    }
}

/// Open the index, rebuilding it from nothing if an older Scour wrote it or it
/// is damaged. The decision is here because only this file knows every source
/// is a filesystem that can be read again, and a cold index is scanned on
/// start. A damaged one used to stop the service for good: it failed to open,
/// systemd started it again five seconds later, and that went on for ever.
fn open_index(dir: &std::path::Path) -> Result<NativeIndex> {
    match NativeIndex::open_or_create(dir) {
        Err(scour_core::Error::IndexCorrupt { detail }) => {
            let aside = set_aside(dir)
                .with_context(|| format!("moving the damaged index at {}", dir.display()))?;
            eprintln!(
                "scourd: the index at {} is damaged ({detail}); it was moved to {} and is \
                 being built again from the disks",
                dir.display(),
                aside.display()
            );
            NativeIndex::open_or_create(dir)
                .with_context(|| format!("opening the index at {}", dir.display()))
        }
        Err(scour_core::Error::IndexOutdated { found, expected }) => {
            eprintln!(
                "scourd: the index at {} is format {found} and this build writes {expected}; \
                 building it again",
                dir.display()
            );
            NativeIndex::discard(dir)
                .with_context(|| format!("clearing the old index at {}", dir.display()))?;
            NativeIndex::open_or_create(dir)
                .with_context(|| format!("opening the index at {}", dir.display()))
        }
        other => other.with_context(|| format!("opening the index at {}", dir.display())),
    }
}

/// Move a damaged index beside itself, as `native.damaged`, rather than delete
/// it: the files are what anyone will want to look at. One is kept; an older
/// one goes, or a disk that keeps damaging it fills up with copies.
fn set_aside(dir: &std::path::Path) -> std::io::Result<std::path::PathBuf> {
    let mut name = dir.file_name().unwrap_or_default().to_os_string();
    name.push(".damaged");
    let aside = dir.with_file_name(name);
    if aside.exists() {
        std::fs::remove_dir_all(&aside)?;
    }
    std::fs::rename(dir, &aside)?;
    Ok(aside)
}

/// Where the index files go: a subdirectory, so a second engine beside this one
/// does not count its bytes in `bytes_on_disk`.
fn index_dir(config: &Config) -> std::path::PathBuf {
    config.index.dir.join("native")
}

/// The data directory *this* configuration points at: the parent of the index,
/// the same convention `main.rs` uses for the kept settings, so `--config` moves
/// everything a service writes about itself or none of it.
fn data_dir(config: &Config) -> &std::path::Path {
    config
        .index
        .dir
        .parent()
        .unwrap_or(config.index.dir.as_path())
}

/// The configured exclusions, plus the platform's own — merged rather than
/// replaced: adding one directory still leaves `/proc` skipped.
fn scan_options(config: &Config) -> scour_core::ScanOptions {
    // Read here rather than passed in: the walk is configured before `main.rs`
    // opens the settings, and the two must read the same directory.
    let added = scour_settings::Settings::load(&state_dir(config));
    scan_options_with(config, &added)
}

/// Where a window's own settings live, beside this index: [`Config::state_dir`],
/// under the name the rest of this program uses.
pub fn state_dir(config: &Config) -> std::path::PathBuf {
    config.state_dir()
}

/// What `config.toml` itself asks to skip — no built-ins, nothing a window
/// added. Reported rather than used: a panel shows these as their own group,
/// because they can be switched off but not deleted from a hand-edited file.
pub fn config_rules(config: &Config) -> (Vec<String>, Vec<String>, Vec<String>, Vec<String>) {
    let c = &config.exclude;
    (
        c.paths.clone(),
        c.dirs.clone(),
        c.files.clone(),
        c.allow.clone(),
    )
}

/// The rules in force, given what a window has written: the built-in set, the
/// hand-written `config.toml`, and what a panel wrote beside the index. Merged
/// first and switched-off ones taken out last, so off means off in both lists.
pub fn scan_options_with(
    config: &Config,
    added: &scour_settings::Settings,
) -> scour_core::ScanOptions {
    let (paths, dirs, files) = platform_defaults();
    let mut o = config.scan_options();
    o.skip_metadata = false;

    o.exclude_paths.extend(added.exclude_paths.iter().cloned());
    o.exclude_dirs.extend(added.exclude_dirs.iter().cloned());
    o.exclude_files.extend(added.exclude_files.iter().cloned());
    o.allow.extend(added.exclude_allow.iter().cloned());
    // Nothing Scour writes about itself is indexed: a commit writes segments,
    // the watcher sees them, indexing them writes more — 62% of a core idle,
    // and 8.25% more per open web window through its browser profile.
    //
    // Denied rather than excluded, since an `allow` is tested first and would
    // win; here rather than in the platform defaults because only this file
    // knows where the index was configured to go.
    merge(
        &mut o.deny,
        vec![
            config.index.dir.to_string_lossy().into_owned(),
            index_dir(config).to_string_lossy().into_owned(),
            data_dir(config).to_string_lossy().into_owned(),
        ],
    );
    merge(&mut o.exclude_paths, paths);
    merge(&mut o.exclude_dirs, dirs);
    merge(&mut o.exclude_files, files);

    // Switched off last, after everything is in, so a rule written in two of
    // the three places goes out of both. `deny` is deliberately not offered:
    // it holds the service's own directories, not an exclusion anyone chose.
    switch_off(&mut o.exclude_paths, "path", added);
    switch_off(&mut o.exclude_dirs, "dir", added);
    switch_off(&mut o.exclude_files, "file", added);
    switch_off(&mut o.allow, "allow", added);
    o
}

fn merge(into: &mut Vec<String>, extra: Vec<String>) {
    for e in extra {
        if !into.iter().any(|x| x.eq_ignore_ascii_case(&e)) {
            into.push(e);
        }
    }
}

fn switch_off(list: &mut Vec<String>, kind: &str, added: &scour_settings::Settings) {
    list.retain(|v| !added.rule_off(kind, v));
}

#[cfg(test)]
mod tests;
