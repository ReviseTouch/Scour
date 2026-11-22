//! What this machine says it can make a picture of, from two of its own
//! declarations: `$XDG_DATA_DIRS/thumbnailers/*.thumbnailer` for the command per
//! MIME type, and `$XDG_DATA_DIRS/mime/globs2` for name-to-type. No content
//! sniffing — opening every file in a result to pick a tile is the cost this
//! avoids — and no glob engine: only `*.ext` and literal names, 45 of 1,791 skipped.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Everything the two declarations say, read once.
#[derive(Debug, Default)]
pub struct Known {
    by_mime: HashMap<String, Vec<String>>,
    by_ext: HashMap<String, String>,
    by_name: HashMap<String, String>,
    icon_by_mime: HashMap<String, String>,
    generic_by_mime: HashMap<String, String>,
}

/// Ask the machine, once for the life of the process. A package installed while
/// the service runs is not noticed until it restarts; the alternative is
/// re-reading every declaration once per row of every result page.
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

    /// Could something on this machine make a picture of a file with this name?
    /// Two hash lookups and no I/O, because it is asked once per row of every
    /// answer. It says nothing about whether the attempt would succeed.
    pub fn can(&self, name: &str) -> bool {
        self.mime_of(name)
            .is_some_and(|mime| self.by_mime.contains_key(mime))
    }

    /// The command for this type, still holding the standard's `%i %u %o %s`.
    pub fn command_for(&self, mime: &str) -> Option<&[String]> {
        self.by_mime.get(mime).map(Vec::as_slice)
    }

    /// How many types this machine can draw.
    pub fn types(&self) -> usize {
        self.by_mime.len()
    }

    /// Freedesktop icon names for `ext` (in priority order): explicit MIME icon,
    /// `media-subtype`, `gnome-mime-media-subtype`, `generic-icons`, `media-x-generic`.
    pub fn mime_icons_for_ext(&self, ext: &str) -> Vec<String> {
        let mut out = Vec::new();
        let lower = ext.trim_start_matches('.').to_ascii_lowercase();
        let mime = self
            .by_ext
            .get(&lower)
            .or_else(|| {
                lower
                    .rsplit_once('.')
                    .and_then(|(_, tail)| self.by_ext.get(tail))
            })
            .map(String::as_str);
        let Some(mime) = mime else {
            return out;
        };
        let mut push = |s: String| {
            if !s.is_empty() && !out.contains(&s) {
                out.push(s);
            }
        };
        if let Some(ic) = self.icon_by_mime.get(mime) {
            push(ic.clone());
        }
        let dashed = mime.replace('/', "-");
        push(dashed.clone());
        push(format!("gnome-mime-{dashed}"));
        if let Some(generic) = self.generic_by_mime.get(mime) {
            push(generic.clone());
        }
        if let Some((media, _)) = mime.split_once('/') {
            push(format!("{media}-x-generic"));
        }
        out
    }
}

fn read() -> Known {
    let mut it = Known::default();
    // Lowest priority first, so the loop overwrites: `$XDG_DATA_HOME` beats
    // `$XDG_DATA_DIRS` and earlier entries beat later, so the order is reversed.
    for base in data_dirs().iter().rev() {
        take_globs(
            &mut it,
            &std::fs::read_to_string(base.join("mime/globs2")).unwrap_or_default(),
        );
        take_icon_map(
            &mut it.icon_by_mime,
            &std::fs::read_to_string(base.join("mime/icons")).unwrap_or_default(),
        );
        take_icon_map(
            &mut it.generic_by_mime,
            &std::fs::read_to_string(base.join("mime/generic-icons")).unwrap_or_default(),
        );
        take_thumbnailers(&mut it, &base.join("thumbnailers"));
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    fill_unix_fallbacks(&mut it);
    it
}

fn take_icon_map(into: &mut HashMap<String, String>, text: &str) {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((mime, icon)) = line.split_once(':') {
            let (mime, icon) = (mime.trim(), icon.trim());
            if !mime.is_empty() && !icon.is_empty() {
                into.insert(mime.to_ascii_lowercase(), icon.to_owned());
            }
        }
    }
}

