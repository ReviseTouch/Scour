//! "Send to": the places a file is commonly sent from a menu — a link on the
//! desktop, a mail, a Bluetooth device, an archive beside it, a removable
//! drive — each offered only where this machine can do it. What is slow is
//! started and left running: the file manager and the index see the result.

use std::ffi::OsString;
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

#[derive(Debug)]
pub enum Error {
    /// Not one of the places [`targets`] offers now.
    Unknown(String),
    /// A directory, to a place that only takes files.
    FilesOnly,
    /// A path that is not there, or has no name.
    Missing(PathBuf),
    /// Not on this platform.
    Unsupported,
    Io(std::io::Error),
}

impl Error {
    /// The catalogue key for what went wrong, where there is a fixed one.
    pub fn msgid(&self) -> &'static str {
        match self {
            Error::Unknown(_) => "that place is not there any more",
            Error::FilesOnly => "only files can be sent there",
            Error::Missing(_) => "it is not there",
            Error::Unsupported => "not on this platform",
            Error::Io(_) => "it could not be sent",
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unknown(id) => write!(f, "no place called {id} to send to"),
            Error::FilesOnly => write!(f, "only files can be sent there"),
            Error::Missing(p) => write!(f, "{} is not there", p.display()),
            Error::Unsupported => write!(f, "not on this platform"),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
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
fn archive_stem(paths: &[&Path]) -> String {
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
fn free_name(dir: &Path, name: &str) -> PathBuf {
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

/// The desktop directory as `user-dirs.dirs` names it, or `~/Desktop`; none
/// where it is missing or is the home directory itself, which the
/// specification says means there is no desktop.
fn desktop_dir() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    let text = std::fs::read_to_string(config.join("user-dirs.dirs")).unwrap_or_default();
    let dir = desktop_in(&home, &text).unwrap_or_else(|| home.join("Desktop"));
    (dir != home && dir.is_dir()).then_some(dir)
}

/// `XDG_DESKTOP_DIR` out of a `user-dirs.dirs`, `$HOME` expanded.
fn desktop_in(home: &Path, user_dirs: &str) -> Option<PathBuf> {
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
fn drives_in(mountinfo: &str, user: &str) -> Vec<(String, PathBuf)> {
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
fn unescape(field: &str) -> String {
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
fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("scour-sendto-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("mkdir");
        d
    }

    #[test]
    fn a_taken_name_gets_the_first_free_number_and_keeps_its_extension() {
        let d = scratch("names");
        assert_eq!(free_name(&d, "a.txt"), d.join("a.txt"));
        std::fs::write(d.join("a.txt"), b"").expect("write");
        std::fs::write(d.join("a (2).txt"), b"").expect("write");
        assert_eq!(free_name(&d, "a.txt"), d.join("a (3).txt"));
        std::fs::write(d.join("b.tar.gz"), b"").expect("write");
        assert_eq!(free_name(&d, "b.tar.gz"), d.join("b (2).tar.gz"));
        std::fs::write(d.join(".hidden"), b"").expect("write");
        assert_eq!(free_name(&d, ".hidden"), d.join(".hidden (2)"));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn the_desktop_is_where_user_dirs_says() {
        let home = Path::new("/home/u");
        let text = "# comment\nXDG_DOWNLOAD_DIR=\"$HOME/İndirilenler\"\nXDG_DESKTOP_DIR=\"$HOME/Masaüstü\"\n";
        assert_eq!(
            desktop_in(home, text),
            Some(PathBuf::from("/home/u/Masaüstü"))
        );
        assert_eq!(
            desktop_in(home, "XDG_DESKTOP_DIR=\"/srv/desk\"\n"),
            Some(PathBuf::from("/srv/desk"))
        );
        assert_eq!(desktop_in(home, ""), None);
    }

    #[test]
    fn a_drive_is_a_mount_under_the_users_media_and_nothing_else() {
        let info = "\
36 1 0:30 / / rw - btrfs /dev/nvme0n1p5 rw
90 36 8:17 / /run/media/hasan/USB\\040BELLEK rw - vfat /dev/sdb1 rw
91 36 8:33 / /media/hasan/Yedek rw - ext4 /dev/sdc1 rw
92 36 8:49 / /run/media/other/Theirs rw - vfat /dev/sdd1 rw
93 90 8:50 / /run/media/hasan/USB\\040BELLEK/inner rw - vfat /dev/sdd2 rw
94 36 0:60 / /mnt/depo rw - ntfs3 /dev/nvme1n1p2 rw";
        assert_eq!(
            drives_in(info, "hasan"),
            vec![
                (
                    "USB BELLEK".to_owned(),
                    PathBuf::from("/run/media/hasan/USB BELLEK")
                ),
                ("Yedek".to_owned(), PathBuf::from("/media/hasan/Yedek")),
            ]
        );
    }

    #[test]
    fn an_archive_is_named_after_what_it_holds() {
        let d = scratch("stems");
        std::fs::write(d.join("report.pdf"), b"").expect("write");
        std::fs::create_dir_all(d.join("photos.2024")).expect("mkdir");
        std::fs::write(d.join("b.txt"), b"").expect("write");
        assert_eq!(archive_stem(&[&d.join("report.pdf")]), "report");
        assert_eq!(archive_stem(&[&d.join("photos.2024")]), "photos.2024");
        let folder = d.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(
            archive_stem(&[&d.join("report.pdf"), &d.join("b.txt")]),
            folder
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_place_not_offered_is_refused_before_anything_starts() {
        let d = scratch("refuse");
        std::fs::write(d.join("f.txt"), b"").expect("write");
        let f = d.join("f.txt");
        assert!(matches!(
            send("drive:/etc", &[f.as_path()]),
            Err(Error::Unknown(_))
        ));
        assert!(matches!(
            send("nowhere", &[f.as_path()]),
            Err(Error::Unknown(_))
        ));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn octal_escapes_become_the_bytes_they_name() {
        assert_eq!(unescape("/a\\040b\\011c"), "/a b\tc");
        assert_eq!(unescape("/plain"), "/plain");
        assert_eq!(unescape("/end\\04"), "/end\\04");
    }
}
