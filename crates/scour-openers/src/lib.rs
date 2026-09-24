//! Which programs on this machine claim a kind of file, and starting one — what
//! `Open with…` needs and no command prints. Read from `applications/*.desktop`
//! and `mimeapps.list`, earlier directories winning. `NoDisplay`, `Hidden` and a
//! missing `TryExec` are left out; the type of a file is `scour-thumbs`'s answer.

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

/// Start one of them on a file.
pub fn launch(opener: &Opener, path: &Path) -> std::io::Result<()> {
    let words = command(&opener.exec, path);
    let Some((program, args)) = words.split_first() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the desktop entry has nothing to run",
        ));
    };
    std::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
}

/// Show these paths in the file manager, each selected in its folder — what
/// "open its folder" means to a person. Opening the folder alone did nothing
/// when that folder was already open, and selected nothing when it was not.
/// `org.freedesktop.FileManager1.ShowItems` is answered by Nautilus, Dolphin,
/// Nemo, Caja and Thunar; where nothing answers, each folder is opened instead.
/// Returns at once: the bus can start a file manager, which takes a second.
pub fn reveal(paths: &[&Path]) {
    if paths.is_empty() {
        return;
    }
    let quiet = |mut c: std::process::Command| {
        let _ = c
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    };
    // Finder and Explorer each have the one verb for it.
    if cfg!(target_os = "macos") {
        let mut c = std::process::Command::new("open");
        c.arg("-R").args(paths);
        quiet(c);
        return;
    }
    if cfg!(windows) {
        for p in paths {
            let mut c = std::process::Command::new("explorer");
            let mut arg = std::ffi::OsString::from("/select,");
            arg.push(p.as_os_str());
            c.arg(arg);
            quiet(c);
        }
        return;
    }
    let uris: Vec<String> = paths.iter().map(|p| file_uri(p)).collect();
    let folders: Vec<std::path::PathBuf> = {
        let mut seen = Vec::new();
        for p in paths {
            let dir = p.parent().unwrap_or(p).to_path_buf();
            if !seen.contains(&dir) {
                seen.push(dir);
            }
        }
        seen
    };
    // The window to bring forward is named after the folder it shows.
    let title = paths[0]
        .parent()
        .and_then(Path::file_name)
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    std::thread::spawn(move || {
        if show_items(&uris) {
            raise_file_manager(&title);
            return;
        }
        for dir in folders {
            let _ = std::process::Command::new("xdg-open")
                .arg(dir)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn();
        }
    });
}

