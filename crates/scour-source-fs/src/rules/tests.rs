use super::*;

fn rules() -> Rules {
    Rules::from_options(&ScanOptions {
        exclude_paths: vec!["/proc".into(), "/home/u/big".into()],
        exclude_dirs: vec!["node_modules".into(), "TARGET".into()],
        exclude_files: vec![".DS_Store".into()],
        allow: vec!["/home/u/big/keep".into()],
        ..Default::default()
    })
}

/// `target` excluded the way the defaults exclude it, plus the allow rules.
fn allowing(allow: &[&str]) -> Rules {
    Rules::from_options(&ScanOptions {
        exclude_dirs: vec!["target".into()],
        allow: allow.iter().map(|s| s.to_string()).collect(),
        ..Default::default()
    })
}

#[test]
fn a_sequence_takes_back_the_binaries_without_the_build() {
    let r = allowing(&["target/release"]);
    // The walk has to be let into `target` to reach `release`.
    assert!(
        r.excludes("/p/target", "target", true),
        "still excluded itself"
    );
    assert!(r.may_contain_allowed("/p/target"), "but walked into");
    // What was asked for comes back…
    assert!(!r.excludes("/p/target/release", "release", true));
    assert!(!r.excludes("/p/target/release/app", "app", false));
    // …and its siblings do not come with it, which is the whole point.
    assert!(r.excludes("/p/target/debug/app", "app", false));
    assert!(
        r.excludes("/p/target/release/deps", "deps", true),
        "nor its depths"
    );
    assert!(r.excludes("/p/target/.rustc_info.json", ".rustc_info.json", false));
    // Every project, not one: the rule names a shape, not a place.
    assert!(!r.excludes("/other/deep/target/release/bin", "bin", false));
}

#[test]
fn a_path_allow_takes_back_one_place_only() {
    let r = allowing(&["/p/target/release"]);
    assert!(r.may_contain_allowed("/p/target"), "on the way");
    assert!(!r.excludes("/p/target/release/app", "app", false));
    assert!(r.excludes("/p/target/debug/app", "app", false));
    assert!(
        r.excludes("/q/target/release/app", "app", false),
        "another project"
    );
}

/// The rule as actually used: platform exclusions plus the shipped configuration.
#[test]
fn the_defaults_plus_one_line_bring_the_binaries_back() {
    let (paths, dirs, files) = platform_defaults();
    let r = Rules::from_options(&ScanOptions {
        exclude_paths: paths,
        exclude_dirs: dirs,
        exclude_files: files,
        allow: vec!["target/release".into(), "target/debug".into()],
        ..Default::default()
    });
    let bin = "/home/u/proj/target/release/app";
    assert!(r.may_contain_allowed("/home/u/proj/target"), "walked into");
    assert!(!r.excludes(bin, "app", false), "the binary is kept");
    assert!(!r.excludes_path(bin), "and the watcher agrees");
    assert!(
        r.excludes("/home/u/proj/target/release/deps", "deps", true),
        "and not the intermediate tree beside it"
    );
    assert!(r.excludes(
        "/home/u/proj/target/release/deps/lib.rlib",
        "lib.rlib",
        false
    ));
    assert!(r.excludes(
        "/home/u/proj/target/.rustc_info.json",
        ".rustc_info.json",
        false
    ));
}

#[test]
fn the_watcher_agrees_without_being_told_what_is_a_directory() {
    let r = allowing(&["target/release"]);
    assert!(!r.excludes_path("/p/target/release/app"));
    assert!(r.excludes_path("/p/target/debug/app"));
}

#[test]
fn names_are_matched_case_insensitively_and_by_type() {
    let r = rules();
    assert!(r.excludes("/a/b/node_modules", "node_modules", true));
    assert!(
        r.excludes("/a/b/Target", "Target", true),
        "rules are case-insensitive"
    );
    // A *file* called node_modules is not the directory rule's business.
    assert!(!r.excludes("/a/b/node_modules", "node_modules", false));
    assert!(r.excludes("/a/.DS_Store", ".DS_Store", false));
}

