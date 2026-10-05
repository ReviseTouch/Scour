//! Starting with Windows: a value under the account's own Run key, which needs
//! no administrator. It runs this window with `--start-service`, which starts
//! the service out of sight and leaves — no window at login, only the index
//! kept current.
//!
//! Elsewhere there is nothing to do here: on Linux `install.sh` and the
//! packages set up a systemd unit.

#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const VALUE: &str = "Scour";

/// Whether this platform starts things this way at all.
pub const OFFERED: bool = cfg!(windows);

/// `reg.exe`, without the console window a GUI program's child would open.
#[cfg(windows)]
fn reg(args: &[&str]) -> std::io::Result<std::process::Output> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new("reg")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
}

/// Start the service with Windows, or stop doing so.
#[cfg(windows)]
pub fn set(on: bool) -> Result<(), String> {
    let out = if on {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let command = format!("\"{}\" --start-service", exe.display());
        reg(&[
            "add", RUN_KEY, "/v", VALUE, "/t", "REG_SZ", "/d", &command, "/f",
        ])
    } else {
        reg(&["delete", RUN_KEY, "/v", VALUE, "/f"])
    }
    .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

#[cfg(not(windows))]
pub fn set(_on: bool) -> Result<(), String> {
    Err("starting with the system is set up by the installer here".into())
}
