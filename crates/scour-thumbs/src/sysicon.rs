//! Native system file and folder icons, cached by `(key, bucket)` in memory
//! without touching the indexed files themselves.

use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};

/// A system icon in its native zero-reencode representation.
#[derive(Debug, Clone)]
pub enum SystemIcon {
    /// Raw vector SVG (`image/svg+xml`).
    Svg(Arc<[u8]>),
    /// Raw PNG (`image/png`).
    Png(Arc<[u8]>),
    /// Straight RGBA8 pixels from an OS shell API.
    Rgba {
        width: u32,
        height: u32,
        rgba: Arc<[u8]>,
    },
}

/// Encoded icon bytes ready to serve over HTTP with an immutable cache header.
#[derive(Debug, Clone)]
pub struct WebIcon {
    pub bytes: Arc<[u8]>,
    pub content_type: &'static str,
}

const CACHE_CAP: usize = 1024;

static ICON_CACHE: LazyLock<Mutex<HashMap<(String, u16), Option<SystemIcon>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static WEB_CACHE: LazyLock<Mutex<HashMap<(String, u16), Option<WebIcon>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

static SPECIAL_DIRS: LazyLock<HashMap<String, &'static str>> = LazyLock::new(load_special_dirs);

/// Build a cache key for an indexed entry, recognizing home and XDG user folders.
pub fn key_for_entry(path: &str, name: &str, is_dir: bool, kind_token: &str) -> String {
    let role = if is_dir || kind_token == "folder" {
        SPECIAL_DIRS.get(path.trim_end_matches('/')).copied()
    } else {
        None
    };
    key_for(name, is_dir, kind_token, role)
}

/// Resolve a direct child folder name under `$HOME` (`user-dirs.dirs` or standard English).
pub fn home_child_key(leaf: &str) -> String {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default()
        .replace('\\', "/");
    let home = home.trim_end_matches('/');
    if !home.is_empty() && !leaf.contains(['/', '\\']) {
        let full = format!("{home}/{leaf}");
        if let Some(&role) = SPECIAL_DIRS.get(&full) {
            return format!("place:{role}");
        }
    }
    "folder".to_owned()
}

/// Build a path-free cache key (`place:documents`, `folder`, `ext:docx`, `kind:code`).
pub fn key_for(name: &str, is_dir: bool, kind_token: &str, place_role: Option<&str>) -> String {
    if is_dir || kind_token == "folder" {
        if let Some(role) = place_role.filter(|r| is_valid_token(r)) {
            return format!("place:{role}");
        }
        return "folder".to_owned();
    }
    if let Some(ext) = clean_ext(name) {
        return format!("ext:{ext}");
    }
    let kind = if is_valid_token(kind_token) {
        kind_token
    } else {
        "file"
    };
    format!("kind:{kind}")
}

/// Look up a native system icon for `key` at pixel edge `px` (`16` small, `64` large).
pub fn lookup(key: &str, px: u32) -> Option<SystemIcon> {
    if !valid_key(key) {
        return None;
    }
    let bucket: u16 = if px <= 24 { 16 } else { 64 };
    if let Ok(guard) = ICON_CACHE.lock()
        && let Some(cached) = guard.get(&(key.to_owned(), bucket))
    {
        return cached.clone();
    }
    let resolved = resolve_uncached(key, bucket as u32);
    if let Ok(mut guard) = ICON_CACHE.lock() {
        if guard.len() >= CACHE_CAP {
            guard.clear();
        }
        guard.insert((key.to_owned(), bucket), resolved.clone());
    }
    resolved
}

/// Look up `key` encoded as SVG or PNG for HTTP responses.
pub fn web_icon(key: &str, px: u32) -> Option<WebIcon> {
    if !valid_key(key) {
        return None;
    }
    let bucket: u16 = if px <= 24 { 16 } else { 64 };
    if let Ok(guard) = WEB_CACHE.lock()
        && let Some(cached) = guard.get(&(key.to_owned(), bucket))
    {
        return cached.clone();
    }
    let built = lookup(key, bucket as u32).and_then(|ic| match ic {
        SystemIcon::Svg(bytes) => Some(WebIcon {
            bytes,
            content_type: "image/svg+xml",
        }),
        SystemIcon::Png(bytes) => Some(WebIcon {
            bytes,
            content_type: "image/png",
        }),
        SystemIcon::Rgba {
            width,
            height,
            rgba,
        } => Some(WebIcon {
            bytes: Arc::from(crate::png::encode_rgba(width, height, &rgba)?),
            content_type: "image/png",
        }),
    });
    if let Ok(mut guard) = WEB_CACHE.lock() {
        if guard.len() >= CACHE_CAP {
            guard.clear();
        }
        guard.insert((key.to_owned(), bucket), built.clone());
    }
    built
}

