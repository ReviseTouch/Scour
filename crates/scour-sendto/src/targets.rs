//! The places a file can be sent on this machine: the desktop, the drives
//! plugged in, the programs present.

use std::path::{Path, PathBuf};

/// One place to send to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// What a face sends back. A drive's carries its mount point.
    pub id: String,
    /// What a person reads: English and a catalogue key, unless `named`.
    pub label: String,
    /// The label is a name the machine gave — a drive's — and not translated.
    pub named: bool,
    /// Refuses a directory: a mail or a Bluetooth transfer carries files.
    pub files_only: bool,
}

/// Where a selection can go on this machine now, in the order a menu lists
/// them. Asked each time the menu opens: a drive comes and goes.
pub fn targets() -> Vec<Target> {
    let mut out = Vec::new();
    let fixed = |id: &str, label: &str, files_only: bool| Target {
        id: id.to_owned(),
        label: label.to_owned(),
        named: false,
        files_only,
    };
    if cfg!(unix) && desktop_dir().is_some() {
        out.push(fixed("desktop", "Desktop (as a link)", false));
    }
    if which("xdg-email").is_some() {
        out.push(fixed("mail", "Mail recipient", true));
    }
    if which("bluetooth-sendto").is_some() {
        out.push(fixed("bluetooth", "Bluetooth device", true));
    }
    if which("zip").is_some() || which("tar").is_some() {
        out.push(fixed("archive", "Compressed archive", false));
    }
    for (label, at) in drives() {
        out.push(Target {
            id: format!("drive:{}", at.display()),
            label,
            named: true,
            files_only: false,
        });
    }
    out
}

/// The desktop directory as `user-dirs.dirs` names it, or `~/Desktop`; none
/// where it is missing or is the home directory itself, which the
/// specification says means there is no desktop.
pub(super) fn desktop_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let text = std::fs::read_to_string(config.join("user-dirs.dirs")).unwrap_or_default();
    let dir = desktop_in(&home, &text).unwrap_or_else(|| home.join("Desktop"));
    (dir != home && dir.is_dir()).then_some(dir)
}

/// `XDG_DESKTOP_DIR` out of a `user-dirs.dirs`, `$HOME` expanded.
pub(super) fn desktop_in(home: &Path, user_dirs: &str) -> Option<PathBuf> {
    user_dirs.lines().find_map(|line| {
        let value = line
            .trim()
            .strip_prefix("XDG_DESKTOP_DIR=")?
            .trim_matches('"');
        Some(match value.strip_prefix("$HOME") {
            Some(rest) => home.join(rest.trim_start_matches('/')),
            None => PathBuf::from(value),
        })
    })
}

/// The removable drives this user has mounted, by the name they were given.
fn drives() -> Vec<(String, PathBuf)> {
    let user = std::env::var("USER").ok().or_else(|| {
        std::env::var_os("HOME").and_then(|h| {
            Path::new(&h)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
        })
    });
    let Some(user) = user else {
        return Vec::new();
    };
    std::fs::read_to_string("/proc/self/mountinfo")
        .map(|text| drives_in(&text, &user))
        .unwrap_or_default()
}

/// Mount points under `/run/media/USER/` or `/media/USER/` — where a desktop
/// mounts what is plugged in — from a `mountinfo`, octal escapes undone.
pub(super) fn drives_in(mountinfo: &str, user: &str) -> Vec<(String, PathBuf)> {
    let bases = [format!("/run/media/{user}/"), format!("/media/{user}/")];
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for line in mountinfo.lines() {
        let Some(point) = line.split(' ').nth(4).map(unescape) else {
            continue;
        };
        let Some(label) = bases
            .iter()
            .find_map(|b| point.strip_prefix(b.as_str()))
            .filter(|rest| !rest.is_empty() && !rest.contains('/'))
        else {
            continue;
        };
        let at = PathBuf::from(&point);
        if !out.iter().any(|(_, p)| *p == at) {
            out.push((label.to_owned(), at));
        }
    }
    out
}

/// `\040` and its kind back into the bytes they stand for.
pub(super) fn unescape(field: &str) -> String {
    let bytes = field.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\\'
            && i + 3 < bytes.len()
            && bytes[i + 1..i + 4]
                .iter()
                .all(|b| (b'0'..=b'7').contains(b))
        {
            let n = (bytes[i + 1] - b'0') * 64 + (bytes[i + 2] - b'0') * 8 + (bytes[i + 3] - b'0');
            out.push(n);
            i += 4;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// A program on `PATH`, if there is one by that name.
pub(super) fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|p| p.is_file())
}