/// KDE Plasma, XFCE, LXQt and minimal Wayland desktops often ship Poppler,
/// FFmpeg or ImageMagick without GNOME `.thumbnailer` files. Fill only gaps.
#[cfg(all(unix, not(target_os = "macos")))]
fn fill_unix_fallbacks(it: &mut Known) {
    const CORE_EXTS: &[(&str, &str)] = &[
        ("png", "image/png"),
        ("jpg", "image/jpeg"),
        ("jpeg", "image/jpeg"),
        ("jfif", "image/jpeg"),
        ("webp", "image/webp"),
        ("gif", "image/gif"),
        ("bmp", "image/bmp"),
        ("tif", "image/tiff"),
        ("tiff", "image/tiff"),
        ("avif", "image/avif"),
        ("heic", "image/heic"),
        ("heif", "image/heif"),
        ("jxl", "image/jxl"),
        ("svg", "image/svg+xml"),
        ("pdf", "application/pdf"),
        ("mp4", "video/mp4"),
        ("m4v", "video/mp4"),
        ("mkv", "video/x-matroska"),
        ("webm", "video/webm"),
        ("mov", "video/quicktime"),
        ("avi", "video/x-msvideo"),
        ("mpeg", "video/mpeg"),
        ("mpg", "video/mpeg"),
    ];
    for &(ext, mime) in CORE_EXTS {
        it.by_ext
            .entry(ext.to_owned())
            .or_insert_with(|| mime.to_owned());
    }

    const FALLBACKS: &[(&[&str], &[&str])] = &[
        (
            &["application/pdf"],
            &[
                "pdftoppm",
                "-png",
                "-singlefile",
                "-scale-to",
                "%s",
                "%i",
                "%O",
            ],
        ),
        (
            &["application/pdf"],
            &[
                "mutool", "draw", "-F", "png", "-w", "%s", "-o", "%o", "%i", "1",
            ],
        ),
        (
            &["application/pdf"],
            &[
                "gs",
                "-q",
                "-dSAFER",
                "-dBATCH",
                "-dNOPAUSE",
                "-sDEVICE=png16m",
                "-dFirstPage=1",
                "-dLastPage=1",
                "-r108",
                "-sOutputFile=%o",
                "%i",
            ],
        ),
        (
            &[
                "video/mp4",
                "video/x-matroska",
                "video/webm",
                "video/quicktime",
                "video/x-msvideo",
                "video/mpeg",
            ],
            &[
                "ffmpegthumbnailer",
                "-i",
                "%i",
                "-o",
                "%o",
                "-s",
                "%s",
                "-f",
            ],
        ),
        (
            &[
                "video/mp4",
                "video/x-matroska",
                "video/webm",
                "video/quicktime",
                "video/x-msvideo",
                "video/mpeg",
            ],
            &[
                "ffmpeg",
                "-nostdin",
                "-loglevel",
                "error",
                "-y",
                "-ss",
                "00:00:01",
                "-i",
                "%i",
                "-frames:v",
                "1",
                "-vf",
                "scale=%s:-1",
                "-f",
                "image2",
                "-vcodec",
                "png",
                "%o",
            ],
        ),
        (
            &["image/svg+xml"],
            &["rsvg-convert", "-w", "%s", "-f", "png", "-o", "%o", "%i"],
        ),
        (&["image/svg+xml"], &["resvg", "-w", "%s", "%i", "%o"]),
        (
            &[
                "image/png",
                "image/jpeg",
                "image/webp",
                "image/gif",
                "image/bmp",
                "image/tiff",
                "image/avif",
                "image/heic",
                "image/heif",
                "image/jxl",
            ],
            &["gdk-pixbuf-thumbnailer", "-s", "%s", "%i", "%o"],
        ),
        (
            &[
                "image/png",
                "image/jpeg",
                "image/webp",
                "image/gif",
                "image/bmp",
                "image/tiff",
                "image/avif",
                "image/heic",
                "image/heif",
                "image/jxl",
            ],
            &[
                "magick",
                "%i[0]",
                "-auto-orient",
                "-thumbnail",
                "%sx%s",
                "png:%o",
            ],
        ),
        (
            &[
                "image/png",
                "image/jpeg",
                "image/webp",
                "image/gif",
                "image/bmp",
                "image/tiff",
                "image/avif",
                "image/jxl",
            ],
            &[
                "ffmpeg",
                "-nostdin",
                "-loglevel",
                "error",
                "-y",
                "-i",
                "%i",
                "-frames:v",
                "1",
                "-vf",
                "scale=%s:-1",
                "-f",
                "image2",
                "-vcodec",
                "png",
                "%o",
            ],
        ),
    ];
    for &(mimes, cmd) in FALLBACKS {
        if !runnable(cmd[0]) {
            continue;
        }
        let words: Vec<String> = cmd.iter().map(|s| (*s).to_owned()).collect();
        for &mime in mimes {
            it.by_mime
                .entry(mime.to_owned())
                .or_insert_with(|| words.clone());
        }
    }
}

/// `$XDG_DATA_HOME` then `$XDG_DATA_DIRS`, in the standard's own order of
/// precedence — highest first.
pub(crate) fn data_dirs() -> Vec<PathBuf> {
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
    // Sorted: `read_dir` order is the filesystem's, so two thumbnailers claiming
    // one type would resolve differently between runs.
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
    // The standard's "is this installed" check: an entry left by a removed
    // package would otherwise be a failing spawn per file, forever.
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
        // The weight is ignored: the file arrives sorted by it, highest first,
        // so the first claim on a glob wins.
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

/// Whitespace, and double quotes when there are any — less than the desktop entry
/// specification allows. A line this cannot parse yields no thumbnailer at all,
/// never a wrong command.
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
        // `/bin/sh` stands in, so the `TryExec` check passes anywhere.
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
        // A wildcard this does not read is skipped, not half-understood.
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

    /// Against the real directories: a smoke test of the parse, not of a desktop.
    #[test]
    fn the_real_machine_can_be_read() {
        let it = known();
        for (mime, command) in &it.by_mime {
            assert!(!mime.is_empty(), "a thumbnailer claimed an empty type");
            assert!(!command.is_empty(), "{mime} has an empty command");
        }
    }
}
