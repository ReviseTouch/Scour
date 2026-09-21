//! Where `scourd` is.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// The service's file name on this platform.
pub const NAME: &str = if cfg!(windows) {
    "scourd.exe"
} else {
    "scourd"
};

/// `scourd` for the face that is running: beside it first, then on `PATH`.
pub fn scourd() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok();
    locate(exe.as_deref(), std::env::var_os("PATH").as_deref())
}

/// The same search, spelled out, so it can be asked without a real `PATH`.
///
/// **Beside the face wins.** A tarball unpacked into a home directory has both
/// binaries in one folder and nothing on `PATH`; a person with an older
/// `scourd` installed system-wide must still get the one they just unpacked.
pub fn locate(exe: Option<&Path>, path: Option<&OsStr>) -> Option<PathBuf> {
    let beside = exe
        .and_then(Path::parent)
        .map(|dir| dir.join(NAME))
        .filter(|p| runnable(p));
    beside.or_else(|| {
        std::env::split_paths(path?)
            .map(|dir| dir.join(NAME))
            .find(|p| runnable(p))
    })
}

/// A file this process could actually exec.
fn runnable(path: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(path) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}
