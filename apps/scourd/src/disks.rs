//! Which disks are indexed: the configuration, the disks found and what a
//! person said about them, made into the list of sources.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use scour_config::{Config, SourceCfg, SourceKindCfg};
use scour_proto::{Disk, DiskState};
use scour_settings::Settings;
use scour_volumes::{Kind, Volume};

/// What the service indexes, and what it says about every disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub sources: Vec<Planned>,
    pub disks: Vec<Disk>,
}

/// One source to build: what it is, the roots under its own that it leaves
/// to others, and whether it is the system's disk — walked seldom when
/// nothing watches it, since its pulse never rests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub source: SourceCfg,
    pub skip: Vec<PathBuf>,
    pub system: bool,
    /// For a folder of its own: the least time between two walks when nothing
    /// reports its changes, as the person chose it.
    pub every: Option<std::time::Duration>,
}

impl Plan {
    /// Disks waiting for somebody to say.
    pub fn asking(&self) -> u32 {
        self.disks
            .iter()
            .filter(|d| d.state == DiskState::Asking)
            .count() as u32
    }
}

/// Sources a person wrote in `config.toml` always; built-in disks unless taken
/// out, when nobody wrote any; plugged-in and network disks once chosen, or as
/// [`Settings::removable`] and [`Settings::network`] say; folders added from a
/// face, unless a disk already holds them.
/// With no disk found at all, the configuration's own default.
pub fn plan(
    config: &Config,
    volumes: &[Volume],
    settings: &Settings,
    later: &HashSet<String>,
) -> Plan {
    let given: Vec<SourceCfg> = if config.sources_given() {
        config.sources.clone()
    } else {
        Vec::new()
    };
    let mut sources = given.clone();
    let mut system = HashSet::new();
    let mut disks = Vec::new();
    for v in volumes {
        let state = state_of(v, &given, config.sources_given(), settings, later);
        if matches!(state, DiskState::Auto | DiskState::Chosen) {
            sources.push(source_of(v));
            if v.kind == Kind::System {
                system.insert(v.id.clone());
            }
        }
        disks.push(Disk {
            volume: v.clone(),
            state,
            seen: Default::default(),
        });
    }
    if sources.is_empty() && volumes.is_empty() {
        sources = config.sources.clone();
    }
    if !config.sources_given()
        && let Some(home) = home_apart(config, &sources, volumes)
    {
        sources.insert(0, home);
    }
    let added = folders_apart(settings, &sources, volumes);
    sources.extend(added);
    let mut planned: Vec<Planned> = with_skips(sources)
        .into_iter()
        .map(|(source, skip)| Planned {
            system: system.contains(&source.name),
            // A folder of its own, and a network disk: nothing may report
            // their changes, and they wait between walks as long as was chosen.
            every: (source.name.starts_with("folder:") || source.kind == SourceKindCfg::Network)
                .then(|| folders_every(settings)),
            source,
            skip,
        })
        .collect();
    fence(&mut planned[given.len()..], volumes);
    Plan {
        sources: planned,
        disks,
    }
}

/// The home folder as a source of its own when the disk holding it is
/// indexed: it keeps its own pace while the rest of the system disk waits,
/// and the name the default source always had, so its rows stay.
fn home_apart(config: &Config, sources: &[SourceCfg], volumes: &[Volume]) -> Option<SourceCfg> {
    let home = config.sources.iter().find(|s| s.name == "home")?;
    let [root] = home.roots.as_slice() else {
        return None;
    };
    let held = sources
        .iter()
        .flat_map(|s| &s.roots)
        .any(|r| root != r && root.starts_with(r));
    let a_disk = volumes
        .iter()
        .flat_map(|v| &v.roots)
        .any(|r| Path::new(r) == root);
    (held && !a_disk).then(|| home.clone())
}

/// The service's own sources stop at every disk mounted inside them, indexed
/// or not: `/` chosen with `/mnt/depo` left out must not walk into it anyway.
/// Sources written by hand walk as they always have, and are not passed here.
fn fence(sources: &mut [Planned], volumes: &[Volume]) {
    for p in sources.iter_mut() {
        let inside = volumes
            .iter()
            .filter(|v| v.id != p.source.name)
            .flat_map(|v| v.roots.iter().map(PathBuf::from))
            .filter(|r| {
                p.source
                    .roots
                    .iter()
                    .any(|mine| r != mine && r.starts_with(mine))
            });
        for r in inside {
            if !p.skip.contains(&r) {
                p.skip.push(r);
            }
        }
    }
}

