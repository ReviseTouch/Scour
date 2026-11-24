//! Ask the desktop's own folder picker for one directory, without linking a
//! graphical toolkit into every face.

use std::path::Path;
use std::process::{Command, Stdio};

/// Open a native folder dialog starting near `start`, returning the chosen path.
pub fn pick_folder(start: Option<&Path>) -> Option<String> {
    let dir = start.filter(|p| p.is_dir());
    #[cfg(target_os = "macos")]
    {
        return macos(dir);
    }
    #[cfg(windows)]
    {
        return windows(dir);
    }
    #[cfg(all(not(target_os = "macos"), not(windows)))]
    {
        linux(dir)
    }
}

#[cfg(all(not(target_os = "macos"), not(windows)))]
fn linux(start: Option<&Path>) -> Option<String> {
    let kde = std::env::var("XDG_CURRENT_DESKTOP")
        .unwrap_or_default()
        .to_ascii_lowercase()
        .contains("kde");
    let start_arg = start.and_then(|p| p.to_str()).unwrap_or(".");
    let zenity_dir = start.map(|p| format!("{}/", p.display()));

    let mut tries: Vec<(&str, Vec<String>)> = Vec::new();
    let kd = (
        "kdialog",
        vec!["--getexistingdirectory".into(), start_arg.into()],
    );
    let mut zn_args = vec!["--file-selection".into(), "--directory".into()];
    if let Some(d) = &zenity_dir {
        zn_args.push(format!("--filename={d}"));
    }
    let zn = ("zenity", zn_args.clone());
    let qa = ("qarma", zn_args);
    let yd = (
        "yad",
        vec![
            "--file".into(),
            "--directory".into(),
            format!("--filename={start_arg}/"),
        ],
    );

    if kde {
        tries.extend([kd, zn, qa, yd]);
    } else {
        tries.extend([zn, kd, qa, yd]);
    }

    for (prog, args) in tries {
        let Ok(out) = Command::new(prog)
            .args(&args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
        else {
            continue;
        };
        if !out.status.success() {
            return None;
        }
        let chosen = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        if !chosen.is_empty() {
            return Some(chosen);
        }
        return None;
    }
    None
}

#[cfg(target_os = "macos")]
fn macos(start: Option<&Path>) -> Option<String> {
    let script = match start.and_then(|p| p.to_str()) {
        Some(p) if !p.contains('"') => {
            format!("POSIX path of (choose folder default location POSIX file \"{p}\")")
        }
        _ => "POSIX path of (choose folder)".to_owned(),
    };
    let out = Command::new("osascript")
        .args(["-e", &script])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| {
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .trim_end_matches('/')
                .to_owned()
        })
        .filter(|s| !s.is_empty())
}

#[cfg(windows)]
fn windows(start: Option<&Path>) -> Option<String> {
    let init = start
        .and_then(|p| p.to_str())
        .filter(|s| !s.contains('\''))
        .map(|s| format!("$d.SelectedPath = '{s}'; "))
        .unwrap_or_default();
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; \
         $d = New-Object System.Windows.Forms.FolderBrowserDialog; \
         {init}if ($d.ShowDialog() -eq 'OK') {{ [Console]::Out.Write($d.SelectedPath) }}"
    );
    let out = Command::new("powershell")
        .args(["-NoProfile", "-STA", "-Command", &script])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
}
