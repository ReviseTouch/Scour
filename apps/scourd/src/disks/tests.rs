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
    p.sources.iter().map(|p| p.source.name.as_str()).collect()
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
    // `/` leaves `/mnt/depo` to its own source, and the two waiting alone.
    assert_eq!(
        p.sources[0].skip,
        ["/mnt/depo", "/run/media/u/Yedek", "/srv/nas"].map(PathBuf::from)
    );
    assert!(p.sources[1].skip.is_empty());
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
    assert_eq!(p.sources[1].source.kind, SourceKindCfg::Removable);
    // A disk left out or not yet answered for is not walked through `/`.
    assert_eq!(
        p.sources[0].skip,
        ["/run/media/u/Yedek", "/mnt/depo", "/srv/nas"].map(PathBuf::from)
    );
    assert_eq!(p.asking(), 0);
}

/// What was said for a kind of disk answers for every one of that kind
/// nobody answered for; an answer for the disk itself still wins.
#[test]
fn a_rule_for_plugged_in_and_network_disks_answers_for_them() {
    let settings = Settings {
        removable: "always".into(),
        network: "never".into(),
        ..Settings::default()
    };
    let p = plan(&Config::default(), &machine(), &settings, &HashSet::new());
    assert_eq!(
        states(&p),
        [
            ("uuid:sys", DiskState::Auto),
            ("uuid:depo", DiskState::Auto),
            ("uuid:usb", DiskState::Chosen),
            ("net:nas:/x", DiskState::Off),
        ]
    );
    assert_eq!(p.asking(), 0);
    let settings = Settings {
        removable: "always".into(),
        volumes_off: vec!["uuid:usb".into()],
        ..Settings::default()
    };
    let p = plan(&Config::default(), &machine(), &settings, &HashSet::new());
    assert!(states(&p).contains(&("uuid:usb", DiskState::Off)));
}

/// A folder added from a face is a source of its own, unless an indexed
/// disk holds it already; and a relative path is not a folder.
#[test]
fn an_added_folder_is_indexed_unless_a_disk_already_is() {
    let settings = Settings {
        volumes_off: vec!["uuid:depo".into()],
        folders: vec![
            "/mnt/depo/PROJELER".into(),
            "/etc".into(),
            "relative/path".into(),
            "/mnt/depo/PROJELER/inside".into(),
        ],
        ..Settings::default()
    };
    let p = plan(&Config::default(), &machine(), &settings, &HashSet::new());
    assert_eq!(names(&p), ["uuid:sys", "folder:/mnt/depo/PROJELER"]);
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
        p.sources[2].skip,
        ["/home/u", "/mnt/depo", "/run/media/u/Yedek", "/srv/nas"].map(PathBuf::from)
    );
}

/// One partition with everything on it, and nothing written: the home
/// folder is the old default source, apart, and `/` is the system's.
#[test]
fn the_home_folder_inside_the_system_disk_is_a_source_of_its_own() {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return;
    };
    let config = Config {
        sources: vec![SourceCfg {
            name: "home".into(),
            roots: vec![home.clone()],
            ..SourceCfg::default()
        }],
        ..Config::default()
    };
    assert!(!config.sources_given(), "the default is not a choice");
    let p = plan(
        &config,
        &[disk("uuid:sys", "/", Kind::System)],
        &Settings::default(),
        &HashSet::new(),
    );
    assert_eq!(names(&p), ["home", "uuid:sys"]);
    assert!(!p.sources[0].system && p.sources[1].system);
    assert_eq!(p.sources[1].skip, [home]);

    // Taken out with its disk.
    let settings = Settings {
        volumes_off: vec!["uuid:sys".into()],
        ..Settings::default()
    };
    let p = plan(
        &config,
        &[disk("uuid:sys", "/", Kind::System)],
        &settings,
        &HashSet::new(),
    );
    assert!(p.sources.is_empty());
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
