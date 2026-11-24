//! Sending: a link, a mail, an archive beside the file, a copy on a drive.

use super::*;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// What became of a send.
#[derive(Debug)]
pub enum Sent {
    /// Links made, and where.
    Linked(Vec<PathBuf>),
    /// Handed to another program, which takes it from here.
    Handed,
    /// Being written into this archive.
    Packing(PathBuf),
    /// Being copied into this folder.
    Copying(PathBuf),
}

/// Send these paths to one of [`targets`]. What takes long is started and
/// returns at once; what is refused is refused before anything starts.
pub fn send(id: &str, paths: &[&Path]) -> Result<Sent, Error> {
    let Some(target) = targets().into_iter().find(|t| t.id == id) else {
        return Err(Error::Unknown(id.to_owned()));
    };
    for p in paths {
        let md = std::fs::symlink_metadata(p).map_err(|_| Error::Missing(p.to_path_buf()))?;
        if target.files_only && md.is_dir() {
            return Err(Error::FilesOnly);
        }
    }
    if paths.is_empty() {
        return Ok(Sent::Handed);
    }
    match id {
        "desktop" => link_on_desktop(paths),
        "mail" => {
            let mut c = std::process::Command::new("xdg-email");
            for p in paths {
                c.arg("--attach").arg(p);
            }
            start(c)?;
            Ok(Sent::Handed)
        }
        "bluetooth" => {
            let mut c = std::process::Command::new("bluetooth-sendto");
            c.args(paths);
            start(c)?;
            Ok(Sent::Handed)
        }
        "archive" => pack(paths),
        drive => {
            let at = PathBuf::from(drive.trim_start_matches("drive:"));
            // No overwriting what is already on the drive, and nothing followed.
            let mut c = std::process::Command::new("cp");
            c.args(["-R", "-n", "-P", "--preserve=timestamps", "--"]);
            c.args(paths).arg(&at);
            start(c)?;
            Ok(Sent::Copying(at))
        }
    }
}

/// Start a program and let it be: it owns its own window or its own time.
fn start(mut c: std::process::Command) -> Result<(), Error> {
    c.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    Ok(())
}

#[cfg(unix)]
fn link_on_desktop(paths: &[&Path]) -> Result<Sent, Error> {
    let desk = desktop_dir().ok_or(Error::Unsupported)?;
    let mut made = Vec::new();
    for p in paths {
        let name = p
            .file_name()
            .ok_or_else(|| Error::Missing(p.to_path_buf()))?;
        let at = free_name(&desk, &name.to_string_lossy());
        std::os::unix::fs::symlink(p, &at)?;
        made.push(at);
    }
    Ok(Sent::Linked(made))
}

#[cfg(not(unix))]
fn link_on_desktop(_paths: &[&Path]) -> Result<Sent, Error> {
    Err(Error::Unsupported)
}

/// An archive beside the first path, named after it — or after their folder,
/// for several. Paths from several folders keep their folders inside it.
fn pack(paths: &[&Path]) -> Result<Sent, Error> {
    let first = paths[0];
    let parent = first
        .parent()
        .ok_or_else(|| Error::Missing(first.to_path_buf()))?;
    let together = paths.iter().all(|p| p.parent() == Some(parent));
    let (from, names): (PathBuf, Vec<OsString>) = if together {
        let names = paths
            .iter()
            .map(|p| {
                let mut n = OsString::from("./");
                n.push(p.file_name().unwrap_or_default());
                n
            })
            .collect();
        (parent.to_path_buf(), names)
    } else {
        let names = paths
            .iter()
            .map(|p| p.strip_prefix("/").unwrap_or(p).as_os_str().to_owned())
            .collect();
        (PathBuf::from("/"), names)
    };
    let zip = which("zip").is_some();
    let stem = archive_stem(paths);
    let archive = free_name(
        parent,
        &format!("{stem}.{}", if zip { "zip" } else { "tar.gz" }),
    );
    let mut c = if zip {
        let mut c = std::process::Command::new("zip");
        // Recursive, quiet, and a link stored as a link.
        c.args(["-r", "-q", "-y"]).arg(&archive).args(&names);
        c
    } else {
        let mut c = std::process::Command::new("tar");
        c.arg("-czf")
            .arg(&archive)
            .arg("-C")
            .arg(&from)
            .arg("--")
            .args(&names);
        c
    };
    c.current_dir(&from);
    start(c)?;
    Ok(Sent::Packing(archive))
}

/// What an archive is called: the one file without its extension, the one
/// folder whole, or the folder several came from.
pub(super) fn archive_stem(paths: &[&Path]) -> String {
    let named = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned());
    if let [one] = paths {
        let is_dir = std::fs::symlink_metadata(one).is_ok_and(|m| m.is_dir());
        return if is_dir {
            named(one)
        } else {
            one.file_stem().map(|n| n.to_string_lossy().into_owned())
        }
        .unwrap_or_else(|| "archive".to_owned());
    }
    paths[0]
        .parent()
        .and_then(named)
        .unwrap_or_else(|| "archive".to_owned())
}

/// `name` in `dir`, or `stem (2).ext`, `stem (3).ext`… — the first free one.
/// Asked of the directory entry, not the target: a dangling link is taken.
pub(super) fn free_name(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if std::fs::symlink_metadata(&first).is_err() {
        return first;
    }
    // `.tar.gz` is one extension to a person.
    let (stem, ext) = match name.strip_suffix(".tar.gz") {
        Some(stem) => (stem.to_owned(), ".tar.gz".to_owned()),
        None => match name.rfind('.').filter(|at| *at > 0) {
            Some(at) => (name[..at].to_owned(), name[at..].to_owned()),
            None => (name.to_owned(), String::new()),
        },
    };
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| std::fs::symlink_metadata(p).is_err())
        .expect("an unbounded range finds a free name")
}