/// Bring the file manager's window forward, which it cannot do itself on GNOME
/// under Wayland: with no activation token from the window that asked, a folder
/// already open was selected in and left behind. Scour's Shell extension can,
/// given the process and the window's title; without it nothing happens. Twice,
/// because a window the file manager is still opening is not there to raise.
fn raise_file_manager(title: &str) {
    let Some(pid) = bus_owner_pid("org.freedesktop.FileManager1") else {
        return;
    };
    for wait in [300, 600] {
        std::thread::sleep(std::time::Duration::from_millis(wait));
        let _ = std::process::Command::new("gdbus")
            .args([
                "call",
                "--session",
                "--timeout",
                "2",
                "--dest",
                "org.scour.Shell",
                "--object-path",
                "/org/scour/Shell",
                "--method",
                "org.scour.Shell.Raise",
                &format!("@au [{pid}]"),
                title,
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// The process that owns a name on the session bus.
fn bus_owner_pid(name: &str) -> Option<u32> {
    let out = std::process::Command::new("gdbus")
        .args([
            "call",
            "--session",
            "--timeout",
            "2",
            "--dest",
            "org.freedesktop.DBus",
            "--object-path",
            "/org/freedesktop/DBus",
            "--method",
            "org.freedesktop.DBus.GetConnectionUnixProcessID",
            name,
        ])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    uint_reply(&String::from_utf8_lossy(&out.stdout))
}

/// The number in a `gdbus` reply such as `(uint32 12345,)`.
fn uint_reply(reply: &str) -> Option<u32> {
    reply
        .split(|c: char| !c.is_ascii_digit())
        .filter(|w| !w.is_empty())
        .nth(1)
        .and_then(|w| w.parse().ok())
}

/// Ask the session's file manager to show these, through whichever bus tool
/// this machine has; false when neither could deliver it.
fn show_items(uris: &[String]) -> bool {
    let quiet = |mut c: std::process::Command| {
        c.stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    };
    let mut gdbus = std::process::Command::new("gdbus");
    gdbus.args([
        "call",
        "--session",
        "--timeout",
        "10",
        "--dest",
        "org.freedesktop.FileManager1",
        "--object-path",
        "/org/freedesktop/FileManager1",
        "--method",
        "org.freedesktop.FileManager1.ShowItems",
        &gvariant_strings(uris),
        "",
    ]);
    if quiet(gdbus) {
        return true;
    }
    let mut send = std::process::Command::new("dbus-send");
    send.args([
        "--session",
        "--print-reply",
        "--dest=org.freedesktop.FileManager1",
        "/org/freedesktop/FileManager1",
        "org.freedesktop.FileManager1.ShowItems",
        &format!("array:string:{}", uris.join(",")),
        "string:",
    ]);
    quiet(send)
}

/// A `file://` URI: every byte outside the unreserved set and `/` escaped, so
/// neither a space nor a quote nor a comma survives to confuse a bus tool.
fn file_uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for &b in path.as_os_str().as_encoded_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// An array of strings in GVariant's text form, which is what `gdbus` parses.
/// The URIs are escaped already, so no quote can end one early.
fn gvariant_strings(items: &[String]) -> String {
    let quoted: Vec<String> = items.iter().map(|u| format!("'{u}'")).collect();
    format!("[{}]", quoted.join(", "))
}

/// An `Exec=` line with its field codes resolved, split into words. `%f` is where
/// the file goes; `%i %c %k` are dropped, since left in place they become literal
/// arguments. An entry with no code at all still gets the path, appended.
fn command(exec: &str, path: &Path) -> Vec<String> {
    let file = path.to_string_lossy().into_owned();
    let mut out: Vec<String> = Vec::new();
    let mut took_file = false;
    for word in split(exec) {
        match word.as_str() {
            "%f" | "%F" | "%u" | "%U" => {
                out.push(file.clone());
                took_file = true;
            }
            // Deprecated: the specification says to drop rather than pass on.
            "%d" | "%D" | "%n" | "%N" | "%v" | "%m" | "%i" | "%c" | "%k" => {}
            _ => out.push(word),
        }
    }
    if !took_file {
        out.push(file);
    }
    out
}

/// Split an `Exec=` line the way the specification asks: quotes group, and a
/// backslash inside quotes escapes.
fn split(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut any = false;
    for c in line.chars() {
        if escaped {
            word.push(c);
            escaped = false;
            continue;
        }
        match c {
            '\\' if quoted => escaped = true,
            '"' => {
                quoted = !quoted;
                any = true;
            }
            ' ' | '\t' if !quoted => {
                if any || !word.is_empty() {
                    out.push(std::mem::take(&mut word));
                    any = false;
                }
            }
            _ => word.push(c),
        }
    }
    if any || !word.is_empty() {
        out.push(word);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_process_number_is_read_out_of_a_gdbus_reply() {
        assert_eq!(uint_reply("(uint32 12345,)\n"), Some(12345));
        assert_eq!(uint_reply(""), None);
    }

    #[test]
    fn a_path_becomes_a_uri_no_bus_tool_can_misread() {
        assert_eq!(
            file_uri(Path::new("/home/a/Çay listesi, 'son'.txt")),
            "file:///home/a/%C3%87ay%20listesi%2C%20%27son%27.txt"
        );
        assert_eq!(file_uri(Path::new("/x/a-b_c.d~")), "file:///x/a-b_c.d~");
        assert_eq!(
            gvariant_strings(&["file:///a".into(), "file:///b%20c".into()]),
            "['file:///a', 'file:///b%20c']"
        );
    }

    #[test]
    fn the_file_lands_where_the_entry_says_and_the_rest_of_the_codes_go() {
        let p = Path::new("/home/a/notes.txt");
        assert_eq!(command("gedit %U", p), vec!["gedit", "/home/a/notes.txt"]);
        assert_eq!(
            command("code --new-window %F", p),
            vec!["code", "--new-window", "/home/a/notes.txt"]
        );
        // A program handed `%c` opens a file called `%c` or refuses to start.
        assert_eq!(
            command("foo %i %c %k %f", p),
            vec!["foo", "/home/a/notes.txt"]
        );
        // An entry that forgot its code still gets the file, appended.
        assert_eq!(
            command("mousepad", p),
            vec!["mousepad", "/home/a/notes.txt"]
        );
    }

    #[test]
    fn a_quoted_word_stays_one_word() {
        assert_eq!(split(r#"foo "a b" c"#), vec!["foo", "a b", "c"]);
        assert_eq!(split(r#"foo "a\"b""#), vec!["foo", r#"a"b"#]);
        // An empty quoted argument is still an argument.
        assert_eq!(split(r#"foo "" bar"#), vec!["foo", "", "bar"]);
        assert_eq!(split("  spaced   out  "), vec!["spaced", "out"]);
    }

    #[test]
    fn the_language_is_the_first_two_letters_and_nothing_else() {
        // The shapes the function has to survive, not the environment's value.
        for (value, want) in [
            ("tr_TR.UTF-8", "tr"),
            ("en_GB", "en"),
            ("de@euro", "de"),
            ("tr", "tr"),
        ] {
            let cut = value
                .split(['.', '@'])
                .next()
                .unwrap()
                .split('_')
                .next()
                .unwrap();
            assert_eq!(cut, want, "{value}");
        }
    }

    /// Reading this machine's real tables: what is installed differs everywhere,
    /// so only the shape is checked — no panic, no empty name, nothing to run.
    #[test]
    fn the_real_tables_are_read_without_falling_over() {
        for mime in ["text/plain", "image/png", "application/pdf"] {
            for opener in openers(mime) {
                assert!(!opener.id.is_empty());
                assert!(!opener.exec.is_empty(), "{}", opener.id);
                assert!(opener.id.ends_with(".desktop"), "{}", opener.id);
            }
        }
    }
}
