//! File handles: what the kernel names a directory by, which filesystem it is
//! on, and whether the handle can be opened again.

/// What identifies a directory across a reboot, a remount and a rename.
/// The device number, because that is what `stat` hands the walk; the event
/// carries btrfs's subvolume id, matched through one row a mounted subvolume.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct DirKey {
    pub(super) dev: u64,
    pub(super) ino: u64,
}

/// Which filesystem a directory is on and, on btrfs, which subvolume: what an
/// event says about its parent beside the inode, and what a device is asked
/// once. **An inode number alone names nothing**: here 2.6% of the NTFS
/// volume's directories and 2.7% of `/`'s share one with a directory in the
/// home folder, and every btrfs subvolume's root is 256. An event offered by
/// number alone went to whichever map knew that number first — the wrong
/// path, a `stat` that failed, and the real change missed until the daily check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct FsTag {
    /// `__kernel_fsid_t`, as the event and `statfs` both give it.
    pub(super) fsid: [u32; 2],
    /// The subvolume, from a btrfs handle; `None` on every other filesystem.
    pub(super) root: Option<u64>,
}

impl FsTag {
    /// Whether an event with tag `event` can be about a directory on this one.
    /// btrfs mixes the subvolume into the low half of `statfs`'s fsid, and which
    /// subvolume's fsid an event carries depends on the path the helper marked —
    /// measured: `@tmp` `d30e8e93:6f61cd83`, `@home` `d30e8e93:6f61cd87`. So on
    /// btrfs the high half names the filesystem and the handle the subvolume.
    /// An event with no fsid — a test's — goes anywhere, as every event used to.
    pub(super) fn holds(&self, event: &FsTag) -> bool {
        if event.fsid == [0, 0] {
            return true;
        }
        match (self.root, event.root) {
            (Some(mine), Some(theirs)) => mine == theirs && self.fsid[0] == event.fsid[0],
            (None, None) => self.fsid == event.fsid,
            _ => false,
        }
    }

    /// Ask the directory at `path`: `statfs` for the fsid, its own handle for the
    /// subvolume. `None` when either cannot be had, which matches any event.
    pub(super) fn of(path: &str) -> Option<FsTag> {
        let c = std::ffi::CString::new(path).ok()?;
        // SAFETY: a zeroed `statfs` is valid to write into, and `c` outlives the call.
        let mut st: libc::statfs = unsafe { std::mem::zeroed() };
        if unsafe { libc::statfs(c.as_ptr(), &mut st) } != 0 {
            return None;
        }
        // SAFETY: `fsid_t` is two `c_int`s with a private name; same size and layout.
        let fsid: [i32; 2] = unsafe { std::mem::transmute(st.f_fsid) };
        let (kind, bytes) = handle_of(&c)?;
        Some(FsTag {
            fsid: [fsid[0] as u32, fsid[1] as u32],
            root: btrfs_root(kind, &bytes),
        })
    }
}

/// The file handle the kernel gives `path`: its type and bytes, as an event
/// would carry them for the same directory.
fn handle_of(c: &std::ffi::CStr) -> Option<(i32, Vec<u8>)> {
    // `struct file_handle`: bytes u32, type i32, then the handle. As u32s, so
    // the header is aligned.
    let mut raw = [0u32; 2 + 32];
    raw[0] = 128;
    let mut mount_id: libc::c_int = 0;
    // SAFETY: `raw` is a `file_handle` header with room for the 128 bytes it
    // declares; `c` and `mount_id` outlive the call.
    let rc = unsafe {
        libc::syscall(
            libc::SYS_name_to_handle_at,
            libc::AT_FDCWD,
            c.as_ptr(),
            raw.as_mut_ptr(),
            &mut mount_id as *mut libc::c_int,
            0,
        )
    };
    if rc != 0 {
        return None;
    }
    let kind = raw[1] as i32;
    let len = (raw[0] as usize).min(128);
    let bytes: Vec<u8> = raw[2..]
        .iter()
        .flat_map(|w| w.to_ne_bytes())
        .take(len)
        .collect();
    Some((kind, bytes))
}

/// Whether events under `root` can be turned back into paths. A filesystem
/// whose handles this cannot read (vfat, bcachefs, ZFS…) would take the mark
/// and have every event dropped, while counting as watched — which also stops
/// the pulse walks that would otherwise keep it fresh.
pub(super) fn decodable(root: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(c) = std::ffi::CString::new(root.as_os_str().as_bytes()) else {
        return false;
    };
    handle_of(&c).is_some_and(|(kind, bytes)| handle_ino(kind, &bytes).is_some())
}

/// The subvolume a btrfs handle names: `u64` at 8, after the inode.
pub(super) fn btrfs_root(fh_type: i32, bytes: &[u8]) -> Option<u64> {
    match (fh_type, bytes.len()) {
        (0x4d..=0x4f, 20..) => Some(u64::from_le_bytes(bytes.get(8..16)?.try_into().ok()?)),
        _ => None,
    }
}

/// The inode number a file handle carries, by filesystem — undocumented layouts,
/// read anyway because `open_by_handle_at` needs a capability this process does
/// not hold. `(type, length)` distinguishes them: tmpfs and ntfs3 both report type 1.
pub(super) fn handle_ino(fh_type: i32, bytes: &[u8]) -> Option<u64> {
    let le64 = |at: usize| -> Option<u64> {
        bytes
            .get(at..at + 8)?
            .try_into()
            .ok()
            .map(u64::from_le_bytes)
    };
    let le32 = |at: usize| -> Option<u64> {
        bytes
            .get(at..at + 4)?
            .try_into()
            .ok()
            .map(|b| u32::from_le_bytes(b) as u64)
    };
    match (fh_type, bytes.len()) {
        // btrfs: ino u64 @0, subvolume id u64 @8, generation u32 @16.
        (0x4d..=0x4f, 20..) => le64(0),
        // tmpfs: generation u32 @0, ino u64 @4 — twelve bytes, hence the length match.
        (1, 12) => le64(4),
        // FILEID_INO32_GEN, what ntfs3 and most others use: ino u32 @0, generation @4.
        (1, 8) => le32(0),
        // XFS with 64-bit inodes (its default): ino u64 @0, generation u32 @8.
        (0x81, 12) => le64(0),
        _ => None,
    }
}
