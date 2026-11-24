//! Showing a file in the file manager, selected, and bringing that window
//! forward.

use super::*;
use std::path::Path;

/// Show these paths in the file manager, each selected in its folder — what
/// "open its folder" means to a person. Opening the folder alone did nothing
/// when that folder was already open, and selected nothing when it was not.
/// `org.freedesktop.FileManager1.ShowItems` is answered by Nautilus, Dolphin,
/// Nemo, Caja and Thunar; where nothing answers, each folder is opened instead.
/// Returns at once: the bus can start a file manager, which takes a second.
pub fn reveal(paths: &[&Path]) {
    if paths.is_empty() {
        return;
    }
    #[cfg(windows)]
    windows::reveal(paths);
    #[cfg(not(windows))]
    reveal_elsewhere(paths);
}

#[cfg(not(windows))]
fn reveal_elsewhere(paths: &[&Path]) {
    let quiet = |mut c: std::process::Command| {
        let _ = c
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    };
    // Finder and Explorer each have the one verb for it.
    if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg("-R").args(paths);
        quiet(c);
        return;
    }
    let uris: Vec<String> = paths.iter().map(|p| file_uri(p)).collect();
    let folders: Vec<std::path::PathBuf> = {
        let mut seen = Vec::new();
        for p in paths {
            let dir = p.parent().unwrap_or(p).to_path_buf();
            if !seen.contains(&dir) {
                seen.push(dir);
            }
        }
        seen
    };
    // The window to bring forward is named after the folder it shows.
    let title = paths[0]
        .parent()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    std::thread::spawn(move || {
        if show_items(&uris) {
            raise_file_manager(&title);
            return;
        }
        for dir in folders {
            let _ = open(dir.as_os_str());
        }
    });
}

/// Bring the file manager's window forward, which it cannot do itself on GNOME
/// under Wayland: with no activation token from the window that asked, a folder
/// already open was selected in and left behind. Scour's Shell extension can,
/// given the process and the window's title; without it nothing happens. Twice,
/// because a window the file manager is still opening is not there to raise.
#[cfg_attr(windows, allow(dead_code))]
fn raise_file_manager(title: &str) {
    let Some(pid) = bus_owner_pid("org.freedesktop.FileManager1") else {
        return;
    };
    for wait in [300, 600] {
        std::thread::sleep(std::time::Duration::from_millis(wait));
        let _ = std::process::Command::new("gdbus")
            .args([
                "call",
                "--session",
                "--timeout",
                "2",
                "--dest",
                "org.scour.Shell",
                "--object-path",
                "/org/scour/Shell",
                "--method",
                "org.scour.Shell.Raise",
                &format!("@au [{pid}]"),
                title,
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// The process that owns a name on the session bus.
#[cfg_attr(windows, allow(dead_code))]
fn bus_owner_pid(name: &str) -> Option<u32> {
    let out = std::process::Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--timeout",
            "2",
            "--dest",
            "org.freedesktop.DBus",
            "--object-path",
            "/org/freedesktop/DBus",
            "--method",
            "org.freedesktop.DBus.GetConnectionUnixProcessID",
            name,
        ])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    uint_reply(&String::from_utf8_lossy(&out.stdout))
}

/// The number in a `gdbus` reply such as `(uint32 12345,)`.
#[cfg_attr(windows, allow(dead_code))]
pub(super) fn uint_reply(reply: &str) -> Option<u32> {
    reply
        .split(|c: char| !c.is_ascii_digit())
        .filter(|w| !w.is_empty())
        .nth(1)
        .and_then(|w| w.parse().ok())
}

/// Ask the session's file manager to show these, through whichever bus tool
/// this machine has; false when neither could deliver it.
#[cfg_attr(windows, allow(dead_code))]
fn show_items(uris: &[String]) -> bool {
    let quiet = |mut c: std::process::Command| {
        c.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    let mut gdbus = std::process::Command::new("gdbus");
    gdbus.args([
        "call",
        "--session",
        "--timeout",
        "10",
        "--dest",
        "org.freedesktop.FileManager1",
        "--object-path",
        "/org/freedesktop/FileManager1",
        "--method",
        "org.freedesktop.FileManager1.ShowItems",
        &gvariant_strings(uris),
        "",
    ]);
    if quiet(gdbus) {
        return true;
    }
    let mut send = std::process::Command::new("dbus-send");
    send.args([
        "--session",
        "--print-reply",
        "--dest=org.freedesktop.FileManager1",
        "/org/freedesktop/FileManager1",
        "org.freedesktop.FileManager1.ShowItems",
        &format!("array:string:{}", uris.join(",")),
        "string:",
    ]);
    quiet(send)
}

/// A `file://` URI: every byte outside the unreserved set and `/` escaped, so
/// neither a space nor a quote nor a comma survives to confuse a bus tool.
#[cfg_attr(windows, allow(dead_code))]
pub(super) fn file_uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_encoded_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// An array of strings in GVariant's text form, which is what `gdbus` parses.
/// The URIs are escaped already, so no quote can end one early.
pub(super) fn gvariant_strings(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|u| format!("'{u}'")).collect();
    format!("[{}]", quoted.join(", "))
}
