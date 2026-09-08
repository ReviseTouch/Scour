//! The desktop's wastebasket, as the specification describes it.
//!
//! **Why a deletion should be a move.** A search tool that offers to delete
//! offers to delete the wrong file, because the row under the pointer is one
//! of several with the same name and the person is going by what they can
//! see. `unlink` makes that mistake final. Moving to the trash makes it a
//! mistake somebody can undo from their file manager, without this program
//! having to grow an undo of its own — the desktop already has one, and it is
//! the one they already know how to use.
//!
//! That is also what lets the menu keep its rule. The comment in the browser
//! face says a menu should be safe to press; a permanent delete is not, and a
//! reversible one is.
//!
//! ## What the specification actually asks for
//!
//! <https://specifications.freedesktop.org/trash-spec/trashspec-1.0.html>
//!
//! * The trash for a file on the home filesystem is `$XDG_DATA_HOME/Trash`,
//!   holding `files/` and `info/`.
//! * A file on **another** filesystem goes to a trash at that filesystem's
//!   mount point, because the move has to be a rename and a rename cannot
//!   cross a device. Either `$topdir/.Trash/$uid` — only if `.Trash` exists,
//!   is a real directory rather than a symlink, and has the sticky bit — or
//!   `$topdir/.Trash-$uid`, which this creates.
//! * Every trashed file has an `info/NAME.trashinfo` beside it saying where it
//!   came from and when it went.
//! * Names collide, so the name is *claimed* by creating the info file with
//!   `O_EXCL` before anything is moved. Two programs trashing `notes.txt` at
//!   the same moment is the case this is for, and checking whether a name is
//!   free and then using it is exactly the race it is not allowed to be.
//!
//! ## What this deliberately does not do
//!
//! It does not empty the trash, restore from it, or list it. Those are the
//! file manager's, and a search tool that grows them has become a file
//! manager with a search box.

#[cfg(unix)]
use std::io::Write;
use std::path::{Path, PathBuf};

