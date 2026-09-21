//! KDE: the launch key of `scour.desktop` in `kglobalshortcutsrc`.

use std::path::Path;
use std::process::Command;

use crate::hotkey::Error;
use crate::key::Key;

const FILE: &str = "kglobalshortcutsrc";
const ARGS: [&str; 7] = [
    "--file",
    FILE,
    "--group",
    "services",
    "--group",
    "scour.desktop",
    "--key",
];

pub(crate) fn current(kreadconfig: &Path) -> Result<Option<Key>, Error> {
    let out = Command::new(kreadconfig)
        .args(ARGS)
        .arg("_launch")
        .output()
        .map_err(|e| Error::Tool(format!("{}: {e}", kreadconfig.display())))?;
    if !out.status.success() {
        return Err(Error::Tool(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }
    let value = String::from_utf8_lossy(&out.stdout);
    // Several keys are separated by a tab or a comma; the first is the one shown.
    let first = value
        .split(['\t', ','])
        .next()
        .unwrap_or("")
        .trim()
        .to_owned();
    if first.is_empty() || first == "none" {
        return Ok(None);
    }
    Key::from_kde(&first)
        .map(Some)
        .ok_or(Error::Unreadable(first))
}

pub(crate) fn bind(kwriteconfig: &Path, key: &Key) -> Result<(), Error> {
    write(kwriteconfig, &["_launch", &key.kde()])
}

pub(crate) fn clear(kwriteconfig: &Path) -> Result<(), Error> {
    write(kwriteconfig, &["_launch", "--delete"])
}

fn write(kwriteconfig: &Path, tail: &[&str]) -> Result<(), Error> {
    let out = Command::new(kwriteconfig)
        .args(ARGS)
        .args(tail)
        .output()
        .map_err(|e| Error::Tool(format!("{}: {e}", kwriteconfig.display())))?;
    if !out.status.success() {
        return Err(Error::Tool(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }
    Ok(())
}
