use super::*;

fn build(paths: &[&str]) -> (Vec<u8>, Vec<u32>) {
    let mut w = DirWriter::new();
    for p in paths {
        w.intern(p);
    }
    w.finish()
}

#[test]
fn every_path_comes_back() {
    let paths = [
        "/home/u",
        "/home/u/Projeler",
        "/home/u/Projeler/Scour",
        "/home/u/Projeler/Scour/crates",
        "/home/u/Projeler/Scour/crates/scour-core",
        "/home/u/Projeler/Scour/crates/scour-core/src",
        "/home/u/Projeler/Scour/crates/scour-core/tests",
        "/home/u/Belgeler",
        "/etc",
    ];
    let (bytes, remap) = build(&paths);
    let table = DirTable::open(&bytes).expect("open");
    assert_eq!(table.len(), paths.len());
    for (provisional, path) in paths.iter().enumerate() {
        assert_eq!(table.get(remap[provisional]).as_deref(), Some(*path));
    }
    assert_eq!(table.get(paths.len() as u32), None);
}

#[test]
fn interning_the_same_directory_twice_costs_nothing() {
    let mut w = DirWriter::new();
    let a = w.intern("/home/u");
    let b = w.intern("/home/u");
    assert_eq!(a, b);
    assert_eq!(w.len(), 1);
}

#[test]
fn a_subtree_survives_a_sibling_that_sorts_between_it_and_its_children() {
    // `/a-x` sorts after `/a` and before `/a/y`, so a walk that stops at
    // the first non-descendant never sees `/a/y`.
    let paths = [
        "/a",
        "/a-x",
        "/a-x/deep",
        "/a/x",
        "/a/x/deep",
        "/a/y",
        "/ab",
        "/ab/z",
        "/b",
    ];
    let (bytes, _) = build(&paths);
    let table = DirTable::open(&bytes).expect("open");

    let scope = table.subtree("/a");
    let mut got: Vec<String> = scope.ids().map(|i| table.get(i).expect("row")).collect();
    got.sort();
    assert_eq!(got, vec!["/a", "/a/x", "/a/x/deep", "/a/y"]);

    // Both near misses stay out.
    assert!(
        !got.iter().any(|p| p.starts_with("/ab")),
        "/ab is not inside /a"
    );
    assert!(
        !got.iter().any(|p| p.starts_with("/a-")),
        "/a-x is not inside /a"
    );

    assert_eq!(table.subtree("/b").len(), 1);
    assert!(table.subtree("/nowhere").is_empty());
    // A directory with no children is still itself.
    assert_eq!(table.subtree("/ab/z").len(), 1);
}

#[test]
fn an_exact_lookup_does_not_match_a_longer_name() {
    let paths = ["/a", "/ab", "/abc"];
    let (bytes, _) = build(&paths);
    let table = DirTable::open(&bytes).expect("open");
    assert_eq!(
        table.exact("/a").and_then(|i| table.get(i)).as_deref(),
        Some("/a")
    );
    assert_eq!(
        table.exact("/ab/").and_then(|i| table.get(i)).as_deref(),
        Some("/ab")
    );
    assert_eq!(table.exact("/abcd"), None);
}

#[test]
fn front_coding_actually_shrinks_it() {
    // 117.7 bytes a path down to a few, on a corpus-like shape.
    let mut paths = Vec::new();
    for a in 0..40 {
        for b in 0..40 {
            paths.push(format!(
                "/home/u/Projeler/project-{a:02}/crates/module-{b:02}/src"
            ));
        }
    }
    let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let raw: usize = paths.iter().map(String::len).sum();
    let (bytes, _) = build(&refs);
    assert!(
        bytes.len() * 4 < raw,
        "front coding should be several times smaller: {} vs {raw}",
        bytes.len()
    );

    // And it still reads back exactly.
    let table = DirTable::open(&bytes).expect("open");
    let mut all: Vec<String> = (0..table.len() as u32)
        .map(|i| table.get(i).expect("row"))
        .collect();
    all.sort();
    let mut want = paths.clone();
    want.sort();
    assert_eq!(all, want);
}

