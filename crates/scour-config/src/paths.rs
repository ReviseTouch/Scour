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
        socket_at(
            std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from),
            run_user_dir(),
        )
        .to_string_lossy()
        .into_owned()
    }
}

/// The socket, given the runtime directory the environment names and the one
/// the system keeps for this user.
///
/// **The same answer with or without the environment.** A client started by
/// something that strips `XDG_RUNTIME_DIR` — an MCP host, a cron job — used to
/// fall back to the data directory and look for a socket the service never
/// made. `/run/user/<uid>` is where logind puts the runtime directory, so it
/// is tried before giving up on it.
#[cfg(not(windows))]
pub fn socket_at(runtime: Option<PathBuf>, run_user: Option<PathBuf>) -> PathBuf {
    runtime
        .filter(|p| p.is_absolute())
        .or(run_user)
        .map(|p| p.join("scour"))
        .unwrap_or_else(data_dir)
        .join("scour.sock")
}

/// `/run/user/<uid>` if it exists. The uid is read off the process itself, or
/// off the home directory where there is no `/proc`.
#[cfg(not(windows))]
fn run_user_dir() -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let uid = std::fs::metadata("/proc/self")
        .ok()
        .or_else(|| std::env::var_os("HOME").and_then(|h| std::fs::metadata(h).ok()))?
        .uid();
    let dir = PathBuf::from(format!("/run/user/{uid}"));
    dir.is_dir().then_some(dir)
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

    /// The service and every client must name one socket, whatever the
    /// environment of the process asking.
    #[test]
    #[cfg(not(windows))]
    fn the_socket_is_the_same_with_and_without_the_environment() {
        let run = PathBuf::from("/run/user/1000");
        let with_env = socket_at(Some(run.clone()), Some(run.clone()));
        let without = socket_at(None, Some(run.clone()));
        assert_eq!(with_env, without);
        assert_eq!(with_env, PathBuf::from("/run/user/1000/scour/scour.sock"));
        // A relative or empty XDG_RUNTIME_DIR is no runtime directory.
        assert_eq!(
            socket_at(Some(PathBuf::from("")), Some(run.clone())),
            without
        );
        // Only with neither does it fall back to the data directory.
        assert!(socket_at(None, None).starts_with(data_dir()));
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn linux_follows_the_xdg_layout() {
        let c = config_path();
        assert!(c.to_string_lossy().contains("/scour/"), "{c:?}");
        assert!(c.ends_with("config.toml"));
    }
}