#[test]
fn a_rule_naming_two_components_excludes_the_tree_under_it() {
    // A rule with a `/` matches a component sequence, not a name: the scan
    // prunes the directory and the watcher judges everything below it.
    let rules = Rules::from_options(&ScanOptions {
        exclude_dirs: vec![".git/objects".into(), "node_modules".into()],
        ..Default::default()
    });

    // The scan is asked about the directory, and prunes it.
    assert!(rules.excludes("/p/.git/objects", "objects", true));
    // The watcher is asked about anything, with no walk behind it.
    assert!(rules.excludes_path("/p/.git/objects/aa/3f2b1c"));
    assert!(rules.excludes_path("/p/.git/objects"));

    // What must not be caught: the same last component under another parent.
    assert!(!rules.excludes("/p/build/objects", "objects", true));
    assert!(!rules.excludes_path("/p/build/objects/aa"));
    assert!(!rules.excludes_path("/p/.git/config"));
    // A misleading suffix is not a component boundary.
    assert!(!rules.excludes_path("/p/not.git/objects-old/x"));

    // And the one-component rules still work the cheap way.
    assert!(rules.excludes("/p/node_modules", "node_modules", true));
    assert!(rules.excludes_path("/p/node_modules/react/index.js"));
}

#[test]
fn an_allow_rule_wins() {
    let r = rules();
    assert!(r.excludes("/home/u/big/junk", "junk", true));
    assert!(!r.excludes("/home/u/big/keep", "keep", true));
    assert!(!r.excludes("/home/u/big/keep/deep/x.txt", "x.txt", false));
}

#[test]
fn an_excluded_directory_on_the_way_to_an_allowed_one_is_not_pruned() {
    // Otherwise the walk would stop at /home/u/big and never see the allow.
    let r = rules();
    assert!(r.may_contain_allowed("/home/u/big"));
    assert!(r.may_contain_allowed("/home/u"));
    assert!(!r.may_contain_allowed("/home/u/other"));
}

#[test]
fn platform_defaults_are_populated_and_sane() {
    let (paths, dirs, files) = platform_defaults();
    assert!(dirs.iter().any(|d| d == "node_modules"));
    // 28% of a developer's index and the loudest thing a compile does.
    assert!(dirs.iter().any(|d| d == "target"));
    // And the ones that are plausible names for real work stay out of it.
    for plausible in ["build", "dist", "out", "src"] {
        assert!(
            !dirs.iter().any(|d| d == plausible),
            "{plausible} is a name people use for their own work"
        );
    }
    assert!(files.iter().any(|f| f == ".DS_Store"));
    // Every default path must be absolute, or it would match nothing.
    assert!(
        paths.iter().all(|p| p.starts_with('/') || p.contains(':')),
        "{paths:?}"
    );
    assert!(dirs.iter().any(|d| d == ".cache"));
    assert!(dirs.iter().any(|d| d == ".local/share/Trash"));
    #[cfg(target_os = "linux")]
    {
        let mine = format!(".Trash-{}", unsafe { libc::getuid() });
        assert!(
            dirs.iter().any(|d| d == &mine),
            "the trash is named after this uid"
        );
        assert!(!dirs.iter().any(|d| d == ".Trash-1000") || mine == ".Trash-1000");
    }
}

/// Caches and the two trashes go, by name anywhere and by sequence in the home.
#[test]
fn caches_and_trashes_are_skipped_by_default() {
    let (paths, dirs, files) = platform_defaults();
    let r = Rules::from_options(&ScanOptions {
        exclude_paths: paths,
        exclude_dirs: dirs,
        exclude_files: files,
        ..Default::default()
    });
    assert!(r.excludes("/home/u/.cache", ".cache", true));
    assert!(r.excludes_path("/home/u/.cache/mozilla/firefox/cache2/entries/x"));
    assert!(
        r.excludes("/home/u/proj/.cache", ".cache", true),
        "a project's cache too"
    );
    assert!(r.excludes("/home/u/.local/share/Trash", "Trash", true));
    assert!(r.excludes_path("/home/u/.local/share/Trash/files/old.pdf"));
    assert!(
        !r.excludes("/home/u/.local/share", "share", true),
        "only the trash under it"
    );
    assert!(
        !r.excludes("/home/u/Trash", "Trash", true),
        "a folder somebody named Trash stays"
    );
    assert!(
        !r.excludes("/home/u/cache", "cache", true),
        "so does one named cache"
    );
}
