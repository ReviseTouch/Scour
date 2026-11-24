use super::*;

/// A damaged index is moved aside and a new one opened, so the service
/// comes up and scans; it used to fail to start for as long as it was tried.
#[test]
fn a_damaged_index_is_moved_aside_and_built_again() {
    let root = tempfile::tempdir().expect("tempdir");
    let dir = root.path().join("native");
    drop(NativeIndex::open_or_create(&dir).expect("a new index"));
    std::fs::write(dir.join("native-index.json"), "{ not json").expect("damage it");
    let opened = open_index(&dir);
    assert!(opened.is_ok(), "{:?}", opened.err());
    let aside = root.path().join("native.damaged");
    assert!(
        aside.join("native-index.json").is_file(),
        "the damaged files are kept"
    );
    // A second time keeps one copy, the newest.
    drop(opened);
    std::fs::write(dir.join("native-index.json"), "{ again").expect("damage it");
    assert!(open_index(&dir).is_ok());
    let kept = std::fs::read_to_string(aside.join("native-index.json")).expect("read");
    assert_eq!(kept, "{ again");
}

/// The rules with nothing switched off: never `scan_options`, which reads
/// the real settings file and makes a test depend on the desktop it runs on.
fn plain(config: &Config) -> scour_core::ScanOptions {
    scan_options_with(config, &scour_settings::Settings::default())
}

#[test]
fn platform_exclusions_are_added_to_the_configured_ones() {
    let mut c = Config::default();
    c.exclude.dirs = vec!["my-own".into()];
    let o = plain(&c);
    assert!(
        o.exclude_dirs.iter().any(|d| d == "my-own"),
        "the user's rule survives"
    );
    assert!(
        o.exclude_dirs.iter().any(|d| d == "node_modules"),
        "the platform's does too"
    );
}

/// Whatever list a rule is in, switching it off takes it out of force: only
/// what a window wrote can be deleted, so the switch has to reach all three.
#[test]
fn a_rule_switched_off_is_not_in_force_whichever_list_it_is_in() {
    let mut c = Config::default();
    c.exclude.dirs = vec!["from-the-file".into()];
    let added = scour_settings::Settings {
        exclude_dirs: vec!["from-a-window".into()],
        exclude_off: vec![
            "dir:node_modules".into(), // built in
            "dir:from-the-file".into(),
            "dir:from-a-window".into(),
        ],
        ..Default::default()
    };
    let o = scan_options_with(&c, &added);
    for gone in ["node_modules", "from-the-file", "from-a-window"] {
        assert!(
            !o.exclude_dirs.iter().any(|d| d == gone),
            "{gone} is switched off and still being skipped: {:?}",
            o.exclude_dirs
        );
    }
    // And the ones nobody touched are untouched.
    assert!(o.exclude_dirs.iter().any(|d| d == "__pycache__"));
}

/// A rule written in two places is off, not half-off: subtracting at the
/// end rather than per source is what makes that true.
#[test]
fn switching_off_a_rule_written_twice_switches_off_both_copies() {
    let mut c = Config::default();
    c.exclude.dirs = vec!["target".into()];
    let added = scour_settings::Settings {
        exclude_off: vec!["dir:target".into()],
        ..Default::default()
    };
    let o = scan_options_with(&c, &added);
    assert!(
        !o.exclude_dirs
            .iter()
            .any(|d| d.eq_ignore_ascii_case("target")),
        "one copy of the rule survived the switch: {:?}",
        o.exclude_dirs
    );
}

/// What the service writes about itself cannot be switched back on: `deny`
/// is a separate list because an `allow` rule reopened the loop once.
#[test]
fn the_switches_cannot_reopen_the_index_to_itself() {
    let c = Config::default();
    let dir = index_dir(&c).to_string_lossy().into_owned();
    let added = scour_settings::Settings {
        // Every spelling of it somebody might reach for.
        exclude_off: vec![
            format!("path:{dir}"),
            format!("dir:{dir}"),
            format!("deny:{dir}"),
        ],
        ..Default::default()
    };
    let o = scan_options_with(&c, &added);
    let rules = scour_source_fs::Rules::from_options(&o);
    assert!(
        rules.excludes_path(&format!("{dir}/seg-00000001.cols")),
        "a switch reopened the index to itself: {:?}",
        o.deny
    );
}

#[test]
fn scour_never_indexes_what_it_writes_about_itself() {
    // The web window's browser profile is under the data directory, and
    // watched it kept the page waking itself at 8.25% of a core.
    let c = Config::default();
    let o = plain(&c);
    let rules = scour_source_fs::Rules::from_options(&o);
    let data = scour_config::data_dir();
    assert!(
        rules.excludes_path(&format!(
            "{}/app/Default/Cache/Cache_Data/x_0",
            data.display()
        )),
        "the window's own browser cache is indexed: {:?}",
        o.deny
    );

    // The configured directory, not the one `ProjectDirs` names: under
    // `--config` the two disagree, and the default is where they do not.
    let mut elsewhere = Config::default();
    elsewhere.index.dir = "/srv/scour-elsewhere/index".into();
    let o = plain(&elsewhere);
    let rules = scour_source_fs::Rules::from_options(&o);
    assert!(
        rules.excludes_path("/srv/scour-elsewhere/app/Default/Cache/Cache_Data/x_0"),
        "this service's own browser cache is indexed: {:?}",
        o.deny
    );
    assert!(
        !o.deny.iter().any(|p| std::path::Path::new(p) == data),
        "a service living elsewhere still refuses the real data directory: {:?}",
        o.deny
    );
}

#[test]
fn the_index_never_indexes_itself() {
    // The commit/watch/index loop: 62% of a core on an idle machine, and
    // two thirds of every filesystem event.
    let c = Config::default();
    let o = plain(&c);
    let dir = index_dir(&c).to_string_lossy().into_owned();
    assert!(
        o.deny.iter().any(|p| dir.starts_with(p.as_str())),
        "the index directory {dir} is not excluded: {:?}",
        o.deny
    );
    // And an allow rule cannot take it back; that is what `deny` is for.
    let mut c2 = Config::default();
    c2.exclude.allow = vec!["/".into()];
    let o2 = plain(&c2);
    let rules = scour_source_fs::Rules::from_options(&o2);
    assert!(
        rules.excludes_path(&format!("{dir}/seg-00000001.cols")),
        "an allow rule reopened the index to itself"
    );
}

#[test]
fn a_rule_is_not_duplicated_when_it_is_already_there() {
    let mut c = Config::default();
    c.exclude.dirs = vec!["NODE_MODULES".into()];
    let o = plain(&c);
    assert_eq!(
        o.exclude_dirs
            .iter()
            .filter(|d| d.eq_ignore_ascii_case("node_modules"))
            .count(),
        1
    );
}