fn state_of(
    v: &Volume,
    given: &[SourceCfg],
    manual: bool,
    settings: &Settings,
    later: &HashSet<String>,
) -> DiskState {
    let covered = v.roots.iter().all(|r| {
        given
            .iter()
            .flat_map(|s| &s.roots)
            .any(|g| Path::new(r).starts_with(g))
    });
    if covered {
        DiskState::Configured
    } else if settings.volumes_off.contains(&v.id) {
        DiskState::Off
    } else if settings.volumes_on.contains(&v.id) {
        DiskState::Chosen
    } else if v.kind.is_built_in() {
        if manual {
            DiskState::Manual
        } else {
            DiskState::Auto
        }
    } else {
        // Nobody has answered for this one: what was said for its kind.
        let policy = match v.kind {
            Kind::Network => settings.network.as_str(),
            // A disc is left out unless somebody said otherwise.
            Kind::Optical => match settings.optical.as_str() {
                "always" => "always",
                "ask" => "",
                _ => "never",
            },
            _ => settings.removable.as_str(),
        };
        match policy {
            "always" => DiskState::Chosen,
            "never" => DiskState::Off,
            _ if later.contains(&v.id) => DiskState::Later,
            _ => DiskState::Asking,
        }
    }
}

/// The name a folder added from a face goes by as a source: what its last walk
/// is filed under, for the settings page.
pub fn folder_source(path: &str) -> String {
    format!("folder:{path}")
}

/// The folders added from a face, each a source of its own unless a source
/// already holds it. Named by path, so the rows stay with the folder.
/// Held means the deepest root above it is a source's: `/` holds `/etc`, but
/// not `/mnt/depo/x` when `/mnt/depo` is a disk of its own left out — `/`
/// stops at every disk mounted inside it.
fn folders_apart(settings: &Settings, sources: &[SourceCfg], volumes: &[Volume]) -> Vec<SourceCfg> {
    let mut out: Vec<SourceCfg> = Vec::new();
    for f in &settings.folders {
        let root = PathBuf::from(f);
        if !root.is_absolute() || settings.folders_off.contains(f) {
            continue;
        }
        let indexed: Vec<&PathBuf> = sources.iter().chain(&out).flat_map(|s| &s.roots).collect();
        let disks = volumes
            .iter()
            .flat_map(|v| v.roots.iter().map(PathBuf::from));
        let deepest = indexed
            .iter()
            .map(|r| (*r).clone())
            .chain(disks)
            .filter(|r| root.starts_with(r))
            .max_by_key(|r| r.components().count());
        let held = deepest.is_some_and(|d| indexed.iter().any(|r| **r == d));
        if !held {
            out.push(SourceCfg {
                name: folder_source(f),
                kind: if crate::as_owner::is_share(&root) {
                    SourceKindCfg::Network
                } else {
                    SourceKindCfg::Local
                },
                roots: vec![root],
                watch: !settings.folders_unwatched.contains(f),
            });
        }
    }
    out
}

/// How long a folder nothing reports changes for waits between walks.
pub fn folders_every(settings: &Settings) -> std::time::Duration {
    let minutes = if settings.folders_every == 0 {
        10
    } else {
        settings.folders_every
    };
    std::time::Duration::from_secs(u64::from(minutes) * 60)
}

/// A disk as a source, named by its id so a new label or letter keeps its rows.
fn source_of(v: &Volume) -> SourceCfg {
    SourceCfg {
        name: v.id.clone(),
        kind: match v.kind {
            Kind::System | Kind::Fixed => SourceKindCfg::Local,
            Kind::Removable | Kind::Optical => SourceKindCfg::Removable,
            Kind::Network => SourceKindCfg::Network,
        },
        roots: v.roots.iter().map(PathBuf::from).collect(),
        watch: true,
    }
}

/// Every other source's root that lies under one of this source's roots.
fn with_skips(sources: Vec<SourceCfg>) -> Vec<(SourceCfg, Vec<PathBuf>)> {
    let all: Vec<(usize, PathBuf)> = sources
        .iter()
        .enumerate()
        .flat_map(|(i, s)| s.roots.iter().map(move |r| (i, r.clone())))
        .collect();
    sources
        .into_iter()
        .enumerate()
        .map(|(i, s)| {
            let skip = all
                .iter()
                .filter(|(j, r)| {
                    *j != i && s.roots.iter().any(|mine| r != mine && r.starts_with(mine))
                })
                .map(|(_, r)| r.clone())
                .collect();
            (s, skip)
        })
        .collect()
}

#[cfg(test)]
mod tests;
