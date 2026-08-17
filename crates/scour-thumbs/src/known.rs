//! What this machine says it can make a picture of.
//!
//! Two declarations, both the desktop's own, neither of them ours:
//!
//! * **`$XDG_DATA_DIRS/thumbnailers/*.thumbnailer`** — the thumbnail managing
//!   standard's contract. Per MIME type, the command that turns a file into a
//!   PNG. This is read rather than guessed at: a hardcoded list of programs
//!   would be a fifth opinion about a question the machine already answers, it
//!   would be wrong the moment a package is installed or removed, and it would
//!   silently stop matching what Files and Loupe do.
//! * **`$XDG_DATA_DIRS/mime/globs2`** — the shared MIME-info database, which
//!   is how a name becomes a MIME type. Also read rather than guessed at, for
//!   the same reason and one more: the thumbnailers are *keyed* by MIME type,
//!   so any table of our own would have to agree with this one exactly to be
//!   worth having.
//!
//! ## What is deliberately not done
//!
//! **No content sniffing.** The MIME database also carries magic byte
//! patterns, and the full rule prefers them over the name in some cases. That
//! would mean opening every file in a result list to decide whether to draw a
//! tile, which is the cost the whole design is arranged to avoid. Every type
//! anything on this machine can thumbnail — images, video, PDF, office
//! documents — is named by its extension. A file whose name says nothing gets
//! the glyph it gets today.
//!
//! **No glob engine.** Only `*.ext` and bare literal names are taken from
//! `globs2`; patterns with a wildcard anywhere else are skipped. On this
//! machine that leaves 45 of 1,791 globs unread, none of which any installed
//! thumbnailer claims.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Everything the two declarations say, read once.
#[derive(Debug, Default)]
pub struct Known {
    by_mime: HashMap<String, Vec<String>>,
    by_ext: HashMap<String, String>,
    by_name: HashMap<String, String>,
}

/// Ask the machine, once for the life of the process.
///
/// **Once, and never again**, which is a decision rather than an oversight: a
/// package installed while the service is running will not be noticed until it
/// restarts. The alternative is re-reading nine files per row of every result
/// page, and the thing being avoided — a thumbnailer appearing mid-session —
/// costs a restart to pick up and nothing to be wrong about in the meantime.
pub fn known() -> &'static Known {
    static IT: std::sync::LazyLock<Known> = std::sync::LazyLock::new(read);
    &IT
}

impl Known {
    /// The MIME type this machine gives a file with this name.
    pub fn mime_of(&self, name: &str) -> Option<&str> {
        let lower = name.to_lowercase();
        if let Some(mime) = self.by_name.get(&lower) {
            return Some(mime);
        }
        let (_, ext) = lower.rsplit_once('.')?;
        self.by_ext.get(ext).map(String::as_str)
    }

    /// Could something on this machine make a picture of a file with this
    /// name?
    ///
    /// **Two hash lookups and no I/O**, because this is asked once per row of
    /// every answer — the same budget `has_thumbnail` was cut down to. It says
    /// nothing about whether the attempt would succeed; that costs a process,
    /// and the answer to it is the failure directory.
    pub fn can(&self, name: &str) -> bool {
        self.mime_of(name)
            .is_some_and(|mime| self.by_mime.contains_key(mime))
    }

    /// The command that makes a picture of this type, as a template still
    /// holding the standard's `%i` `%u` `%o` `%s`.
    pub fn command_for(&self, mime: &str) -> Option<&[String]> {
        self.by_mime.get(mime).map(Vec::as_slice)
    }

    /// How many types this machine can draw. For diagnostics and for the test
    /// that this read anything at all.
    pub fn types(&self) -> usize {
        self.by_mime.len()
    }
}

fn read() -> Known {
    let mut it = Known::default();
    // **Lowest priority first, so the loop below simply overwrites.**
    // `$XDG_DATA_HOME` beats `$XDG_DATA_DIRS`, and earlier entries in
    // `XDG_DATA_DIRS` beat later ones, so the search order is reversed here
    // and the last writer wins.
    for base in data_dirs().iter().rev() {
        take_globs(
            &mut it,
            &std::fs::read_to_string(base.join("mime/globs2")).unwrap_or_default(),
        );
        take_thumbnailers(&mut it, &base.join("thumbnailers"));
    }
    it
}

