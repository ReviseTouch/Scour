//! Windows: the key is the Start menu shortcut's own. Explorer registers the
//! hot key of every shortcut there, so nothing of Scour's has to be running,
//! and it works the same whether the index is a service's or the session's.
//! A shortcut carries Ctrl, Alt and Shift only: the Windows key is the shell's.

use std::path::{Path, PathBuf};

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, IPersistFile,
    STGM_READWRITE,
};
use windows::Win32::UI::Shell::{
    IShellLinkW, SHCNE_UPDATEITEM, SHCNF_PATHW, SHChangeNotify, ShellLink,
};
use windows::core::{HSTRING, Interface};

use crate::hotkey::Error;
use crate::key::Key;

/// The Start menu entry `scour-gui --install` writes, and this writes if
/// nothing has.
fn shortcut() -> Result<PathBuf, Error> {
    std::env::var_os("APPDATA")
        .map(|d| PathBuf::from(d).join(r"Microsoft\Windows\Start Menu\Programs\Scour.lnk"))
        .ok_or_else(|| Error::Tool("APPDATA is not set".to_owned()))
}

fn tool(e: windows::core::Error) -> Error {
    Error::Tool(format!("Start menu shortcut: {e}"))
}

/// The shortcut, loaded when it is there. `None` when it is not.
fn open(at: &Path) -> Result<Option<(IShellLinkW, IPersistFile)>, Error> {
    // SAFETY: COM on this thread, one object, released when it drops.
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let link: IShellLinkW =
            CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(tool)?;
        let file: IPersistFile = link.cast().map_err(tool)?;
        if !at.is_file() {
            return Ok(None);
        }
        file.Load(&HSTRING::from(at.as_os_str()), STGM_READWRITE)
            .map_err(tool)?;
        Ok(Some((link, file)))
    }
}

pub fn current() -> Result<Option<Key>, Error> {
    let Some((link, _)) = open(&shortcut()?)? else {
        return Ok(None);
    };
    // SAFETY: a getter on an object this function owns.
    let word = unsafe { link.GetHotkey() }.map_err(tool)?;
    if word == 0 {
        return Ok(None);
    }
    Key::from_windows(word)
        .map(Some)
        .ok_or_else(|| Error::Unreadable(format!("{word:#06x}")))
}

/// Write `key` on the shortcut, making the shortcut — to `target`, the window
/// beside this program — when a copy unpacked from a zip has none yet.
pub fn bind(key: &Key, target: &Path) -> Result<(), Error> {
    let word = key
        .windows()
        .ok_or(Error::Refused(crate::hotkey::WINDOWS_REFUSED))?;
    set(word, Some(target))
}

pub fn clear() -> Result<(), Error> {
    set(0, None)
}

fn set(word: u16, target: Option<&Path>) -> Result<(), Error> {
    let at = shortcut()?;
    let (link, file) = match (open(&at)?, target) {
        (Some(both), _) => both,
        // Nothing to clear a key from.
        (None, None) => return Ok(()),
        (None, Some(target)) => {
            if let Some(dir) = at.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            // SAFETY: as in `open`; the strings outlive the calls.
            unsafe {
                let link: IShellLinkW =
                    CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER).map_err(tool)?;
                link.SetPath(&HSTRING::from(target.as_os_str()))
                    .map_err(tool)?;
                if let Some(dir) = target.parent() {
                    link.SetWorkingDirectory(&HSTRING::from(dir.as_os_str()))
                        .map_err(tool)?;
                }
                link.SetDescription(&HSTRING::from("Scour: find any file"))
                    .map_err(tool)?;
                let file: IPersistFile = link.cast().map_err(tool)?;
                (link, file)
            }
        }
    };
    let path = HSTRING::from(at.as_os_str());
    // SAFETY: the object is this function's; the path outlives both calls, and
    // the notification only names it.
    unsafe {
        link.SetHotkey(word).map_err(tool)?;
        file.Save(&path, true).map_err(tool)?;
        // Explorer reads a shortcut's key when it is told the file changed.
        SHChangeNotify(
            SHCNE_UPDATEITEM,
            SHCNF_PATHW,
            Some(path.as_ptr().cast()),
            None,
        );
    }
    Ok(())
}
