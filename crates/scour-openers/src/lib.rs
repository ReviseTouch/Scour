//! Which programs on this machine claim a kind of file, and starting one.
//!
//! **This is what `Open with…` needs and `xdg-open` does not give.** Opening a
//! file is one call to `xdg-open`; offering a *choice* means reading the same
//! tables `xdg-open` reads and showing what is in them. There is no command
//! that prints the list — `xdg-mime query default` gives one answer and no
//! alternatives — so the tables are read here.
//!
//! ## Where the answer comes from
//!
//! * **`applications/*.desktop`** under `$XDG_DATA_HOME` and each
//!   `$XDG_DATA_DIRS` entry. A desktop entry names itself, says what to run,
//!   and lists the MIME types it will take.
//! * **`mimeapps.list`**, under `$XDG_CONFIG_HOME`, each `$XDG_CONFIG_DIRS`
//!   entry, and beside the applications themselves. Its `[Default
//!   Applications]` section is what somebody chose, and it wins; its
//!   `[Added Associations]` adds programs the desktop file itself does not
//!   claim.
//!
//! Earlier directories win, which is the specification's rule and the reason a
//! choice made in a home directory beats a package's.
//!
//! ## What is deliberately left out
//!
//! `NoDisplay` and `Hidden` entries, and anything whose `TryExec` is not on the
//! path. All three mean "do not offer this to a person", and a menu that offers
//! a program which is not installed is a menu that fails after the press.
//!
//! The type of a file is **not** worked out here. `scour-thumbs` already reads
//! the shared MIME database to answer that, and two readers of one table is the
//! drift this workspace keeps writing down.

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
    /// The `Exec=` line, field codes and all. Use [`launch`] rather than
    /// running it: the codes have to come out first.
    pub exec: String,
    /// Somebody chose this one for this type.
    pub preferred: bool,
}

/// Every program that claims this MIME type, the chosen one first.
///
/// The list is read from disk each time. That is deliberate: this is asked once
/// when a menu is opened, never per row, and a person who has just installed an
/// editor expects to see it without restarting a search tool.
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

    // Then everything that claims the type itself, by name so the list does not
    // reshuffle between two openings of the same menu.
    let mut rest: Vec<&Entry> = entries
        .values()
        .filter(|e| !seen.contains(&e.id) && e.takes(mime))
        .collect();
    rest.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
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

/// An `Exec=` line with its field codes resolved, split into words.
///
/// **The codes are not optional decoration.** `%f` is where the file goes and
/// `%i %c %k` are things a launcher fills in that a search tool has no business
/// filling in — left in place they become literal arguments, and a program
/// handed `%c` as a file name opens a file called `%c` or refuses to start.
/// An entry with no code at all still gets the path, appended: that is what
/// every launcher does, and an entry that forgot its `%f` is common.
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
            // Deprecated and meaningless here; the specification says to drop
            // them rather than pass them on.
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

/// Every usable desktop entry on this machine, keyed by file name.
///
/// Earlier directories win: `$XDG_DATA_HOME` before `$XDG_DATA_DIRS`, so a
/// entry a person put in their own home replaces the packaged one of the same
/// name rather than appearing beside it.
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
            // Only the main group. An action's `[Desktop Action open]` has its
            // own `Exec`, and taking that one starts the wrong thing.
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
            let ids = value.split(';').filter(|v| !v.is_empty()).map(str::to_owned);
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
        return std::fs::metadata(program).map(|m| m.is_file()).unwrap_or(false);
    }
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .any(|d| std::fs::metadata(d.join(program)).map(|m| m.is_file()).unwrap_or(false))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The field codes, which are the part that goes wrong quietly.
    #[test]
    fn the_file_lands_where_the_entry_says_and_the_rest_of_the_codes_go() {
        let p = Path::new("/home/a/notes.txt");
        assert_eq!(command("gedit %U", p), vec!["gedit", "/home/a/notes.txt"]);
        assert_eq!(command("code --new-window %F", p),
                   vec!["code", "--new-window", "/home/a/notes.txt"]);
        // `%i %c %k` are a launcher's to fill in, and a program handed `%c`
        // opens a file called `%c` or refuses to start.
        assert_eq!(command("foo %i %c %k %f", p), vec!["foo", "/home/a/notes.txt"]);
        // An entry that forgot its code still gets the file, appended — which
        // is what every launcher does.
        assert_eq!(command("mousepad", p), vec!["mousepad", "/home/a/notes.txt"]);
    }

    /// A path with a space in it is the ordinary case, not an edge one.
    #[test]
    fn a_quoted_word_stays_one_word() {
        assert_eq!(split(r#"foo "a b" c"#), vec!["foo", "a b", "c"]);
        assert_eq!(split(r#"foo "a\"b""#), vec!["foo", r#"a"b"#]);
        // An empty quoted argument is still an argument.
        assert_eq!(split(r#"foo "" bar"#), vec!["foo", "", "bar"]);
        assert_eq!(split("  spaced   out  "), vec!["spaced", "out"]);
    }

    /// The two letters, out of whatever shape the variable is in.
    #[test]
    fn the_language_is_the_first_two_letters_and_nothing_else() {
        // Not asserted against the environment — that is the caller's — but
        // the shapes the function has to survive.
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

    /// Reading this machine's real tables, without asserting what is on it.
    ///
    /// **A test that says nothing about the answer and everything about the
    /// shape of it.** What is installed differs on every machine, so the thing
    /// worth checking is that the reader survives the real files: no panic, no
    /// entry with an empty name, no entry with nothing to run.
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
