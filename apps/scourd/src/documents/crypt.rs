//! Whether a document's text may be written where the cache is: never from
//! an encrypted volume to one that is not. Where that cannot be known, only
//! a document on the cache's own volume is kept.

use std::path::Path;

pub(crate) fn may_keep(document: &Path, cache: &Path) -> bool {
    match (encrypted(document), encrypted(cache)) {
        (Some(false), _) | (Some(true), Some(true)) => true,
        _ => same_volume(document, cache),
    }
}

#[cfg(unix)]
fn same_volume(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(a), Ok(b)) => a.dev() == b.dev(),
        _ => false,
    }
}

/// The drive or share each is on, by its prefix: `C:`, `\\server\share`.
#[cfg(not(unix))]
fn same_volume(a: &Path, b: &Path) -> bool {
    let prefix = |p: &Path| {
        p.components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
    };
    prefix(a).is_some() && prefix(a) == prefix(b)
}

/// Linux: the block device under the mount holding `path` is, or stands on,
/// a dm-crypt mapping (LUKS, plain dm-crypt, LVM on LUKS).
#[cfg(target_os = "linux")]
fn encrypted(path: &Path) -> Option<bool> {
    let path = std::fs::canonicalize(path).ok()?;
    let mounts = std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    let source = mounts
        .lines()
        .filter_map(mount_and_source)
        .filter(|(at, _)| path.starts_with(at))
        .max_by_key(|(at, _)| at.len())
        .map(|(_, source)| source)?;
    if !source.starts_with("/dev/") {
        // A network, memory or FUSE filesystem: not ours to judge.
        return None;
    }
    let device = std::fs::canonicalize(&source).ok()?;
    let name = device.file_name()?.to_str()?.to_owned();
    Some(stands_on_crypt(&name, 0))
}

#[cfg(target_os = "linux")]
fn mount_and_source(line: &str) -> Option<(String, String)> {
    let fields: Vec<&str> = line.split(' ').collect();
    let at = unescape(fields.get(4)?);
    let dash = fields.iter().position(|f| *f == "-")?;
    Some((at, unescape(fields.get(dash + 2)?)))
}

#[cfg(target_os = "linux")]
fn unescape(field: &str) -> String {
    field
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\134", "\\")
}

/// A dm device whose uuid says `CRYPT-`, or one built on such a device.
#[cfg(target_os = "linux")]
fn stands_on_crypt(device: &str, depth: u8) -> bool {
    if depth > 8 {
        return false;
    }
    let base = Path::new("/sys/class/block").join(device);
    let uuid = std::fs::read_to_string(base.join("dm/uuid")).unwrap_or_default();
    if uuid.starts_with("CRYPT-") {
        return true;
    }
    std::fs::read_dir(base.join("slaves"))
        .into_iter()
        .flatten()
        .flatten()
        .any(|s| stands_on_crypt(&s.file_name().to_string_lossy(), depth + 1))
}

#[cfg(not(target_os = "linux"))]
fn encrypted(_path: &Path) -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_on_the_caches_own_volume_is_always_kept() {
        let dir = tempfile::tempdir().unwrap();
        let doc = dir.path().join("a.pdf");
        std::fs::write(&doc, b"x").unwrap();
        assert!(may_keep(&doc, dir.path()));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn mountinfo_lines_are_read() {
        let line =
            "36 35 98:0 /mnt1 /mnt/my\\040disk rw,noatime master:1 - ext3 /dev/mapper/luks-1 rw";
        assert_eq!(
            mount_and_source(line),
            Some(("/mnt/my disk".into(), "/dev/mapper/luks-1".into()))
        );
        // No device called this; nothing to stand on.
        assert!(!stands_on_crypt("no-such-device", 0));
    }
}
