//! Which programs claim a kind of file: the desktop's entries and its
//! `mimeapps.list`, earlier directories winning.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One program that will take this kind of file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Opener {
    /// The desktop entry's file name — `org.gnome.gedit.desktop`. Stable, and
    /// what a face sends back when one is picked.
    pub id: String,
    /// What to show a person, in their language where the entry has one.
    pub name: String,
    /// The `Exec=` line, field codes and all. Use [`launch`]: the codes have to
    /// come out first.
    pub exec: String,
    /// Somebody chose this one for this type.
    pub preferred: bool,
}

/// Every program that claims this MIME type, the chosen one first. Read from disk
/// each time: this is asked once per menu, never per row.
pub fn openers(mime: &str) -> Vec<Opener> {
    let entries = desktop_entries();
    let (default, added) = associations(mime);

    let mut out: Vec<Opener> = Vec::new();
    let mut seen: Vec<String> = Vec::new();

    // Chosen first, in the order the file lists them.
    for id in default.iter().chain(added.iter()) {
        if seen.contains(id) {
            continue;
        }
        if let Some(entry) = entries.get(id) {
            seen.push(id.clone());
            out.push(Opener {
                id: id.clone(),
                name: entry.name.clone(),
                exec: entry.exec.clone(),
                preferred: default.contains(id),
            });
        }
    }

    // Then everything claiming the type, by name so the menu does not reshuffle.
    let mut rest: Vec<&Entry> = entries
        .values()
        .filter(|e| !seen.contains(&e.id) && e.takes(mime))
        .collect();
    rest.sort_by_cached_key(|a| a.name.to_lowercase());
    for entry in rest {
        out.push(Opener {
            id: entry.id.clone(),
            name: entry.name.clone(),
            exec: entry.exec.clone(),
            preferred: false,
        });
    }
    out
}

/// One desktop entry, reduced to what a menu needs.
#[derive(Debug, Clone)]
struct Entry {
    id: String,
    name: String,
    exec: String,
    mimes: Vec<String>,
}

impl Entry {
    fn takes(&self, mime: &str) -> bool {
        self.mimes.iter().any(|m| m == mime)
    }
}

/// Every usable desktop entry on this machine, keyed by file name. Earlier
/// directories win, so a home entry replaces the packaged one of the same name.
fn desktop_entries() -> BTreeMap<String, Entry> {
    let mut out: BTreeMap<String, Entry> = BTreeMap::new();
    for dir in data_dirs() {
        let apps = dir.join("applications");
        let Ok(listing) = std::fs::read_dir(&apps) else {
            continue;
        };
        for item in listing.flatten() {
            let name = item.file_name().to_string_lossy().into_owned();
            if !name.ends_with(".desktop") || out.contains_key(&name) {
                continue;
            }
            if let Some(entry) = read_entry(&item.path(), &name) {
                out.insert(name, entry);
            }
        }
    }
    out
}

fn read_entry(at: &Path, id: &str) -> Option<Entry> {
    let text = std::fs::read_to_string(at).ok()?;
    let lang = language();
    let mut name = String::new();
    let mut localised: Option<String> = None;
    let mut exec = String::new();
    let mut try_exec = String::new();
    let mut mimes: Vec<String> = Vec::new();
    let mut in_entry = false;

    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            // Only the main group: `[Desktop Action open]` has its own `Exec`.
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let (key, value) = (key.trim(), value.trim());
        match key {
            "Name" => name = value.to_owned(),
            "Exec" => exec = value.to_owned(),
            "TryExec" => try_exec = value.to_owned(),
            "MimeType" => {
                mimes = value
                    .split(';')
                    .filter(|m| !m.is_empty())
                    .map(str::to_owned)
                    .collect()
            }
            "NoDisplay" | "Hidden" if value == "true" => return None,
            _ => {
                if let Some(tag) = key.strip_prefix("Name[").and_then(|k| k.strip_suffix(']'))
                    && (tag == lang || tag.split('_').next() == Some(lang.as_str()))
                {
                    localised = Some(value.to_owned());
                }
            }
        }
    }

    if exec.is_empty() {
        return None;
    }
    if !try_exec.is_empty() && !runnable(&try_exec) {
        return None;
    }
    Some(Entry {
        id: id.to_owned(),
        name: localised.unwrap_or(name),
        exec,
        mimes,
    })
}

/// What `mimeapps.list` says: the chosen ones, and the added ones.
fn associations(mime: &str) -> (Vec<String>, Vec<String>) {
    let mut default: Vec<String> = Vec::new();
    let mut added: Vec<String> = Vec::new();

    for at in mimeapps_files() {
        let Ok(text) = std::fs::read_to_string(&at) else {
            continue;
        };
        let mut section = String::new();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                section = line.to_owned();
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            if key.trim() != mime {
                continue;
            }
            let ids = value
                .split(';')
                .filter(|v| !v.is_empty())
                .map(str::to_owned);
            match section.as_str() {
                "[Default Applications]" => default.extend(ids),
                "[Added Associations]" => added.extend(ids),
                _ => {}
            }
        }
    }
    (default, added)
}

/// The files that carry associations, in the order they win.
fn mimeapps_files() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(home) = config_home() {
        out.push(home.join("mimeapps.list"));
    }
    for dir in config_dirs() {
        out.push(dir.join("mimeapps.list"));
    }
    for dir in data_dirs() {
        out.push(dir.join("applications/mimeapps.list"));
    }
    out
}

fn data_dirs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    match std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        Some(v) => out.push(PathBuf::from(v)),
        None => {
            if let Some(home) = std::env::var_os("HOME") {
                out.push(PathBuf::from(home).join(".local/share"));
            }
        }
    }
    let dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    out.extend(dirs.split(':').filter(|d| !d.is_empty()).map(PathBuf::from));
    out
}

fn config_home() -> Option<PathBuf> {
    match std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        Some(v) => Some(PathBuf::from(v)),
        None => std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")),
    }
}

fn config_dirs() -> Vec<PathBuf> {
    std::env::var("XDG_CONFIG_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/etc/xdg".into())
        .split(':')
        .filter(|d| !d.is_empty())
        .map(PathBuf::from)
        .collect()
}

/// The two letters of the language, for `Name[tr]`.
fn language() -> String {
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(v) = std::env::var(key)
            && !v.is_empty()
            && v != "C"
            && v != "POSIX"
        {
            return v
                .split(['.', '@'])
                .next()
                .unwrap_or("en")
                .split('_')
                .next()
                .unwrap_or("en")
                .to_owned();
        }
    }
    "en".into()
}

/// Is this on the path, or an executable file where it points?
fn runnable(program: &str) -> bool {
    if program.contains('/') {
        return std::fs::metadata(program)
            .map(|m| m.is_file())
            .unwrap_or(false);
    }
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|d| {
                std::fs::metadata(d.join(program))
                    .map(|m| m.is_file())
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}
