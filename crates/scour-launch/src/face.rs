//! Starting another face: the window, the browser, the terminal.
//!
//! On Unix through `scour-open`, the one script that knows nine terminals and
//! the browser's app window. On Windows that script does not exist, and there
//! was nothing in its place: the faces would not open from each other (PC2,
//! 17 November). There each face's own program is started — the terminal one
//! in a console of its own, since it needs one.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A program of Scour's: beside the running face, then, on Unix, two levels up
/// in `scripts` for a build run from `target/`, then on `PATH`. The name
/// without `.exe`; it is added where Windows wants it.
pub fn program(name: &str) -> Option<PathBuf> {
    let file = if cfg!(windows) && Path::new(name).extension().is_none() {
        format!("{name}.exe")
    } else {
        name.to_owned()
    };
    if let Ok(here) = std::env::current_exe()
        && let Some(dir) = here.parent()
    {
        let beside = dir.join(&file);
        if beside.is_file() {
            return Some(beside);
        }
        let script = dir.join("../../scripts").join(&file);
        if script.is_file() {
            return Some(script);
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(&file))
        .find(|p| p.is_file())
}

/// The command that opens `face` — `window`, `browser` or `tui` — detached
/// from the one asking, or None when it is not installed. Not spawned: a
/// caller may add to its environment first.
pub fn face(face: &str) -> Option<Command> {
    if cfg!(windows) {
        let name = match face {
            "window" => "scour-gui",
            "browser" | "web" => "scour-web",
            "tui" => "scour-tui",
            _ => return None,
        };
        let mut command = Command::new(program(name)?);
        detach(&mut command, face == "tui");
        return Some(command);
    }
    let mut command = match face {
        "window" => Command::new(program("scour-gui")?),
        "browser" | "web" | "tui" => {
            let mut c = Command::new(program("scour-open")?);
            c.arg(if face == "tui" { "tui" } else { "browser" });
            c
        }
        _ => return None,
    };
    detach(&mut command, false);
    Some(command)
}

/// Its own process group, no streams of ours, and on Windows no console —
/// unless it is the terminal face, which is given a new one.
fn detach(command: &mut Command, console: bool) {
    if !console {
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(
            CREATE_NEW_PROCESS_GROUP
                | if console {
                    CREATE_NEW_CONSOLE
                } else {
                    CREATE_NO_WINDOW
                },
        );
    }
    let _ = console;
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_unknown_face_is_nothing() {
        assert!(super::face("doom").is_none());
    }
}
