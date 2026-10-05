//! Installing for one account on Windows, which needs no administrator:
//! `install.cmd` in the zip runs `scour-gui.exe --install`. Everything goes
//! where a per-user program goes and nowhere else —
//!
//! - the files, to `%LOCALAPPDATA%\Programs\Scour`;
//! - a Start menu shortcut to the window;
//! - that folder on the account's PATH, so `scour` answers in a terminal.
//!
//! Then the installed window opens, and asks once whether to start with
//! Windows (see `autostart`). `--uninstall` takes the shortcut, the PATH entry
//! and the Run value away again; `uninstall.cmd` then removes the folder.

#[cfg(windows)]
mod imp {
    use std::path::{Path, PathBuf};

    /// Where a per-user program lives.
    fn home() -> Result<PathBuf, String> {
        std::env::var_os("LOCALAPPDATA")
            .map(|d| PathBuf::from(d).join("Programs").join("Scour"))
            .ok_or_else(|| "LOCALAPPDATA is not set".to_owned())
    }

    fn shortcut() -> Result<PathBuf, String> {
        std::env::var_os("APPDATA")
            .map(|d| PathBuf::from(d).join(r"Microsoft\Windows\Start Menu\Programs\Scour.lnk"))
            .ok_or_else(|| "APPDATA is not set".to_owned())
    }

    fn quiet(program: &str, args: &[&str]) -> std::io::Result<std::process::Output> {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        std::process::Command::new(program)
            .args(args)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
    }

    pub fn install() -> Result<PathBuf, String> {
        let from = std::env::current_exe()
            .map_err(|e| e.to_string())?
            .parent()
            .map(Path::to_path_buf)
            .ok_or("no folder for this program")?;
        let to = home()?;
        if !same(&from, &to) {
            // A running copy holds its files, the service included; whatever
            // face opens next starts it again.
            for exe in ["scourd.exe", "scour-web.exe", "scour-tui.exe"] {
                let _ = quiet("taskkill", &["/f", "/im", exe]);
            }
            std::fs::create_dir_all(&to).map_err(|e| format!("{}: {e}", to.display()))?;
            let entries = std::fs::read_dir(&from).map_err(|e| e.to_string())?;
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file() {
                    let dest = to.join(entry.file_name());
                    std::fs::copy(&path, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
                }
            }
        }
        link(&to.join("scour-gui.exe"), &to)?;
        on_path(&to, true)?;
        Ok(to)
    }

    pub fn uninstall() -> Result<(), String> {
        let to = home()?;
        let _ = crate::autostart::set(false);
        if let Ok(lnk) = shortcut() {
            let _ = std::fs::remove_file(lnk);
        }
        on_path(&to, false)
    }

    fn same(a: &Path, b: &Path) -> bool {
        match (a.canonicalize(), b.canonicalize()) {
            (Ok(a), Ok(b)) => a == b,
            _ => false,
        }
    }

    /// The Start menu entry: an `IShellLink` saved through `IPersistFile`.
    fn link(target: &Path, dir: &Path) -> Result<(), String> {
        use windows::Win32::System::Com::{
            CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
            IPersistFile,
        };
        use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
        use windows::core::{HSTRING, Interface};
        let at = shortcut()?;
        if let Some(parent) = at.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        // SAFETY: COM on this thread, one object, released when it drops.
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            let made: windows::core::Result<()> = (|| {
                let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
                link.SetPath(&HSTRING::from(target.as_os_str()))?;
                link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))?;
                link.SetDescription(&HSTRING::from("Scour: find any file"))?;
                let file: IPersistFile = link.cast()?;
                file.Save(&HSTRING::from(at.as_os_str()), true)
            })();
            made.map_err(|e| format!("Start menu shortcut: {e}"))
        }
    }

    /// Add the folder to the account's PATH, or take it off, and tell running
    /// programs — a terminal opened afterwards sees it without a new login.
    fn on_path(dir: &Path, wanted: bool) -> Result<(), String> {
        let out = quiet("reg", &["query", r"HKCU\Environment", "/v", "Path"])
            .map_err(|e| e.to_string())?;
        let said = String::from_utf8_lossy(&out.stdout);
        // `    Path    REG_EXPAND_SZ    C:\a;C:\b`
        let now = said
            .lines()
            .find_map(|l| {
                let l = l.trim();
                l.strip_prefix("Path")
                    .or_else(|| l.strip_prefix("PATH"))
                    .map(str::trim)
                    .and_then(|r| r.split_once(char::is_whitespace))
                    .map(|(_, v)| v.trim().to_owned())
            })
            .unwrap_or_default();
        let me = dir.to_string_lossy().into_owned();
        let parts: Vec<&str> = now.split(';').filter(|p| !p.is_empty()).collect();
        let has = parts.iter().any(|p| p.eq_ignore_ascii_case(&me));
        let next: Vec<&str> = if wanted {
            if has {
                return Ok(());
            }
            parts.into_iter().chain([me.as_str()]).collect()
        } else {
            if !has {
                return Ok(());
            }
            parts
                .into_iter()
                .filter(|p| !p.eq_ignore_ascii_case(&me))
                .collect()
        };
        let joined = next.join(";");
        // Nothing left is no value, not an empty one.
        let out = if joined.is_empty() {
            quiet("reg", &["delete", r"HKCU\Environment", "/v", "Path", "/f"])
        } else {
            quiet(
                "reg",
                &[
                    "add",
                    r"HKCU\Environment",
                    "/v",
                    "Path",
                    "/t",
                    "REG_EXPAND_SZ",
                    "/d",
                    &joined,
                    "/f",
                ],
            )
        }
        .map_err(|e| e.to_string())?;
        if !out.status.success() {
            return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
        }
        announce();
        Ok(())
    }

    fn announce() {
        use windows::Win32::UI::WindowsAndMessaging::{
            HWND_BROADCAST, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_SETTINGCHANGE,
        };
        use windows::core::w;
        // SAFETY: a broadcast with a static string; no reply is read.
        unsafe {
            let _ = SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                windows::Win32::Foundation::WPARAM(0),
                windows::Win32::Foundation::LPARAM(w!("Environment").as_ptr() as isize),
                SMTO_ABORTIFHUNG,
                2000,
                None,
            );
        }
    }
}

/// Install, open the installed window, and say what happened if it failed —
/// to a dialog, since this program has no console.
#[cfg(windows)]
pub fn run(install: bool) {
    let said = if install {
        imp::install().and_then(|to| {
            std::process::Command::new(to.join("scour-gui.exe"))
                .current_dir(&to)
                .spawn()
                .map(drop)
                .map_err(|e| e.to_string())
        })
    } else {
        imp::uninstall()
    };
    if let Err(e) = said {
        let _ = rfd::MessageDialog::new()
            .set_title("Scour")
            .set_description(e)
            .set_level(rfd::MessageLevel::Error)
            .show();
    }
}

#[cfg(not(windows))]
pub fn run(_install: bool) {
    eprintln!("scour-gui: on Linux, install.sh or a package installs Scour");
}