/// `$XDG_DATA_HOME` then `$XDG_DATA_DIRS`, in the standard's own order of
/// precedence — highest first.
fn data_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let mut out = vec![
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share")),
    ];
    let dirs = std::env::var("XDG_DATA_DIRS").unwrap_or_default();
    let dirs = if dirs.trim().is_empty() {
        "/usr/local/share:/usr/share".to_owned()
    } else {
        dirs
    };
    out.extend(dirs.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));
    out
}

fn take_thumbnailers(it: &mut Known, dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    // Sorted, because `read_dir` is in whatever order the filesystem hands
    // back and two thumbnailers claiming one type would otherwise be resolved
    // differently on different machines — and differently between two runs on
    // the same one.
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "thumbnailer"))
        .collect();
    files.sort();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        take_thumbnailer(it, &text);
    }
}

fn take_thumbnailer(it: &mut Known, text: &str) {
    let mut exec = None;
    let mut try_exec = None;
    let mut mimes = None;
    let mut inside = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            inside = line == "[Thumbnailer Entry]";
            continue;
        }
        if !inside {
            continue;
        }
        match line.split_once('=') {
            Some(("Exec", v)) => exec = Some(v.trim()),
            Some(("TryExec", v)) => try_exec = Some(v.trim()),
            Some(("MimeType", v)) => mimes = Some(v.trim()),
            _ => {}
        }
    }
    let (Some(exec), Some(mimes)) = (exec, mimes) else {
        return;
    };
    // The standard's own "is this installed" check, and it is worth honouring:
    // an entry left behind by a removed package would otherwise be a process
    // spawn that fails, per file, forever.
    if let Some(binary) = try_exec
        && !runnable(binary)
    {
        return;
    }
    let words = split_command(exec);
    if words.is_empty() || !runnable(&words[0]) {
        return;
    }
    for mime in mimes.split(';').map(str::trim).filter(|m| !m.is_empty()) {
        it.by_mime.insert(mime.to_lowercase(), words.clone());
    }
}

/// Is there a program by this name that can be run?
fn runnable(binary: &str) -> bool {
    let is_exec = |p: &Path| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        }
        #[cfg(not(unix))]
        {
            p.is_file()
        }
    };
    if binary.contains('/') {
        return is_exec(Path::new(binary));
    }
    let path = std::env::var("PATH").unwrap_or_default();
    path.split(':')
        .filter(|d| !d.is_empty())
        .any(|d| is_exec(&Path::new(d).join(binary)))
}

/// `weight:mime:glob` — the shared MIME-info database's own format.
fn take_globs(it: &mut Known, text: &str) {
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let mut parts = line.split(':');
        let (Some(_weight), Some(mime), Some(glob)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        // The weight is deliberately ignored: this file is already sorted by
        // it, highest first, so the first claim on a glob wins and later ones
        // are the alternatives. Written the other way round the parse would
        // have to re-sort a file that arrives sorted.
        let mime = mime.to_lowercase();
        if let Some(ext) = glob.strip_prefix("*.")
            && !ext.contains(['*', '?', '['])
        {
            it.by_ext.entry(ext.to_lowercase()).or_insert(mime);
        } else if !glob.contains(['*', '?', '[']) {
            it.by_name.entry(glob.to_lowercase()).or_insert(mime);
        }
    }
}

