//! What `traits_of` believes about a filesystem, against what it actually does.
//!
//! `cargo run --release -p scour-source-fs --example filesystems <dir>...`
//!
//! With no arguments it tries every writable filesystem currently mounted.
//!
//! [`FsTraits`] is read from `statfs`'s magic number and a table, and a wrong
//! entry in that table is not a compile error or a crash — it is a silently
//! wrong index. Claiming `stable_ids` where `st_ino` is invented makes every
//! file its own duplicate after a remount; claiming `case_sensitive` where the
//! filesystem folds makes `Rapor.pdf` and `rapor.pdf` two rows for one file.
//!
//! So this does not trust the table. It writes files and finds out:
//!
//! * **case** — create `ScourCase.probe`, then try to open `scourcase.probe`.
//!   If it opens, two names are one file.
//! * **identities** — create a hundred files and count the distinct
//!   `(dev, ino)` pairs. On the FAT family `st_ino` is the driver's invention
//!   and repeats.
//! * **rename** — an identity that changes when a file is renamed is not an
//!   identity, and a rename then reaches the index as a delete and an add.
//!
//! Everything it makes, it removes.
//!
//! **The formats this cannot reach.** Mounting a loopback image needs real
//! `CAP_SYS_ADMIN` — a user namespace does not help, because ext4, btrfs, xfs,
//! vfat and exfat are not `FS_USERNS_MOUNT`. So the full matrix is
//! `scripts/fsmatrix.sh`, which needs `sudo`; this covers whatever the machine
//! already has mounted, which needs nothing.

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
}

impl Measured {
    fn agrees_with(&self, claimed: &FsTraits) -> bool {
        // Only one direction is a defect. Claiming *less* than the filesystem
        // offers is the deliberate conservative choice `FsTraits::UNKNOWN`
        // documents; claiming more is the silent wrong index.
        !(claimed.case_sensitive && !self.case_sensitive)
            && !(claimed.stable_ids && !(self.distinct_ids && self.id_survives_rename))
    }

    fn show(&self) -> String {
        format!(
            "case={} ids={} rename={}",
            yes(self.case_sensitive),
            yes(self.distinct_ids),
            yes(self.id_survives_rename)
        )
    }
}

fn yes(b: bool) -> &'static str {
    if b { "yes" } else { "NO " }
}

fn describe(t: &FsTraits) -> String {
    format!("case={} ids={}", yes(t.case_sensitive), yes(t.stable_ids))
}

fn probe(dir: &Path) -> Option<Measured> {
    use std::os::unix::fs::MetadataExt;

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

    // Identities. A hundred files, and how many distinct `(dev, ino)` pairs
    // they turn out to have.
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

    cleanup();
    Some(Measured {
        case_sensitive,
        distinct_ids,
        id_survives_rename,
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
