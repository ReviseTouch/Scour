//! Starting a program on files, and opening a file with whatever the desktop
//! chose.

use super::*;
use std::path::Path;

/// Start one of them on a file.
pub fn launch(opener: &Opener, path: &Path) -> std::io::Result<()> {
    let words = command(&opener.exec, path);
    let Some((program, args)) = words.split_first() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the desktop entry has nothing to run",
        ));
    };
    std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Hand a file, a folder or an address to the desktop's own opener, as a
/// double-click in the file manager would: `xdg-open`, `open`, `explorer`.
/// Spawned and forgotten, every standard stream closed. Whether it would *run*
/// something is the caller's question — see `scour_core::runs_when_opened`.
pub fn open(target: &std::ffi::OsStr) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        windows::open(target)
    }
    #[cfg(not(windows))]
    {
        open_elsewhere(target)
    }
}

#[cfg(not(windows))]
fn open_elsewhere(target: &std::ffi::OsStr) -> std::io::Result<()> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(target)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

/// An `Exec=` line with its field codes resolved, split into words. `%f` is where
/// the file goes; `%i %c %k` are dropped, since left in place they become literal
/// arguments. An entry with no code at all still gets the path, appended.
pub(super) fn command(exec: &str, path: &Path) -> Vec<String> {
    let file = path.to_string_lossy().into_owned();
    let mut out: Vec<String> = Vec::new();
    let mut took_file = false;
    for word in split(exec) {
        match word.as_str() {
            "%f" | "%F" | "%u" | "%U" => {
                out.push(file.clone());
                took_file = true;
            }
            // Deprecated: the specification says to drop rather than pass on.
            "%d" | "%D" | "%n" | "%N" | "%v" | "%m" | "%i" | "%c" | "%k" => {}
            _ => out.push(word),
        }
    }
    if !took_file {
        out.push(file);
    }
    out
}

/// Split an `Exec=` line the way the specification asks: quotes group, and a
/// backslash inside quotes escapes.
pub(super) fn split(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut any = false;
    for c in line.chars() {
        if escaped {
            word.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' if quoted => escaped = true,
            '"' => {
                quoted = !quoted;
                any = true;
            }
            ' ' | '\t' if !quoted => {
                if any || !word.is_empty() {
                    out.push(std::mem::take(&mut word));
                    any = false;
                }
            }
            _ => word.push(c),
        }
    }
    if any || !word.is_empty() {
        out.push(word);
    }
    out
}
