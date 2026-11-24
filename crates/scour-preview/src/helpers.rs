//! The programs a preview may hand a file to, found on `PATH`.

/// The desktop's own quick-look command, if one is installed: GNOME's `sushi` or
/// macOS `qlmanage`. Found on `PATH`, not from `XDG_CURRENT_DESKTOP`.
pub fn quicklook(chosen: Option<&str>) -> Option<Vec<String>> {
    if let Some(cmd) = chosen {
        let parts: Vec<String> = cmd.split_whitespace().map(str::to_owned).collect();
        return (!parts.is_empty()).then_some(parts);
    }
    const CANDIDATES: &[&[&str]] = &[
        // `-p` previews; without it qlmanage writes thumbnails into the cwd.
        &["qlmanage", "-p"],
        // GNOME, Cinnamon, and GTK quick-look viewers.
        &["sushi"],
        &["gnome-sushi"],
        &["nemo-preview"],
        &["gloobus-preview"],
        &["QuickLook.exe"],
    ];
    if let Some(found) = CANDIDATES
        .iter()
        .find(|c| on_path(c[0]))
        .map(|c| c.iter().map(|s| (*s).to_owned()).collect())
    {
        return Some(found);
    }
    #[cfg(windows)]
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        let ql = std::path::PathBuf::from(local).join(r"Programs\QuickLook\QuickLook.exe");
        if ql.is_file() {
            return Some(vec![ql.to_string_lossy().into_owned()]);
        }
    }
    None
}

/// Is this program on `PATH`? A lookup, never a shell — a shell would take the
/// name as a script.
fn on_path(name: &str) -> bool {
    find_on_path(name).is_some()
}

pub(crate) fn find_on_path(name: &str) -> Option<std::path::PathBuf> {
    let paths = std::env::var_os("PATH")?;
    std::env::split_paths(&paths)
        .map(|dir| dir.join(name))
        .find(|p| {
            p.is_file() && {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::metadata(p)
                        .map(|m| m.permissions().mode() & 0o111 != 0)
                        .unwrap_or(false)
                }
                #[cfg(not(unix))]
                {
                    true
                }
            }
        })
}