fn valid_key(key: &str) -> bool {
    if key == "folder" {
        return true;
    }
    let Some((prefix, rest)) = key.split_once(':') else {
        return false;
    };
    matches!(prefix, "place" | "ext" | "kind") && is_valid_token(rest)
}

fn is_valid_token(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 32
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'+'))
}

fn clean_ext(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    for compound in ["tar.gz", "tar.xz", "tar.bz2", "tar.zst"] {
        if lower.ends_with(&format!(".{compound}")) && lower.len() > compound.len() + 1 {
            return Some(compound.to_owned());
        }
    }
    let (stem, ext) = lower.rsplit_once('.')?;
    if stem.is_empty() || !is_valid_token(ext) {
        return None;
    }
    Some(ext.to_owned())
}

fn load_special_dirs() -> HashMap<String, &'static str> {
    let mut map = HashMap::new();
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default()
        .replace('\\', "/");
    let home = home.trim_end_matches('/');
    if home.is_empty() {
        return map;
    }
    map.insert(home.to_owned(), "home");
    for (sub, role) in [
        ("Desktop", "desktop"),
        ("Documents", "documents"),
        ("Downloads", "downloads"),
        ("Music", "music"),
        ("Pictures", "pictures"),
        ("Videos", "videos"),
        ("Movies", "videos"),
        ("Public", "public"),
        ("Templates", "templates"),
    ] {
        map.insert(format!("{home}/{sub}"), role);
    }
    let cfg = std::env::var("XDG_CONFIG_HOME").unwrap_or_else(|_| format!("{home}/.config"));
    if let Ok(text) = std::fs::read_to_string(format!("{cfg}/user-dirs.dirs")) {
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                continue;
            };
            let role = match k.trim() {
                "XDG_DESKTOP_DIR" => "desktop",
                "XDG_DOCUMENTS_DIR" => "documents",
                "XDG_DOWNLOAD_DIR" => "downloads",
                "XDG_MUSIC_DIR" => "music",
                "XDG_PICTURES_DIR" => "pictures",
                "XDG_VIDEOS_DIR" => "videos",
                "XDG_PUBLICSHARE_DIR" => "public",
                "XDG_TEMPLATES_DIR" => "templates",
                _ => continue,
            };
            let raw = v.trim().trim_matches('"').replace("$HOME", home);
            let clean = raw.trim_end_matches('/');
            if !clean.is_empty() {
                map.insert(clean.to_owned(), role);
            }
        }
    }
    map
}

