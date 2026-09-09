//! What `traits_of` believes about a filesystem, against what it actually does.
//! `cargo run -p scour-source-fs --example filesystems <dir>...`, or with no
//! arguments every writable filesystem mounted. A wrong table entry is a silently
//! wrong index, so this writes files and finds out; `scripts/fsmatrix.sh` does the rest.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use scour_source_fs::fs::{FsTraits, medium_of, traits_of};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let dirs: Vec<PathBuf> = if args.is_empty() {
        writable_mounts()
    } else {
        args.iter().map(PathBuf::from).collect()
    };

    println!(
        "{:<26} {:<10} {:<9} {:<20} measured",
        "path", "type", "medium", "claimed"
    );
    for d in &dirs {
        let claimed = traits_of(d);
        let medium = medium_of(d);
        let measured = probe(d);
        let agree = if measured.as_ref().is_none_or(|m| m.agrees_with(&claimed)) {
            ""
        } else {
            "   <-- DISAGREES"
        };
        println!(
            "{:<26} {:<10} {:<9?} {:<20} {}{}",
            short(d),
            fstype(d),
            medium,
            describe(&claimed),
            measured
                .as_ref()
                .map_or("not writable".into(), Measured::show),
            agree
        );
    }
}

/// What a probe actually found. `None` for anything not measurable here.
struct Measured {
    case_sensitive: bool,
    distinct_ids: bool,
    id_survives_rename: bool,
    real_modes: bool,
}

impl Measured {
    fn agrees_with(&self, claimed: &FsTraits) -> bool {
        // Only one direction is a defect: claiming *less* than the filesystem offers
        // is the conservative choice, claiming more is the silent wrong index.
        !(claimed.case_sensitive && !self.case_sensitive)
            && !(claimed.real_modes && !self.real_modes)
    }

    /// `ids` and `rename` are reported and nothing is claimed against them: a row is
    /// identified by its path, so nothing asks the filesystem for an identity.
    fn show(&self) -> String {
        format!(
            "case={} ids={} rename={} chmod={}",
            yes(self.case_sensitive),
            yes(self.distinct_ids),
            yes(self.id_survives_rename),
            yes(self.real_modes)
        )
    }
}

fn yes(b: bool) -> &'static str {
    if b { "yes" } else { "NO " }
}

fn describe(t: &FsTraits) -> String {
    format!("case={} exec={}", yes(t.case_sensitive), yes(t.real_modes))
}

fn probe(dir: &Path) -> Option<Measured> {
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::fs::PermissionsExt;

    let root = dir.join(".scour-fs-probe");
    std::fs::create_dir_all(&root).ok()?;
    let cleanup = || {
        let _ = std::fs::remove_dir_all(&root);
    };

    // Case. Written in one spelling, opened in another.
    let mixed = root.join("ScourCase.probe");
    if std::fs::write(&mixed, b"x").is_err() {
        cleanup();
        return None;
    }
    let case_sensitive = std::fs::metadata(root.join("scourcase.probe")).is_err();

    // Identities. A hundred files, and how many distinct `(dev, ino)` pairs they have.
    let mut ids: HashSet<(u64, u64)> = HashSet::new();
    for i in 0..100 {
        let p = root.join(format!("id{i}.probe"));
        if std::fs::write(&p, b"x").is_err() {
            break;
        }
        if let Ok(md) = std::fs::metadata(&p) {
            ids.insert((md.dev(), md.ino()));
        }
    }
    let distinct_ids = ids.len() == 100;

    // Rename. The same file under a new name has to be the same file.
    let before = root.join("id0.probe");
    let after = root.join("id0-renamed.probe");
    let was = std::fs::metadata(&before).map(|m| (m.dev(), m.ino())).ok();
    let id_survives_rename = std::fs::rename(&before, &after).is_ok()
        && std::fs::metadata(&after)
            .map(|m| Some((m.dev(), m.ino())) == was)
            .unwrap_or(false);

    // Modes. A filesystem with none of its own takes what the mount says and ignores
    // a chmod, so asking for one and reading it back is the whole test.
    let m = root.join("mode.probe");
    let _ = std::fs::write(&m, b"x");
    let real_modes = std::fs::set_permissions(&m, std::fs::Permissions::from_mode(0o600)).is_ok()
        && std::fs::metadata(&m)
            .map(|md| md.permissions().mode() & 0o777 == 0o600)
            .unwrap_or(false);

    cleanup();
    Some(Measured {
        case_sensitive,
        distinct_ids,
        id_survives_rename,
        real_modes,
    })
}

/// Every mount that looks worth probing: real filesystems, not kernel
/// interfaces, and not something already covered by its parent.
fn writable_mounts() -> Vec<PathBuf> {
    let skip = [
        "proc",
        "sysfs",
        "devtmpfs",
        "devpts",
        "cgroup2",
        "securityfs",
        "debugfs",
        "tracefs",
        "pstore",
        "efivarfs",
        "bpf",
        "configfs",
        "hugetlbfs",
        "mqueue",
        "fusectl",
        "binfmt_misc",
        "autofs",
        "binder",
        "ramfs",
    ];
    let Ok(info) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return Vec::new();
    };
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::new();
    for line in info.lines() {
        let Some((_, after)) = line.split_once(" - ") else {
            continue;
        };
        let mut tail = after.split_whitespace();
        let Some(fstype) = tail.next() else { continue };
        if skip.contains(&fstype) {
            continue;
        }
        let target = line.split_whitespace().nth(4).unwrap_or("");
        if target.is_empty() || !seen.insert(fstype.to_owned()) {
            continue;
        }
        let p = PathBuf::from(target);
        // Writable, or there is nothing to measure.
        if std::fs::metadata(&p).is_ok() {
            out.push(p);
        }
    }
    out
}

fn fstype(p: &Path) -> String {
    std::process::Command::new("findmnt")
        .args(["-nro", "FSTYPE", "--target"])
        .arg(p)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "?".into())
}

fn short(p: &Path) -> String {
    let s = p.to_string_lossy();
    if s.len() > 25 {
        format!("…{}", &s[s.len() - 24..])
    } else {
        s.into_owned()
    }
}
