//! Asking a Linux machine: the mount table, `/dev/disk` and `/sys`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::classify::{self, Probe};
use crate::volume::Volume;

/// Every volume mounted now.
pub fn look() -> Vec<Volume> {
    let text = std::fs::read_to_string("/proc/self/mountinfo").unwrap_or_default();
    let mounts = crate::mountinfo::parse(&text);
    classify::volumes(&mounts, &Machine::new())
}

/// `/dev/disk/by-uuid` and `by-label`, read once per look.
struct Machine {
    uuids: HashMap<PathBuf, String>,
    labels: HashMap<PathBuf, String>,
}

impl Machine {
    fn new() -> Machine {
        Machine {
            uuids: links("/dev/disk/by-uuid"),
            labels: links("/dev/disk/by-label"),
        }
    }
}

/// Each link's name by the device it points at.
fn links(dir: &str) -> HashMap<PathBuf, String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return HashMap::new();
    };
    entries
        .flatten()
        .filter_map(|e| {
            let device = std::fs::canonicalize(e.path()).ok()?;
            Some((device, udev_unescape(&e.file_name().to_string_lossy())))
        })
        .collect()
}

/// udev writes a label's space and slash as `\x20` and `\x2f`.
fn udev_unescape(name: &str) -> String {
    let bytes = name.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 2..i + 4)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], bytes.get(i + 1), hex) {
            (b'\\', Some(b'x'), Some(b)) => {
                out.push(b);
                i += 4;
            }
            (b, _, _) => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

impl Probe for Machine {
    fn names(&self, source: &str) -> (Option<String>, Option<String>) {
        let Ok(device) = std::fs::canonicalize(source) else {
            return (None, None);
        };
        (
            self.uuids.get(&device).cloned(),
            self.labels.get(&device).cloned(),
        )
    }

    fn removable(&self, source: &str) -> bool {
        std::fs::canonicalize(source)
            .ok()
            .and_then(|d| d.file_name().map(|n| n.to_string_lossy().into_owned()))
            .is_some_and(|name| plugged_in(&name, 0))
    }

    fn size(&self, at: &str) -> u64 {
        let Ok(path) = std::ffi::CString::new(at) else {
            return 0;
        };
        // SAFETY: a valid C string and a zeroed struct the call fills.
        unsafe {
            let mut s: libc::statvfs = std::mem::zeroed();
            if libc::statvfs(path.as_ptr(), &mut s) != 0 {
                return 0;
            }
            // `fsblkcnt_t` and the block size are narrower on 32-bit systems.
            #[allow(clippy::useless_conversion)]
            u64::from(s.f_blocks).saturating_mul(u64::from(s.f_frsize))
        }
    }
}

/// A block device behind a USB or card-reader bus, or one the kernel marks
/// removable — by its partition's disk too. An encrypted or LVM volume is
/// asked through what it is made of, `depth` levels down at most.
fn plugged_in(name: &str, depth: u8) -> bool {
    let class = Path::new("/sys/class/block").join(name);
    let Ok(real) = std::fs::canonicalize(&class) else {
        return false;
    };
    let path = real.to_string_lossy();
    if path.contains("/usb") || path.contains("/mmc") {
        return true;
    }
    let flag =
        |dir: &Path| std::fs::read_to_string(dir.join("removable")).is_ok_and(|v| v.trim() == "1");
    if flag(&real) || real.parent().is_some_and(flag) {
        return true;
    }
    depth < 3
        && std::fs::read_dir(class.join("slaves")).is_ok_and(|parts| {
            parts
                .flatten()
                .any(|p| plugged_in(&p.file_name().to_string_lossy(), depth + 1))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_udev_label_comes_back_with_its_spaces() {
        assert_eq!(udev_unescape(r"My\x20Disk"), "My Disk");
        assert_eq!(udev_unescape(r"a\x2fb"), "a/b");
        assert_eq!(udev_unescape(r"plain"), "plain");
        assert_eq!(udev_unescape(r"odd\x2"), r"odd\x2");
    }
}
