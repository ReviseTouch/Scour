//! Which trash a file goes to: the home one, or its own volume's.

#[cfg(unix)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
pub(super) fn home_trash() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_DATA_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".local/share"),
    };
    Some(base.join("Trash"))
}

#[cfg(unix)]
/// The trash at the top of the volume a path lives on. `.Trash/$uid` only when
/// `.Trash` is a real directory with the sticky bit, else `.Trash-$uid`.
pub(super) fn volume_trash(path: &Path) -> Option<PathBuf> {
    let top = mount_point_of(path)?;
    let uid = unsafe { libc::getuid() };

    let shared = top.join(".Trash");
    if let Ok(md) = std::fs::symlink_metadata(&shared) {
        use std::os::unix::fs::PermissionsExt;
        let sticky = md.permissions().mode() & 0o1000 != 0;
        if md.is_dir() && !md.file_type().is_symlink() && sticky {
            let mine = shared.join(uid.to_string());
            if own_dir(&mine, uid) {
                return Some(mine);
            }
        }
    }

    let own = top.join(format!(".Trash-{uid}"));
    own_dir(&own, uid).then_some(own)
}

#[cfg(unix)]
/// Make the directory if it is not there, and say whether it is this account's
/// own: a real directory, not a link, owned by `uid`. `.Trash` is writable by
/// everyone and the top of a shared disk often is, so anyone can make the
/// directory first — and a file trashed into theirs is a file handed to them.
pub(super) fn own_dir(dir: &Path, uid: u32) -> bool {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    if std::fs::symlink_metadata(dir).is_err() {
        let _ = std::fs::DirBuilder::new().mode(0o700).create(dir);
    }
    std::fs::symlink_metadata(dir)
        .is_ok_and(|md| md.is_dir() && !md.file_type().is_symlink() && md.uid() == uid)
}

#[cfg(unix)]
/// Walk up until the device number changes: that is the mount point.
fn mount_point_of(path: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let start = std::fs::symlink_metadata(path).ok()?.dev();
    let mut at = path.parent()?;
    loop {
        match at.parent() {
            Some(up) => match std::fs::metadata(up) {
                Ok(md) if md.dev() == start => at = up,
                _ => return Some(at.to_path_buf()),
            },
            None => return Some(at.to_path_buf()),
        }
    }
}

#[cfg(unix)]
/// Which trash directory is this, and what is the volume under it?
pub(super) fn top_dir_of(trash: &Path) -> Option<PathBuf> {
    let name = trash.file_name()?.to_string_lossy().into_owned();
    if name.starts_with(".Trash-") {
        return trash.parent().map(Path::to_path_buf);
    }
    // `.Trash/$uid` — two levels up.
    if trash.parent()?.file_name()? == ".Trash" {
        return trash.parent()?.parent().map(Path::to_path_buf);
    }
    None
}

#[cfg(unix)]
pub(super) fn same_device(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    // The trash may not exist yet: ask the nearest parent that does.
    let dev = |p: &Path| -> Option<u64> {
        let mut at = p;
        loop {
            if let Ok(md) = std::fs::metadata(at) {
                return Some(md.dev());
            }
            at = at.parent()?;
        }
    };
    match (dev(a), dev(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

#[cfg(unix)]
pub(super) fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    }
}
