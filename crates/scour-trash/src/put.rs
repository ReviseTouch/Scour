//! Moving a file into a trash directory, with the record that lets the file
//! manager put it back.

use super::*;
#[cfg(unix)]
use std::io::Write;
use std::path::{Path, PathBuf};

/// Remove a path for good: a file or a link as itself, a directory with all it
/// holds. Nothing is followed — a link to a directory goes, and what it pointed
/// at stays. What the menu offers beside the trash, and asks about first.
pub fn erase(path: &Path) -> Result<(), Error> {
    if path.file_name().is_none() {
        return Err(Error::Unnamed(path.to_owned()));
    }
    let md = std::fs::symlink_metadata(path).map_err(|_| Error::Missing(path.to_owned()))?;
    if md.is_dir() {
        std::fs::remove_dir_all(path)?;
    } else {
        std::fs::remove_file(path)?;
    }
    Ok(())
}

#[cfg(unix)]
/// Send one path to the trash and say where it ended up. A file, a directory or a
/// symlink; a directory goes whole, since a rename moves a tree and never copies.
pub fn trash(path: &Path) -> Result<PathBuf, Error> {
    into(path, home_trash())
}

#[cfg(unix)]
/// [`trash`], with the home trash named rather than looked up — so a test
/// need not set the process-wide `XDG_DATA_HOME` its neighbours also read.
pub(super) fn into(path: &Path, home: Option<PathBuf>) -> Result<PathBuf, Error> {
    let path = absolute(path);
    let name = path
        .file_name()
        .ok_or_else(|| Error::Unnamed(path.clone()))?
        .to_owned();
    // `symlink_metadata`, so a broken symlink is still trashable.
    let meta = std::fs::symlink_metadata(&path).map_err(|_| Error::Missing(path.clone()))?;
    let _ = meta;

    let dir = match &home {
        Some(h) if same_device(h, &path) => h.clone(),
        _ => volume_trash(&path).ok_or_else(|| Error::NoTrash(path.clone()))?,
    };

    std::fs::create_dir_all(dir.join("files"))?;
    std::fs::create_dir_all(dir.join("info"))?;

    let name = name.to_string_lossy().into_owned();
    // Relative to the top directory for a volume trash, absolute for the home
    // one: a volume remounted elsewhere would restore to yesterday's path.
    let recorded = match top_dir_of(&dir) {
        Some(top) => path
            .strip_prefix(&top)
            .map(|r| r.to_path_buf())
            .unwrap_or_else(|_| path.clone()),
        None => path.clone(),
    };

    let stamp = local_stamp();
    let body = format!(
        "[Trash Info]\nPath={}\nDeletionDate={stamp}\n",
        encode(&recorded.to_string_lossy())
    );
    let mut from = 0;
    loop {
        let (n, claimed, mut info) = claim(&dir, &name, from)?;
        let forget = || {
            let _ = std::fs::remove_file(dir.join("info").join(format!("{claimed}.trashinfo")));
        };
        if let Err(e) = info
            .write_all(body.as_bytes())
            .and_then(|()| info.sync_all())
        {
            forget();
            return Err(Error::Io(e));
        }
        drop(info);

        // Never onto something: `files/` can hold a name whose info file is
        // gone, and a plain rename would replace what is there for good.
        let landed = dir.join("files").join(&claimed);
        match move_new(&path, &landed) {
            Ok(()) => return Ok(landed),
            Err(e) => {
                // The name was claimed and nothing moved into it; an orphan info
                // file is a phantom row in every file manager's trash listing.
                forget();
                if e.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(Error::Io(e));
                }
                from = n + 1;
            }
        }
    }
}

#[cfg(unix)]
/// Rename, refusing to replace whatever is at `to`. Linux says so in one call;
/// where it cannot — another kernel, a filesystem without the flag — a look
/// first leaves only the gap between the look and the rename.
pub(super) fn move_new(from: &Path, to: &Path) -> std::io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        let (Ok(a), Ok(b)) = (
            std::ffi::CString::new(from.as_os_str().as_bytes()),
            std::ffi::CString::new(to.as_os_str().as_bytes()),
        ) else {
            return Err(std::io::ErrorKind::InvalidInput.into());
        };
        // SAFETY: two NUL-terminated paths that outlive the call.
        let rc = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                a.as_ptr(),
                libc::AT_FDCWD,
                b.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if rc == 0 {
            return Ok(());
        }
        let e = std::io::Error::last_os_error();
        if !matches!(e.raw_os_error(), Some(libc::EINVAL | libc::ENOSYS)) {
            return Err(e);
        }
    }
    if std::fs::symlink_metadata(to).is_ok() {
        return Err(std::io::ErrorKind::AlreadyExists.into());
    }
    std::fs::rename(from, to)
}

#[cfg(unix)]
/// Is there somewhere to put this path, without moving it to find out? For a menu
/// that would rather grey an item out than fail after the press.
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
/// Claim a name by creating its info file, and return both. `create_new` in a
/// loop: asking whether a name is free and then taking it leaves a gap to race in.
/// A name whose data file is still in `files/` is not free either, info or no.
fn claim(dir: &Path, name: &str, from: u32) -> Result<(u32, String, std::fs::File), Error> {
    let (stem, ext) = split_extension(name);
    for n in from..10_000 {
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
            Ok(f) => {
                if std::fs::symlink_metadata(dir.join("files").join(&candidate)).is_ok() {
                    drop(f);
                    let _ = std::fs::remove_file(&at);
                    continue;
                }
                return Ok((n, candidate, f));
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(Error::Io(e)),
        }
    }
    Err(Error::NoTrash(dir.to_path_buf()))
}

#[cfg(unix)]
/// `notes.tar.gz` splits at the last dot, not the first: `notes.tar` + `gz`, so
/// the numbered form stays `notes.tar.2.gz` and still opens.
pub(super) fn split_extension(name: &str) -> (String, String) {
    match name.rfind('.') {
        // A leading dot is a hidden file, not an extension.
        Some(i) if i > 0 => (name[..i].to_owned(), name[i + 1..].to_owned()),
        _ => (name.to_owned(), String::new()),
    }
}

#[cfg(unix)]
/// Percent-encoding for the specification's `Path=` field: RFC 3986's unreserved
/// set plus `/`, which stays literal so the file is readable in an editor.
pub(super) fn encode(s: &str) -> String {
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
/// `YYYY-MM-DDThh:mm:ss` in local time, as the specification says: a file manager
/// shows this string to a person. `localtime_r` because `std` has no zone database.
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

/// Not implemented off Unix: Windows and macOS reach their own trash
/// through `SHFileOperation` and `NSFileManager`, not this specification. Faces
/// ask [`can_trash`] first and leave the item out of the menu.
#[cfg(not(unix))]
pub fn trash(_path: &Path) -> Result<PathBuf, Error> {
    Err(Error::Unsupported)
}

/// Always false off Unix. See [`trash`].
#[cfg(not(unix))]
pub fn can_trash(_path: &Path) -> bool {
    false
}