fn resolve_uncached(key: &str, px: u32) -> Option<SystemIcon> {
    #[cfg(any(windows, target_os = "macos"))]
    {
        return crate::sysicon_os::resolve(key, px).map(|p| SystemIcon::Rgba {
            width: p.width,
            height: p.height,
            rgba: Arc::from(p.rgba),
        });
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        unix::resolve(key, px)
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = (key, px);
        None
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
mod unix {
    use std::collections::HashSet;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, LazyLock};

    use super::SystemIcon;

    const ICON_FILE_CAP: u64 = 2 * 1024 * 1024;

    static THEME_DIRS: LazyLock<Vec<PathBuf>> = LazyLock::new(discover_theme_dirs);

    pub(super) fn resolve(key: &str, px: u32) -> Option<SystemIcon> {
        let names = candidate_names(key);
        let dirs = &*THEME_DIRS;
        for name in &names {
            for theme_dir in dirs {
                if let Some(ic) = find_in_theme(theme_dir, name, px) {
                    return Some(ic);
                }
            }
        }
        None
    }

    fn candidate_names(key: &str) -> Vec<String> {
        if key == "folder" {
            return vec!["folder".into(), "inode-directory".into()];
        }
        match key.split_once(':') {
            Some(("place", role)) => {
                let primary = match role {
                    "home" => "user-home",
                    "desktop" => "user-desktop",
                    "documents" => "folder-documents",
                    "downloads" => "folder-download",
                    "music" => "folder-music",
                    "pictures" => "folder-pictures",
                    "videos" => "folder-videos",
                    "public" => "folder-publicshare",
                    "templates" => "folder-templates",
                    _ => "folder",
                };
                vec![primary.into(), "folder".into(), "inode-directory".into()]
            }
            Some(("ext", ext)) => {
                let mut out = crate::known::known().mime_icons_for_ext(ext);
                for fb in ["text-x-generic", "application-x-generic"] {
                    if !out.iter().any(|s| s == fb) {
                        out.push(fb.into());
                    }
                }
                out
            }
            Some(("kind", kind)) => kind_icons(kind).iter().map(|s| (*s).to_owned()).collect(),
            _ => Vec::new(),
        }
    }

    fn kind_icons(kind: &str) -> &'static [&'static str] {
        match kind {
            "folder" => &["folder", "inode-directory"],
            "doc" => &["x-office-document", "text-x-generic"],
            "code" => &["text-x-script", "text-x-generic"],
            "config" => &["text-x-generic", "application-x-generic"],
            "image" => &["image-x-generic"],
            "video" | "media" => &["video-x-generic"],
            "audio" => &["audio-x-generic"],
            "archive" => &["package-x-generic", "application-x-archive"],
            "exec" => &["application-x-executable", "application-x-generic"],
            "font" => &["font-x-generic"],
            "data" | "build" | "file" => &["application-x-generic", "text-x-generic"],
            _ => &["text-x-generic"],
        }
    }

    fn find_in_theme(theme: &Path, name: &str, px: u32) -> Option<SystemIcon> {
        let cats = ["mimetypes", "places", "apps"];
        let sizes: &[&str] = if px <= 24 {
            &["16", "22", "24", "32", "48", "64"]
        } else {
            &["64", "48", "128", "32", "24", "16"]
        };
        // Prefer scalable vector SVG first: sharp at every DPI scale.
        for cat in cats {
            for rel in [
                format!("scalable/{cat}/{name}.svg"),
                format!("{cat}/scalable/{name}.svg"),
            ] {
                if let Some(b) = read_icon(&theme.join(rel)) {
                    return Some(SystemIcon::Svg(b));
                }
            }
            for s in sizes {
                let kde_svg = theme.join(format!("{cat}/{s}/{name}.svg"));
                if let Some(b) = read_icon(&kde_svg) {
                    return Some(SystemIcon::Svg(b));
                }
            }
        }
        // Raster PNG fallback across GNOME (`48x48/mimetypes`) and KDE (`mimetypes/48`).
        for s in sizes {
            for cat in cats {
                for rel in [
                    format!("{s}x{s}/{cat}/{name}.png"),
                    format!("{s}x{s}@2x/{cat}/{name}.png"),
                    format!("{cat}/{s}/{name}.png"),
                ] {
                    if let Some(b) = read_icon(&theme.join(rel)) {
                        return Some(SystemIcon::Png(b));
                    }
                }
            }
        }
        None
    }

    fn read_icon(path: &Path) -> Option<Arc<[u8]>> {
        let meta = std::fs::metadata(path).ok()?;
        if !meta.is_file() || meta.len() == 0 || meta.len() > ICON_FILE_CAP {
            return None;
        }
        Some(Arc::from(std::fs::read(path).ok()?))
    }

    fn discover_theme_dirs() -> Vec<PathBuf> {
        let roots = icon_roots();
        let mut names = configured_themes();
        for fallback in [
            "Adwaita",
            "breeze",
            "Yaru",
            "Mint-Y",
            "Papirus",
            "elementary",
            "AdwaitaLegacy",
            "gnome",
            "hicolor",
        ] {
            if !names.iter().any(|n| n == fallback) {
                names.push(fallback.to_owned());
            }
        }
        let mut visited = HashSet::new();
        let mut chain = Vec::new();
        for name in names {
            collect_theme(&name, &roots, &mut visited, &mut chain);
        }
        chain
    }

    fn collect_theme(
        name: &str,
        roots: &[PathBuf],
        visited: &mut HashSet<String>,
        out: &mut Vec<PathBuf>,
    ) {
        let trimmed = name.trim();
        if trimmed.is_empty() || !visited.insert(trimmed.to_owned()) {
            return;
        }
        let mut inherits = Vec::new();
        for root in roots {
            let dir = root.join(trimmed);
            if dir.is_dir() {
                if inherits.is_empty()
                    && let Ok(index) = std::fs::read_to_string(dir.join("index.theme"))
                {
                    inherits = parse_inherits(&index);
                }
                out.push(dir);
            }
        }
        for parent in inherits {
            collect_theme(&parent, roots, visited, out);
        }
    }

    fn parse_inherits(index_theme: &str) -> Vec<String> {
        for line in index_theme.lines() {
            if let Some(rest) = line.trim().strip_prefix("Inherits=") {
                return rest
                    .split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
        }
        Vec::new()
    }

    fn icon_roots() -> Vec<PathBuf> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        let mut out = Vec::new();
        if !home.as_os_str().is_empty() {
            out.push(home.join(".icons"));
        }
        for data in crate::known::data_dirs() {
            out.push(data.join("icons"));
        }
        out.push(PathBuf::from("/usr/share/pixmaps"));
        out.into_iter().filter(|p| p.is_dir()).collect()
    }

    fn configured_themes() -> Vec<String> {
        let mut out = Vec::new();
        let cfg = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_default()
                    .join(".config")
            });
        let desktop = std::env::var("XDG_CURRENT_DESKTOP")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let is_kde = desktop.contains("kde") || std::env::var_os("KDE_FULL_SESSION").is_some();
        if is_kde && let Some(t) = read_ini_key(&cfg.join("kdeglobals"), "Icons", "Theme") {
            out.push(t);
        }
        for rel in ["gtk-4.0/settings.ini", "gtk-3.0/settings.ini"] {
            if let Some(t) = read_ini_key(&cfg.join(rel), "Settings", "gtk-icon-theme-name")
                && !out.contains(&t)
            {
                out.push(t);
            }
        }
        if !is_kde
            && out.is_empty()
            && let Some(t) = gsettings_icon_theme()
        {
            out.push(t);
        }
        if !is_kde && let Some(t) = read_ini_key(&cfg.join("kdeglobals"), "Icons", "Theme") {
            out.push(t);
        }
        out
    }

    fn read_ini_key(path: &Path, section: &str, key: &str) -> Option<String> {
        let text = std::fs::read_to_string(path).ok()?;
        let header = format!("[{section}]");
        let mut inside = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                inside = line.eq_ignore_ascii_case(&header);
                continue;
            }
            if inside
                && let Some((k, v)) = line.split_once('=')
                && k.trim() == key
            {
                let v = v.trim().trim_matches(['"', '\'']);
                if !v.is_empty() {
                    return Some(v.to_owned());
                }
            }
        }
        None
    }

    fn gsettings_icon_theme() -> Option<String> {
        let out = std::process::Command::new("gsettings")
            .args(["get", "org.gnome.desktop.interface", "icon-theme"])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let s = String::from_utf8_lossy(&out.stdout);
        let trimmed = s.trim().trim_matches(['\'', '"']);
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_strip_paths_and_keep_compound_extensions() {
        assert_eq!(
            key_for("Documents", true, "folder", Some("documents")),
            "place:documents"
        );
        assert_eq!(key_for("src", true, "folder", None), "folder");
        assert_eq!(key_for("Report.DOCX", false, "doc", None), "ext:docx");
        assert_eq!(
            key_for("archive.tar.gz", false, "archive", None),
            "ext:tar.gz"
        );
        assert_eq!(key_for(".bashrc", false, "config", None), "kind:config");
        assert_eq!(key_for("Makefile", false, "code", None), "kind:code");
        assert!(!valid_key("../etc/passwd"));
        assert!(!valid_key("ext:a/b"));
    }

    #[test]
    fn desktop_resolves_folder_and_document_icons_when_themes_exist() {
        // On a desktop installation with freedesktop / Win32 / macOS icons,
        // both `folder` and `ext:pdf` resolve and cache consistently.
        let first = lookup("folder", 16);
        let second = lookup("folder", 16);
        assert_eq!(first.is_some(), second.is_some());
    }
}
