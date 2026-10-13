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
    /// Each with the roots of other sources under its own, which it leaves alone.
    pub sources: Vec<(SourceCfg, Vec<PathBuf>)>,
    pub disks: Vec<Disk>,
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
/// out, when nobody wrote any; plugged-in and network disks only once chosen.
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
    let mut disks = Vec::new();
    for v in volumes {
        let state = state_of(v, &given, config.sources_given(), settings, later);
        if matches!(state, DiskState::Auto | DiskState::Chosen) {
            sources.push(source_of(v));
        }
        disks.push(Disk {
            volume: v.clone(),
            state,
        });
    }
    if sources.is_empty() && volumes.is_empty() {
        sources = config.sources.clone();
    }
    let sources = with_skips(sources);
    Plan { sources, disks }
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
    } else if later.contains(&v.id) {
        DiskState::Later
    } else {
        DiskState::Asking
    }
}

/// A disk as a source, named by its id so a new label or letter keeps its rows.
fn source_of(v: &Volume) -> SourceCfg {
    SourceCfg {
        name: v.id.clone(),
        kind: match v.kind {
            Kind::System | Kind::Fixed => SourceKindCfg::Local,
            Kind::Removable => SourceKindCfg::Removable,
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
mod tests {
    use super::*;

    fn disk(id: &str, root: &str, kind: Kind) -> Volume {
        Volume {
            id: id.into(),
            roots: vec![root.into()],
            kind,
            ..Volume::default()
        }
    }

    fn machine() -> Vec<Volume> {
        vec![
            disk("uuid:sys", "/", Kind::System),
            disk("uuid:depo", "/mnt/depo", Kind::Fixed),
            disk("uuid:usb", "/run/media/u/Yedek", Kind::Removable),
            disk("net:nas:/x", "/srv/nas", Kind::Network),
        ]
    }

    fn states(p: &Plan) -> Vec<(&str, DiskState)> {
        p.disks
            .iter()
            .map(|d| (d.volume.id.as_str(), d.state))
            .collect()
    }

    fn names(p: &Plan) -> Vec<&str> {
        p.sources.iter().map(|(s, _)| s.name.as_str()).collect()
    }

    #[test]
    fn with_nothing_written_the_built_in_disks_are_indexed_and_the_rest_asked() {
        let p = plan(
            &Config::default(),
            &machine(),
            &Settings::default(),
            &HashSet::new(),
        );
        assert_eq!(
            states(&p),
            [
                ("uuid:sys", DiskState::Auto),
                ("uuid:depo", DiskState::Auto),
                ("uuid:usb", DiskState::Asking),
                ("net:nas:/x", DiskState::Asking),
            ]
        );
        assert_eq!(names(&p), ["uuid:sys", "uuid:depo"]);
        assert_eq!(p.asking(), 2);
        // `/` leaves `/mnt/depo` to its own source.
        assert_eq!(p.sources[0].1, [PathBuf::from("/mnt/depo")]);
        assert!(p.sources[1].1.is_empty());
    }

    #[test]
    fn a_no_takes_a_disk_out_a_yes_puts_one_in_and_later_stops_the_asking() {
        let settings = Settings {
            volumes_off: vec!["uuid:depo".into()],
            volumes_on: vec!["uuid:usb".into()],
            ..Settings::default()
        };
        let later = HashSet::from(["net:nas:/x".to_string()]);
        let p = plan(&Config::default(), &machine(), &settings, &later);
        assert_eq!(
            states(&p),
            [
                ("uuid:sys", DiskState::Auto),
                ("uuid:depo", DiskState::Off),
                ("uuid:usb", DiskState::Chosen),
                ("net:nas:/x", DiskState::Later),
            ]
        );
        assert_eq!(names(&p), ["uuid:sys", "uuid:usb"]);
        assert_eq!(p.sources[1].0.kind, SourceKindCfg::Removable);
        assert_eq!(p.asking(), 0);
    }

    /// This machine's own configuration: written by hand, so it is what is
    /// indexed, and the disks it does not name wait to be chosen.
    #[test]
    fn sources_written_by_hand_are_kept_and_built_in_disks_wait_to_be_chosen() {
        let config = Config {
            sources: vec![
                SourceCfg {
                    name: "home".into(),
                    roots: vec!["/home/u".into()],
                    ..SourceCfg::default()
                },
                SourceCfg {
                    name: "depo".into(),
                    roots: vec!["/mnt/depo".into()],
                    ..SourceCfg::default()
                },
            ],
            ..Config::default()
        };
        let p = plan(&config, &machine(), &Settings::default(), &HashSet::new());
        assert_eq!(states(&p)[0], ("uuid:sys", DiskState::Manual));
        assert_eq!(states(&p)[1], ("uuid:depo", DiskState::Configured));
        assert_eq!(names(&p), ["home", "depo"]);

        // Chosen after all: `/` joins, and leaves the written ones alone.
        let settings = Settings {
            volumes_on: vec!["uuid:sys".into()],
            ..Settings::default()
        };
        let p = plan(&config, &machine(), &settings, &HashSet::new());
        assert_eq!(names(&p), ["home", "depo", "uuid:sys"]);
        assert_eq!(
            p.sources[2].1,
            [PathBuf::from("/home/u"), PathBuf::from("/mnt/depo")]
        );
    }

    #[test]
    fn with_no_disk_found_the_configurations_own_default_is_indexed() {
        let config = Config {
            sources: vec![SourceCfg {
                roots: vec!["/home/u".into()],
                ..SourceCfg::default()
            }],
            ..Config::default()
        };
        let p = plan(&config, &[], &Settings::default(), &HashSet::new());
        assert_eq!(p.sources.len(), config.sources.len());
        assert!(p.disks.is_empty());
    }
}
