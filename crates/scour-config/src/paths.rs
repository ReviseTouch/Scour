//! Where things live, per platform.
//!
//! Through `directories`, rather than by hand. The hand-rolled version this
//! replaces put macOS configuration in `~/.config`, which is a Linux
//! convention that macOS does not share — the kind of mistake that is invisible
//! until someone on the other platform cannot find their settings.

use std::path::PathBuf;

use directories::ProjectDirs;

fn dirs() -> Option<ProjectDirs> {
    ProjectDirs::from("", "", "scour")
}

/// `~/.config/scour/config.toml`, or the platform's equivalent.
pub fn config_path() -> PathBuf {
    dirs()
        .map(|d| d.config_dir().join("config.toml"))
        .unwrap_or_else(|| PathBuf::from("scour.toml"))
}

/// Where the index and other generated state live.
pub fn data_dir() -> PathBuf {
    dirs()
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn default_index_dir() -> PathBuf {
    data_dir().join("index")
}

/// The address the service listens on.
///
/// A local socket rather than a TCP port. Binding a fixed loopback port is
/// simpler, but it raises a firewall prompt on Windows and macOS the first
/// time, offers no access control at all, and is reachable by anything else on
/// the machine. A socket in the user's own runtime directory is protected by
/// the filesystem's own permissions, which is exactly the boundary wanted here.
pub fn socket_path() -> String {
    #[cfg(windows)]
    {
        // Named pipes are not filesystem paths, and are per-session by
        // convention rather than by permission.
        r"\\.\pipe\scour".to_owned()
    }
    #[cfg(not(windows))]
    {
        let base = dirs()
            .and_then(|d| d.runtime_dir().map(PathBuf::from))
            .unwrap_or_else(data_dir);
        base.join("scour.sock").to_string_lossy().into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_location_is_absolute_and_distinct() {
        let (c, d, i) = (config_path(), data_dir(), default_index_dir());
        assert!(c.is_absolute(), "{c:?}");
        assert!(d.is_absolute(), "{d:?}");
        assert!(i.starts_with(&d));
        assert_ne!(c.parent(), Some(i.as_path()));
        assert!(!socket_path().is_empty());
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn linux_follows_the_xdg_layout() {
        let c = config_path();
        assert!(c.to_string_lossy().contains("/scour/"), "{c:?}");
        assert!(c.ends_with("config.toml"));
    }
}