/// Whitespace, and double quotes when there are any.
///
/// The desktop entry specification defines a fuller quoting than this. None of
/// the nine thumbnailers installed here uses any of it — every `Exec` is bare
/// words — so what is implemented is what is used, and a line this cannot
/// parse produces no thumbnailer rather than a wrong command.
fn split_command(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut escaped = false;
    let mut any = false;
    for c in line.chars() {
        match c {
            _ if escaped => {
                word.push(c);
                escaped = false;
            }
            '\\' if quoted => escaped = true,
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any || !word.is_empty() {
                    out.push(std::mem::take(&mut word));
                }
                any = false;
            }
            c => word.push(c),
        }
    }
    if quoted {
        return Vec::new();
    }
    if any || !word.is_empty() {
        out.push(word);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_thumbnailer_entry_is_read() {
        let mut it = Known::default();
        // `/bin/sh` stands in for the thumbnailer, because the test has to
        // survive the `TryExec` check on a machine with nothing installed.
        take_thumbnailer(
            &mut it,
            "[Thumbnailer Entry]\nTryExec=/bin/sh\nExec=/bin/sh -c x %i %o %s\nMimeType=image/png;image/gif;\n",
        );
        assert_eq!(it.types(), 2);
        assert_eq!(
            it.command_for("image/png").unwrap(),
            ["/bin/sh", "-c", "x", "%i", "%o", "%s"]
        );
        assert!(it.command_for("image/jpeg").is_none());
    }

    /// A package that was removed leaves its declaration behind.
    #[test]
    fn a_thumbnailer_whose_program_is_gone_is_not_offered() {
        let mut it = Known::default();
        take_thumbnailer(
            &mut it,
            "[Thumbnailer Entry]\nTryExec=/nowhere/at/all\nExec=/nowhere/at/all %i %o\nMimeType=image/png;\n",
        );
        assert_eq!(it.types(), 0);
        // And when there is no `TryExec` at all, the `Exec` itself is checked.
        take_thumbnailer(
            &mut it,
            "[Thumbnailer Entry]\nExec=/nowhere/at/all %i %o\nMimeType=image/png;\n",
        );
        assert_eq!(it.types(), 0);
    }

    /// Keys outside the entry's own group are somebody else's.
    #[test]
    fn only_the_thumbnailer_group_is_read() {
        let mut it = Known::default();
        take_thumbnailer(
            &mut it,
            "[Desktop Entry]\nExec=/bin/sh %i\nMimeType=image/png;\n",
        );
        assert_eq!(it.types(), 0);
    }

    #[test]
    fn globs_become_extensions_and_names() {
        let mut it = Known::default();
        take_globs(
            &mut it,
            "# a comment\n50:image/png:*.png\n50:text/x-makefile:Makefile\n50:application/x-nothing:*.tar.*\n",
        );
        assert_eq!(it.mime_of("holiday.PNG"), Some("image/png"));
        assert_eq!(it.mime_of("makefile"), Some("text/x-makefile"));
        // A glob with a wildcard where this does not look is skipped rather
        // than half-understood.
        assert_eq!(it.mime_of("a.tar.gz"), None);
        assert_eq!(it.mime_of("a"), None);
        assert_eq!(it.mime_of(""), None);
    }

    /// Highest weight first is how `globs2` arrives; the first claim wins.
    #[test]
    fn the_first_claim_on_an_extension_wins() {
        let mut it = Known::default();
        take_globs(&mut it, "80:image/png:*.png\n50:application/wrong:*.png\n");
        assert_eq!(it.mime_of("a.png"), Some("image/png"));
    }

    #[test]
    fn can_needs_both_halves() {
        let mut it = Known::default();
        take_globs(&mut it, "50:image/png:*.png\n50:video/mp4:*.mp4\n");
        take_thumbnailer(
            &mut it,
            "[Thumbnailer Entry]\nExec=/bin/sh %i %o\nMimeType=image/png;\n",
        );
        assert!(it.can("a.png"));
        // A type the machine can name but cannot draw.
        assert!(!it.can("a.mp4"));
        // A name the machine cannot even type.
        assert!(!it.can("notes"));
    }

    #[test]
    fn a_command_line_splits_on_spaces_and_quotes() {
        assert_eq!(split_command("a b c"), ["a", "b", "c"]);
        assert_eq!(split_command("  a   b  "), ["a", "b"]);
        assert_eq!(split_command(r#"a "b c" d"#), ["a", "b c", "d"]);
        assert_eq!(split_command(r#"a "b\"c""#), ["a", "b\"c"]);
        // An empty argument is an argument.
        assert_eq!(split_command(r#"a "" b"#), ["a", "", "b"]);
        // Unbalanced: no command rather than a wrong one.
        assert_eq!(split_command(r#"a "b"#), Vec::<String>::new());
        assert_eq!(split_command(""), Vec::<String>::new());
    }

    /// Whatever this machine has, reading it must not panic and must not
    /// invent. Run against the real directories, so it is a smoke test of the
    /// parse rather than of any particular desktop.
    #[test]
    fn the_real_machine_can_be_read() {
        let it = known();
        for (mime, command) in &it.by_mime {
            assert!(!mime.is_empty(), "a thumbnailer claimed an empty type");
            assert!(!command.is_empty(), "{mime} has an empty command");
        }
    }
}
