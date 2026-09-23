//! Which desktop this is, which programs speak to it, and what the key runs.

use std::path::{Path, PathBuf};
use std::process::Command;

/// The Flatpak application id, which is what a key runs inside the sandbox.
pub const FLATPAK_APP: &str = "com.revisetouch.Scour";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Desktop {
    Gnome,
    Kde,
    /// A sandbox: its `gsettings` reach nobody's desktop.
    Flatpak,
    Other,
}

/// What was found on this machine. Built by [`Tools::detect`], or by hand in a
/// test that points the fields at stubs.
#[derive(Debug, Clone, Default)]
pub struct Tools {
    pub gsettings: Option<PathBuf>,
    /// Does `gsettings` answer for GNOME's media-keys schema? Asked once here,
    /// so nothing else has to start a process to find out.
    pub gnome_keys: bool,
    pub kwriteconfig: Option<PathBuf>,
    pub kreadconfig: Option<PathBuf>,
    /// `XDG_CURRENT_DESKTOP`, as given.
    pub desktop_var: String,
    /// `/.flatpak-info` exists.
    pub sandboxed: bool,
    /// The face's own executable; `scour-gui` beside it is what the key runs.
    pub exe: Option<PathBuf>,
    /// Scour's GNOME Shell extension is where the shell looks for one: what
    /// lets the key bring an open window forward.
    pub shell_extension: bool,
}

impl Tools {
    pub fn detect() -> Tools {
        let sandboxed = Path::new("/.flatpak-info").exists();
        let gsettings = which("gsettings");
        let gnome_keys = !sandboxed
            && gsettings.as_deref().is_some_and(|g| {
                Command::new(g)
                    .args(["get", crate::gnome::SCHEMA, "custom-keybindings"])
                    .output()
                    .is_ok_and(|o| o.status.success())
            });
        Tools {
            gsettings,
            gnome_keys,
            kwriteconfig: which("kwriteconfig6").or_else(|| which("kwriteconfig5")),
            kreadconfig: which("kreadconfig6").or_else(|| which("kreadconfig5")),
            desktop_var: std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default(),
            sandboxed,
            exe: std::env::current_exe().ok(),
            shell_extension: !sandboxed && shell_extension_installed(),
        }
    }

    /// KDE when the session says so and its tools are there; GNOME when its
    /// schema answers; the sandbox before either.
    pub fn desktop(&self) -> Desktop {
        if self.sandboxed {
            return Desktop::Flatpak;
        }
        let says_kde = self.desktop_var.to_ascii_uppercase().contains("KDE");
        if says_kde && self.kwriteconfig.is_some() && self.kreadconfig.is_some() {
            return Desktop::Kde;
        }
        if self.gnome_keys && self.gsettings.is_some() {
            return Desktop::Gnome;
        }
        Desktop::Other
    }

    /// What the key runs: the launcher beside this executable, which opens the
    /// face somebody last switched to — so a tarball unpacked into a home
    /// directory binds itself and not an older install; the bare name when
    /// nothing is beside it; the Flatpak command in a sandbox. On Windows the
    /// launcher is a shell script and the window is what there is.
    pub fn command(&self) -> String {
        if self.sandboxed {
            return format!("flatpak run {FLATPAK_APP}");
        }
        let name = if cfg!(windows) {
            "scour-gui.exe"
        } else {
            "scour-open"
        };
        self.exe
            .as_deref()
            .and_then(Path::parent)
            .map(|dir| dir.join(name))
            .filter(|p| p.is_file())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| name.to_owned())
    }
}

/// The user's data directory, then the system's, as the shell searches them.
fn shell_extension_installed() -> bool {
    let home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/share")));
    let system = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|d| !d.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    home.into_iter()
        .chain(std::env::split_paths(&system))
        .any(|dir| {
            dir.join("gnome-shell/extensions")
                .join(crate::gnome::EXTENSION)
                .join("metadata.json")
                .is_file()
        })
}

/// The first executable of that name on `PATH`.
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|p| p.is_file())
}
