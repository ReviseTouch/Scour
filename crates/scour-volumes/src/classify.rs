//! Which mounts are volumes a person would call a disk, and of what kind.

use crate::mountinfo::Mount;
use crate::volume::{Kind, Volume};

/// What only the machine can say about a mounted device.
pub trait Probe {
    /// The filesystem's UUID and label, by the device it was mounted from.
    fn names(&self, source: &str) -> (Option<String>, Option<String>);
    /// Plugged in rather than built in: USB, a card reader, an optical drive.
    fn removable(&self, source: &str) -> bool;
    /// Size in bytes; asked only of volumes that are not on a network.
    fn size(&self, at: &str) -> u64;
}

/// Kernel bookkeeping, memory and images: mounted, but nothing a person keeps
/// files on. A container's or a snap's filesystem is one of these too.
const NOT_DISKS: &[&str] = &[
    "proc",
    "sysfs",
    "devtmpfs",
    "devpts",
    "tmpfs",
    "ramfs",
    "securityfs",
    "cgroup",
    "cgroup2",
    "pstore",
    "efivarfs",
    "bpf",
    "tracefs",
    "debugfs",
    "configfs",
    "fusectl",
    "mqueue",
    "hugetlbfs",
    "autofs",
    "binfmt_misc",
    "rpc_pipefs",
    "nsfs",
    "overlay",
    "squashfs",
    "erofs",
    "fuse.portal",
    "fuse.gvfsd-fuse",
    "fuse.xdg-document-portal",
    "fuse.lxcfs",
    "fuse.snapfuse",
    "zram",
];

/// Reached over a network.
const NETWORK: &[&str] = &[
    "nfs",
    "nfs4",
    "cifs",
    "smb3",
    "smbfs",
    "9p",
    "fuse.sshfs",
    "fuse.rclone",
    "davfs",
    "ceph",
    "glusterfs",
    "fuse.glusterfs",
    "afs",
];

/// Where the system keeps what it mounts for itself. `/run/media` is the
/// exception: that is where a desktop mounts a plugged-in disk.
const SYSTEM_PLACES: &[&str] = &[
    "/proc",
    "/sys",
    "/dev",
    "/run",
    "/boot",
    "/efi",
    "/snap",
    "/tmp",
    "/var/lib/docker",
    "/var/lib/containers",
    "/var/lib/snapd",
];

/// The volumes among these mounts: one per filesystem, however many places
/// it is mounted. A mount showing part of a filesystem another mount already
/// shows — a bind mount — is left out.
pub fn volumes(mounts: &[Mount], probe: &dyn Probe) -> Vec<Volume> {
    let mut kept: Vec<&Mount> = Vec::new();
    for m in mounts.iter().filter(|m| is_disk(m)) {
        if !kept
            .iter()
            .any(|k| k.source == m.source && within(&m.root, &k.root))
        {
            kept.push(m);
        }
    }
    let mut out: Vec<Volume> = Vec::new();
    for m in kept {
        let v = volume(m, probe);
        match out.iter_mut().find(|o| o.id == v.id) {
            Some(same) => join(same, v),
            None => out.push(v),
        }
    }
    out
}

/// One more place the same filesystem is mounted: kept as a root unless it is
/// inside one already, and the volume is the system's if either place is.
fn join(into: &mut Volume, more: Volume) {
    for r in more.roots {
        if into.roots.iter().any(|have| within(&r, have)) {
            continue;
        }
        into.roots.retain(|have| !within(have, &r));
        into.roots.push(r);
    }
    if more.kind == Kind::System {
        into.kind = Kind::System;
    }
}

fn is_disk(m: &Mount) -> bool {
    if NOT_DISKS.contains(&m.fs.as_str()) {
        return false;
    }
    let hidden = SYSTEM_PLACES.iter().any(|p| within(&m.at, p));
    !hidden || within(&m.at, "/run/media")
}

fn volume(m: &Mount, probe: &dyn Probe) -> Volume {
    let network = NETWORK.contains(&m.fs.as_str());
    let (uuid, label) = if network {
        (None, None)
    } else {
        probe.names(&m.source)
    };
    let kind = if m.at == "/" {
        Kind::System
    } else if network {
        Kind::Network
    } else if probe.removable(&m.source)
        || within(&m.at, "/run/media")
        || within(&m.at, "/media")
        || matches!(m.fs.as_str(), "iso9660" | "udf")
    {
        Kind::Removable
    } else {
        Kind::Fixed
    };
    let id = match (network, uuid) {
        (true, _) => format!("net:{}", m.source),
        (false, Some(uuid)) => format!("uuid:{uuid}"),
        (false, None) => format!("dev:{}", m.source),
    };
    Volume {
        id,
        roots: vec![m.at.clone()],
        label: label.unwrap_or_default(),
        kind,
        fs: m.fs.clone(),
        size: if network { 0 } else { probe.size(&m.at) },
    }
}

/// `path` is `base` or somewhere under it.
fn within(path: &str, base: &str) -> bool {
    let base = base.trim_end_matches('/');
    path == base || base.is_empty() || path.strip_prefix(base).is_some_and(|r| r.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn within_means_the_place_or_under_it_and_nothing_beside_it() {
        assert!(within("/run/media/u", "/run/media"));
        assert!(within("/run", "/run"));
        assert!(!within("/runner", "/run"));
        assert!(within("/anything", "/"));
    }
}
