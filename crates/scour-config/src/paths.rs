//! Where things live, per platform. Through `directories` rather than by hand:
//! macOS does not put configuration in `~/.config`.

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

/// The address the service listens on: a local socket, not a TCP port. A loopback
/// port is reachable by anything on the machine and prompts the firewall; a socket
/// in the user's runtime directory is bounded by filesystem permissions.
pub fn socket_path() -> String {
    #[cfg(windows)]
    {
        // Named pipes share one machine-wide namespace, so a fixed name would
        // let the second user to log in read the first user's file names. Not
        // access control — an ACL on the pipe would be — just a separator.
        let who = std::env::var("USERNAME").unwrap_or_else(|_| "user".into());
        let who: String = who
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect();
        format!(
            r"\\.\pipe\scour-{}",
            if who.is_empty() { "user".into() } else { who }
        )
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