#[test]
fn non_ascii_paths_survive() {
    // A prefix must never be cut inside a character.
    let paths = [
        "/home/u/Müzik",
        "/home/u/Müzik/Şarkılar",
        "/home/u/Belgeler/İş",
    ];
    let (bytes, remap) = build(&paths);
    let table = DirTable::open(&bytes).expect("open");
    for (i, p) in paths.iter().enumerate() {
        assert_eq!(table.get(remap[i]).as_deref(), Some(*p));
    }
}

#[test]
fn distance_counts_a_cache_as_further_than_a_project() {
    assert!(steps_of("/home/u/Projeler/Scour/crates") < steps_of("/home/u/.cargo/registry"));
    // A build directory is as far as a hidden one, counted per appearance.
    assert_eq!(steps_of("/home/u/p/target"), steps_of("/home/u/p/.git"));
    assert!(steps_of("/home/u/p/target/debug") > steps_of("/home/u/p/src/debug"));
    // Shallow and hidden is still close: `~/.config` is the user's writing.
    assert!(steps_of("/home/u/.config/fish") < steps_of("/home/u/.cargo/registry/src/crates"));
    // Case does not save a build directory, and the cap holds.
    assert_eq!(steps_of("/a/Target"), steps_of("/a/target"));
    assert_eq!(steps_of(&"/.x".repeat(100)), STEP_CAP as u8);
}

#[test]
fn the_distance_of_every_directory_comes_back_from_the_table() {
    let paths = [
        "/home/u",
        "/home/u/Projeler/Scour/src",
        "/home/u/.cargo/registry/src/crates.io/lzma-sys-0.1.20",
        "/home/u/Projeler/Scour/target/debug/build",
    ];
    let (bytes, remap) = build(&paths);
    let table = DirTable::open(&bytes).expect("open");
    for (i, p) in paths.iter().enumerate() {
        assert_eq!(table.steps(remap[i]), steps_of(p), "{p}");
    }
    // A number the table does not hold is no opinion, not a panic.
    assert_eq!(table.steps(9_999), 0);
}

#[test]
fn a_path_gives_up_its_directory() {
    assert_eq!(dir_part("/home/u/a.txt"), "/home/u");
    assert_eq!(dir_part("/a.txt"), "/");
    assert_eq!(dir_part("a.txt"), "");
}

/// Against brute force. One row out here is a search scoped to the wrong
/// folder, which returns files and looks like a working search. The sizes
/// straddle a restart boundary in both directions (`RESTART` is 16).
#[test]
fn the_first_row_not_below_a_prefix_is_the_one_brute_force_finds() {
    for n in [0usize, 1, 15, 16, 17, 31, 32, 33, 200] {
        let mut paths: Vec<String> = Vec::new();
        for i in 0..n {
            // The sibling that sorts between a directory and its children.
            paths.push(format!("/home/u/Projeler/p{i:03}"));
            if i % 3 == 0 {
                paths.push(format!("/home/u/Projeler/p{i:03}-yedek"));
                paths.push(format!("/home/u/Projeler/p{i:03}/src"));
            }
        }
        paths.sort();
        paths.dedup();
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let (bytes, _) = build(&refs);
        let t = DirTable::open(&bytes).expect("table");

        // Every stored path, extended, shortened, and two outside.
        let mut asked: Vec<String> = vec![String::new(), "/".into(), "~".into(), "/zzz".into()];
        for p in &paths {
            asked.push(p.clone());
            asked.push(format!("{p}/"));
            asked.push(format!("{p}0"));
            asked.push(p[..p.len() - 1].to_owned());
        }
        for q in &asked {
            let brute = paths.partition_point(|p| p.as_str() < q.as_str()) as u32;
            assert_eq!(t.lower_bound(q), brute, "n={n} prefix={q:?}");
        }
    }
}

#[test]
fn an_empty_table_is_not_a_panic() {
    let (bytes, remap) = build(&[]);
    let table = DirTable::open(&bytes).expect("open");
    assert!(table.is_empty());
    assert_eq!(table.get(0), None);
    assert!(table.subtree("/a").is_empty());
    assert!(remap.is_empty());
    assert_eq!(DirTable::open(&[1, 2, 3]).map(|t| t.len()), None);
}
