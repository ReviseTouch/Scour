//! GNOME: one custom keybinding named Scour, through `gsettings`.

use std::path::Path;
use std::process::Command;

use crate::hotkey::Error;
use crate::key::Key;

pub(crate) const SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys";
/// Our own entry; the installer wrote the same path, so the two agree.
const PATH: &str = "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/scour/";

fn entry() -> String {
    format!("{SCHEMA}.custom-keybinding:{PATH}")
}

/// The binding of our entry, if the entry is in the list and has one.
pub(crate) fn current(gsettings: &Path) -> Result<Option<Key>, Error> {
    let list = run(gsettings, &["get", SCHEMA, "custom-keybindings"])?;
    if !parse_list(&list).iter().any(|p| p == PATH) {
        return Ok(None);
    }
    let binding = unquote(&run(gsettings, &["get", &entry(), "binding"])?);
    if binding.is_empty() {
        return Ok(None);
    }
    Key::from_gnome(&binding)
        .map(Some)
        .ok_or(Error::Unreadable(binding))
}

/// Write the entry and make sure the list names it.
pub(crate) fn bind(gsettings: &Path, key: &Key, command: &str) -> Result<(), Error> {
    let e = entry();
    run(gsettings, &["set", &e, "name", "Scour"])?;
    run(gsettings, &["set", &e, "command", command])?;
    run(gsettings, &["set", &e, "binding", &key.gnome()])?;
    let mut paths = parse_list(&run(gsettings, &["get", SCHEMA, "custom-keybindings"])?);
    if !paths.iter().any(|p| p == PATH) {
        paths.push(PATH.to_owned());
        run(
            gsettings,
            &["set", SCHEMA, "custom-keybindings", &format_list(&paths)],
        )?;
    }
    Ok(())
}

/// Take the entry out of the list and reset its keys.
pub(crate) fn clear(gsettings: &Path) -> Result<(), Error> {
    let mut paths = parse_list(&run(gsettings, &["get", SCHEMA, "custom-keybindings"])?);
    if paths.iter().any(|p| p == PATH) {
        paths.retain(|p| p != PATH);
        run(
            gsettings,
            &["set", SCHEMA, "custom-keybindings", &format_list(&paths)],
        )?;
    }
    run(gsettings, &["reset-recursively", &entry()])?;
    Ok(())
}

fn run(gsettings: &Path, args: &[&str]) -> Result<String, Error> {
    let out = Command::new(gsettings)
        .args(args)
        .output()
        .map_err(|e| Error::Tool(format!("{}: {e}", gsettings.display())))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_owned();
        return Err(Error::Tool(format!("gsettings {}: {err}", args.join(" "))));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// `['/a/', '/b/']` or `@as []` to the paths.
fn parse_list(s: &str) -> Vec<String> {
    let s = s.trim().trim_start_matches("@as").trim();
    let inner = s.trim_start_matches('[').trim_end_matches(']');
    inner
        .split(',')
        .map(|p| p.trim().trim_matches('\'').trim_matches('"').to_owned())
        .filter(|p| !p.is_empty())
        .collect()
}

fn format_list(paths: &[String]) -> String {
    if paths.is_empty() {
        return "@as []".to_owned();
    }
    let quoted: Vec<String> = paths.iter().map(|p| format!("'{p}'")).collect();
    format!("[{}]", quoted.join(", "))
}

/// `'<Super>f'` to `<Super>f`.
fn unquote(s: &str) -> String {
    s.trim().trim_matches('\'').trim_matches('"').to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_reads_both_ways() {
        assert!(parse_list("@as []").is_empty());
        assert!(parse_list("[]").is_empty());
        let two = parse_list("['/org/a/', '/org/b/']");
        assert_eq!(two, vec!["/org/a/", "/org/b/"]);
        assert_eq!(format_list(&two), "['/org/a/', '/org/b/']");
        assert_eq!(format_list(&[]), "@as []");
    }
}