/// Why a file could not be sent to the trash.
#[derive(Debug)]
pub enum Error {
    /// There is no wastebasket to move it to on this platform.
    Unsupported,
    /// There is nowhere to put it: no writable trash for this filesystem.
    NoTrash(PathBuf),
    /// The path does not exist, or cannot be read.
    Missing(PathBuf),
    /// A path with no file name — `/`, or one ending in `..`.
    Unnamed(PathBuf),
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Unsupported => write!(f, "this platform has no wastebasket"),
            Error::NoTrash(p) => write!(f, "no trash directory for {}", p.display()),
            Error::Missing(p) => write!(f, "{} is not there", p.display()),
            Error::Unnamed(p) => write!(f, "{} has no name to trash", p.display()),
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

#[cfg(unix)]
/// Send one path to the trash. Returns where it ended up.
///
/// The path may be a file, a directory or a symlink; a directory goes whole,
/// because a rename moves a tree in one step and this never copies.
pub fn trash(path: &Path) -> Result<PathBuf, Error> {
    into(path, home_trash())
}

#[cfg(unix)]
/// [`trash`], with the home wastebasket named rather than looked up.
///
/// **Exists so the tests never touch the desktop's.** The obvious way to
/// redirect them is `XDG_DATA_HOME`, and that is a process-wide variable in a
/// test runner that runs threads in parallel: three tests set it, whichever
/// set it last wins, and two of them then trash into a third's directory. That
/// is not a hypothetical — it passed alone and failed in the workspace run,
/// which is the worst shape a test failure comes in.
fn into(path: &Path, home: Option<PathBuf>) -> Result<PathBuf, Error> {
    let path = absolute(path);
    let name = path
        .file_name()
        .ok_or_else(|| Error::Unnamed(path.clone()))?
        .to_owned();
    // `symlink_metadata`, so a broken symlink is still trashable — it is a
    // thing on disk with a name, and refusing to remove it because what it
    // points at is gone would be the wrong answer twice.
    let meta = std::fs::symlink_metadata(&path).map_err(|_| Error::Missing(path.clone()))?;
    let _ = meta;

    let dir = match &home {
        Some(h) if same_device(h, &path) => h.clone(),
        _ => volume_trash(&path).ok_or_else(|| Error::NoTrash(path.clone()))?,
    };

    std::fs::create_dir_all(dir.join("files"))?;
    std::fs::create_dir_all(dir.join("info"))?;

    let name = name.to_string_lossy().into_owned();
    // **Relative to the top directory for a volume trash**, absolute for the
    // home one. A volume can be mounted somewhere else tomorrow, and a restore
    // that puts the file back at yesterday's mount point puts it nowhere.
    let recorded = match top_dir_of(&dir) {
        Some(top) => path
            .strip_prefix(&top)
            .map(|r| r.to_path_buf())
            .unwrap_or_else(|_| path.clone()),
        None => path.clone(),
    };

    let (claimed, mut info) = claim(&dir, &name)?;
    let stamp = local_stamp();
    let body = format!(
        "[Trash Info]\nPath={}\nDeletionDate={stamp}\n",
        encode(&recorded.to_string_lossy())
    );
    info.write_all(body.as_bytes())?;
    info.sync_all()?;
    drop(info);

    let landed = dir.join("files").join(&claimed);
    match std::fs::rename(&path, &landed) {
        Ok(()) => Ok(landed),
        Err(e) => {
            // The name was claimed and nothing was moved into it. Leaving the
            // info file behind would make the trash list a file that is not
            // there, which every file manager renders as a phantom row.
            let _ = std::fs::remove_file(dir.join("info").join(format!("{claimed}.trashinfo")));
            Err(Error::Io(e))
        }
    }
}

#[cfg(unix)]
/// Is there somewhere to put this path, without moving it to find out?
///
/// For a menu that would rather grey an item out than fail after the press.
pub fn can_trash(path: &Path) -> bool {
    let path = absolute(path);
    if path.file_name().is_none() {
        return false;
    }
    match home_trash() {
        Some(h) if same_device(&h, &path) => true,
        _ => volume_trash(&path).is_some(),
    }
}

#[cfg(unix)]
/// Claim a name by creating its info file, and return both.
///
/// **`create_new`, in a loop, is the whole point.** Asking whether a name is
/// free and then taking it is two steps with a gap, and the gap is where the
/// other program trashing the same name lands.
fn claim(dir: &Path, name: &str) -> Result<(String, std::fs::File), Error> {
    let (stem, ext) = split_extension(name);
    for n in 0u32..10_000 {
        let candidate = if n == 0 {
            name.to_owned()
        } else if ext.is_empty() {
            format!("{stem}.{n}")
        } else {
            format!("{stem}.{n}.{ext}")
        };
        let at = dir.join("info").join(format!("{candidate}.trashinfo"));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&at)
        {
            Ok(f) => return Ok((candidate, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(Error::Io(e)),
        }
    }
    Err(Error::NoTrash(dir.to_path_buf()))
}

#[cfg(unix)]
/// `notes.tar.gz` splits at the last dot, not the first: `notes.tar` + `gz`.
///
/// The numbered name has to stay recognisable, and `notes.2.tar.gz` reads as
/// the same file where `notes.tar.2.gz` reads as a different kind of one.
fn split_extension(name: &str) -> (String, String) {
    match name.rfind('.') {
        // A leading dot is a hidden file, not an extension.
        Some(i) if i > 0 => (name[..i].to_owned(), name[i + 1..].to_owned()),
        _ => (name.to_owned(), String::new()),
    }
}

#[cfg(unix)]
fn home_trash() -> Option<PathBuf> {
    let base = match std::env::var_os("XDG_DATA_HOME") {
        Some(v) if !v.is_empty() => PathBuf::from(v),
        _ => PathBuf::from(std::env::var_os("HOME")?).join(".local/share"),
    };
    Some(base.join("Trash"))
}

#[cfg(unix)]
/// The trash at the top of the volume a path lives on.
///
/// `.Trash/$uid` only when the administrator made `.Trash` deliberately —
/// a directory, not a link, with the sticky bit set, exactly as the
/// specification says. Otherwise `.Trash-$uid`, which belongs to one user and
/// can be created without asking anybody.
fn volume_trash(path: &Path) -> Option<PathBuf> {
    let top = mount_point_of(path)?;
    let uid = unsafe { libc::getuid() };

    let shared = top.join(".Trash");
    if let Ok(md) = std::fs::symlink_metadata(&shared) {
        use std::os::unix::fs::PermissionsExt;
        let sticky = md.permissions().mode() & 0o1000 != 0;
        if md.is_dir() && !md.file_type().is_symlink() && sticky {
            let mine = shared.join(uid.to_string());
            if std::fs::create_dir_all(&mine).is_ok() {
                return Some(mine);
            }
        }
    }

    let own = top.join(format!(".Trash-{uid}"));
    if own.is_dir() || std::fs::create_dir_all(&own).is_ok() {
        return Some(own);
    }
    None
}

#[cfg(unix)]
/// Walk up until the device number changes: that is the mount point.
fn mount_point_of(path: &Path) -> Option<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let start = std::fs::symlink_metadata(path).ok()?.dev();
    let mut at = path.parent()?;
    loop {
        match at.parent() {
            Some(up) => match std::fs::metadata(up) {
                Ok(md) if md.dev() == start => at = up,
                _ => return Some(at.to_path_buf()),
            },
            None => return Some(at.to_path_buf()),
        }
    }
}

#[cfg(unix)]
/// Which trash directory is this, and what is the volume under it?
fn top_dir_of(trash: &Path) -> Option<PathBuf> {
    let name = trash.file_name()?.to_string_lossy().into_owned();
    if name.starts_with(".Trash-") {
        return trash.parent().map(Path::to_path_buf);
    }
    // `.Trash/$uid` — two levels up.
    if trash.parent()?.file_name()? == ".Trash" {
        return trash.parent()?.parent().map(Path::to_path_buf);
    }
    None
}

#[cfg(unix)]
fn same_device(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    // The trash may not exist yet, so the question is really about the nearest
    // parent that does.
    let dev = |p: &Path| -> Option<u64> {
        let mut at = p;
        loop {
            if let Ok(md) = std::fs::metadata(at) {
                return Some(md.dev());
            }
            at = at.parent()?;
        }
    };
    match (dev(a), dev(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

#[cfg(unix)]
fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(path)
    }
}

#[cfg(unix)]
/// Percent-encoding, as the specification's `Path=` field wants it.
///
/// The unreserved set of RFC 3986 plus `/`, which has to stay readable — a
/// trash info file is a thing people open in an editor when something has gone
/// wrong, and `%2F` between every component helps nobody.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(unix)]
/// `YYYY-MM-DDThh:mm:ss` in local time, which is what the specification says.
///
/// Local rather than UTC because a file manager shows this string to a person
/// and a deletion that says it happened three hours from now is a deletion
/// they will not trust. `localtime_r` is the only thing here the standard
/// library cannot do: it needs the timezone database, and reading that is the
/// operating system's job rather than this crate's.
fn local_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as libc::time_t)
        .unwrap_or(0);
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&secs, &mut tm) };
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A trash of our own, so the test never touches the desktop's.
    fn sandbox() -> PathBuf {
        let at = std::env::temp_dir().join(format!(
            "scour-trash-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        std::fs::create_dir_all(&at).unwrap();
        at
    }

    #[test]
    fn a_file_moves_and_leaves_a_note_saying_where_it_came_from() {
        let box_ = sandbox();
        let data = box_.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let file = box_.join("notes.txt");
        std::fs::write(&file, b"hello").unwrap();

        let landed = into(&file, Some(data.join("Trash"))).expect("trashed");
        assert!(!file.exists(), "the original is gone");
        assert_eq!(std::fs::read(&landed).unwrap(), b"hello", "bytes intact");

        let info = data.join("Trash/info").join(format!(
            "{}.trashinfo",
            landed.file_name().unwrap().to_string_lossy()
        ));
        let note = std::fs::read_to_string(&info).unwrap();
        assert!(note.starts_with("[Trash Info]\n"), "{note}");
        assert!(
            note.contains(&format!("Path={}", encode(&file.to_string_lossy()))),
            "{note}"
        );
        assert!(note.contains("DeletionDate=20"), "{note}");

        std::fs::remove_dir_all(&box_).ok();
    }

    /// The case a check-then-use would get wrong.
    #[test]
    fn a_second_file_of_the_same_name_gets_a_number_rather_than_the_first_ones_place() {
        let box_ = sandbox();
        let data = box_.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let mut landed = Vec::new();
        for (i, sub) in ["a", "b", "c"].iter().enumerate() {
            let dir = box_.join(sub);
            std::fs::create_dir_all(&dir).unwrap();
            let file = dir.join("notes.tar.gz");
            std::fs::write(&file, format!("{i}")).unwrap();
            landed.push(into(&file, Some(data.join("Trash"))).expect("trashed"));
        }

        let names: Vec<String> = landed
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names[0], "notes.tar.gz");
        // The extension survives the number, so the file still opens.
        assert_eq!(names[1], "notes.tar.1.gz");
        assert_eq!(names[2], "notes.tar.2.gz");
        // And all three are still distinguishable by their contents.
        for (i, p) in landed.iter().enumerate() {
            assert_eq!(std::fs::read_to_string(p).unwrap(), i.to_string());
        }

        std::fs::remove_dir_all(&box_).ok();
    }

    #[test]
    fn a_directory_goes_whole() {
        let box_ = sandbox();
        let data = box_.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let tree = box_.join("project");
        std::fs::create_dir_all(tree.join("src")).unwrap();
        std::fs::write(tree.join("src/main.rs"), b"fn main() {}").unwrap();

        let landed = into(&tree, Some(data.join("Trash"))).expect("trashed");
        assert!(!tree.exists());
        assert_eq!(
            std::fs::read_to_string(landed.join("src/main.rs")).unwrap(),
            "fn main() {}"
        );

        std::fs::remove_dir_all(&box_).ok();
    }

    #[test]
    fn what_is_not_there_is_not_trashed_and_says_so() {
        let box_ = sandbox();
        let missing = box_.join("never-existed");
        assert!(matches!(into(&missing, None), Err(Error::Missing(_))));
        assert!(matches!(into(Path::new("/"), None), Err(Error::Unnamed(_))));
        std::fs::remove_dir_all(&box_).ok();
    }

    #[test]
    fn the_path_in_the_note_survives_a_round_trip() {
        // Spaces, an accent and a percent sign — the three things a naive
        // writer gets wrong, and all three appear in real file names.
        assert_eq!(
            encode("/home/a b/çay%1.txt"),
            "/home/a%20b/%C3%A7ay%251.txt"
        );
        assert_eq!(encode("/plain/path.txt"), "/plain/path.txt");
    }

    #[test]
    fn an_extension_is_the_last_dot_and_a_hidden_file_has_none() {
        assert_eq!(
            split_extension("notes.tar.gz"),
            ("notes.tar".into(), "gz".into())
        );
        assert_eq!(split_extension("notes"), ("notes".into(), "".into()));
        assert_eq!(split_extension(".bashrc"), (".bashrc".into(), "".into()));
    }
}

/// **Not implemented off Unix, and saying so is the point.**
///
/// This crate is the freedesktop wastebasket: `$XDG_DATA_HOME/Trash`, a
/// `.trashinfo` beside every file, `$topdir/.Trash-$uid` for another volume.
/// None of that exists on Windows or macOS — they have a Recycle Bin and a
/// `.Trashes`, reached through `SHFileOperation` and `NSFileManager`, which
/// are different enough that pretending otherwise would be a delete that
/// quietly did the wrong thing.
///
/// So the faces ask [`can_trash`] first, get `false`, and leave the item out
/// of the menu rather than offering something that fails after the press —
/// the rule the menu table was built around.
#[cfg(not(unix))]
pub fn trash(_path: &Path) -> Result<PathBuf, Error> {
    Err(Error::Unsupported)
}

/// Always false off Unix. See [`trash`].
#[cfg(not(unix))]
pub fn can_trash(_path: &Path) -> bool {
    false
}
