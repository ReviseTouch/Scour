//! The workspace's dependency rule, checked: `AGENTS.md` states it and
//! `Cargo.toml` says it in a comment, and neither fails a build. Lives here
//! because `scourd` is where the layers are joined.

use std::collections::BTreeMap;
use std::process::Command;

/// Linked only by the service; a face that needs one is missing a request.
const SERVICE_ONLY: &[&str] = &[
    "scour-engine",
    "scour-index-native",
    "scour-source-fs",
    "scour-watch",
    "scour-mock",
];

/// Reached by the engine on the service's behalf; a face asks over the socket.
const BEHIND_THE_ENGINE: &[&str] = &["scour-grep", "scour-dupes", "scour-export"];

const FACES: &[&str] = &["scour", "scour-gui", "scour-tui", "scour-web", "scour-mcp"];

/// Crates that know nothing of Scour, so another program could take them.
/// They may share two: `scour-extract`, which reads containers, and
/// `scour-flow`, the model of a document that flows (`serde` alone), which the
/// extractor's reader fills.
const LEAVES: &[&str] = &[
    "scour-chart",
    "scour-clip",
    "scour-drag",
    "scour-doctext",
    "scour-dupes",
    "scour-extract",
    "scour-flow",
    "scour-grep",
    "scour-hotkey",
    "scour-icon",
    "scour-launch",
    "scour-name",
    "scour-openers",
    "scour-places",
    "scour-preview",
    "scour-sendto",
    "scour-settings",
    "scour-textcache",
    "scour-thumbs",
    "scour-trash",
    "scour-volumes",
];
const LEAVES_MAY_USE: &[&str] = &["scour-extract", "scour-flow"];

/// Every workspace crate and the `scour-*` crates it links outside tests and
/// build scripts, on any target.
fn edges() -> BTreeMap<String, Vec<String>> {
    let out = Command::new(env!("CARGO"))
        .args([
            "metadata",
            "--format-version",
            "1",
            "--no-deps",
            "--offline",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("cargo metadata");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let meta: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let mut edges = BTreeMap::new();
    for package in meta["packages"].as_array().unwrap() {
        let name = package["name"].as_str().unwrap().to_string();
        let linked = package["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| d["kind"].is_null())
            .filter_map(|d| d["name"].as_str())
            .filter(|d| d.starts_with("scour"))
            .map(str::to_string)
            .collect();
        edges.insert(name, linked);
    }
    edges
}

#[test]
fn every_crate_keeps_to_its_layer() {
    let edges = edges();
    let mut broken = Vec::new();
    for (from, linked) in &edges {
        for to in linked {
            let why = if SERVICE_ONLY.contains(&to.as_str()) && from != "scourd" {
                Some("only the service names the engine, an index or a source")
            } else if FACES.contains(&from.as_str()) && BEHIND_THE_ENGINE.contains(&to.as_str()) {
                Some("a face asks the service; it does not read files itself")
            } else if LEAVES.contains(&from.as_str()) && !LEAVES_MAY_USE.contains(&to.as_str()) {
                Some("a leaf crate depends on nothing of Scour's")
            } else if from == "scour-core" {
                Some("the contract depends on nothing")
            } else {
                None
            };
            if let Some(why) = why {
                broken.push(format!("{from} → {to}: {why}"));
            }
        }
    }
    assert!(broken.is_empty(), "{}", broken.join("\n"));
}

#[test]
fn the_lists_name_real_crates() {
    let edges = edges();
    for name in SERVICE_ONLY
        .iter()
        .chain(BEHIND_THE_ENGINE)
        .chain(FACES)
        .chain(LEAVES)
    {
        assert!(edges.contains_key(*name), "{name} is not in the workspace");
    }
}
