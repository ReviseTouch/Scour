//! What an entry is.
//!
//! One `stat` yields every field of [`Meta`] at once — size, three timestamps,
//! permission bits, owner, and the blocks actually allocated on disk. Adding a
//! column therefore costs nothing at scan time: it means reading one more field
//! of a syscall that was already made.

use serde::{Deserialize, Serialize};

/// Which source produced an entry. Assigned by configuration order, stable for
/// the lifetime of an index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceId(pub u32);

/// How a source names an entry so it can be found again.
///
/// This is deliberately not "a row id". Different sources can identify things
/// only in different ways, and pretending otherwise is how an indexer ends up
/// with filesystem assumptions baked into places that have no business holding
/// them:
///
/// * A POSIX filesystem has `(dev, ino)`, which survives a rename — so a moved
///   file can be *updated* rather than deleted and re-added.
/// * FAT and most network mounts have nothing stable; the path is the identity,
///   so a rename is genuinely a delete plus an add.
/// * An object store has neither: the key is the name and the version is an
///   opaque token.
///
/// A source declares which of these it provides through [`Caps::STABLE_IDS`],
/// and the engine adapts. Nothing above this type ever writes `if source_is_fs`.
///
/// [`Caps::STABLE_IDS`]: crate::types::Caps::STABLE_IDS
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Key {
    /// POSIX inode identity. Survives rename and move within a device.
    Inode { dev: u64, ino: u64 },
    /// A hash of the path. Identity dies with the name.
    PathHash(u64),
    /// Anything else the source can produce (an etag, a cloud file id).
    Opaque(Box<[u8]>),
}

/// A source-qualified entry identity.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct EntryId {
    pub source: SourceId,
    pub key: Key,
}

impl EntryId {
    pub fn inode(source: SourceId, dev: u64, ino: u64) -> Self {
        Self {
            source,
            key: Key::Inode { dev, ino },
        }
    }

    /// Identity derived from the path, for sources that offer nothing better.
    ///
    /// FxHash's finaliser, inlined rather than depended on — this crate takes
    /// no dependencies, and the hash only has to be stable and well spread
    /// within one index, not cryptographic.
    pub fn path_hash(source: SourceId, path: &str) -> Self {
        const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;
        let mut h: u64 = 0;
        for chunk in path.as_bytes().chunks(8) {
            let mut buf = [0u8; 8];
            buf[..chunk.len()].copy_from_slice(chunk);
            h = (h.rotate_left(5) ^ u64::from_le_bytes(buf)).wrapping_mul(SEED);
        }
        Self {
            source,
            key: Key::PathHash(h),
        }
    }
}

/// Everything measurable about an entry. All of it comes from one `stat`,
/// except `items`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub size: i64,
    /// Modification time, unix epoch seconds.
    pub mtime: i64,
    /// Status-change time on unix; creation time on Windows. The two are not
    /// the same thing, and the query language names it `dc:` on both, which is
    /// a compromise the user interface has to explain rather than hide.
    pub ctime: i64,
    /// Last access time. Most Linux systems mount with `relatime`, so this may
    /// lag by up to a day.
    pub atime: i64,
    /// `st_mode`: file type plus permission bits. Zero where unavailable.
    pub mode: i64,
    pub uid: i64,
    pub gid: i64,
    /// Space *allocated* on disk. Smaller than `size` for sparse files, larger
    /// for tiny ones because of block rounding.
    pub disk: i64,
    /// Entries directly inside a directory; `-1` for files and when unknown.
    pub items: i64,
}

impl Meta {
    /// Nothing known: no stat was made, or it failed.
    pub const UNKNOWN: Self = Self {
        size: 0,
        mtime: 0,
        ctime: 0,
        atime: 0,
        mode: 0,
        uid: 0,
        gid: 0,
        disk: 0,
        items: -1,
    };

    /// Convert from `std::fs::Metadata`. `size` is meaningless for directories,
    /// so it is forced to zero.
    pub fn from_std(md: &std::fs::Metadata, is_dir: bool) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Self {
                size: if is_dir { 0 } else { md.size() as i64 },
                mtime: md.mtime(),
                ctime: md.ctime(),
                atime: md.atime(),
                mode: md.mode() as i64,
                uid: md.uid() as i64,
                gid: md.gid() as i64,
                disk: md.blocks() as i64 * 512,
                items: -1,
            }
        }
        #[cfg(not(unix))]
        {
            let secs = |t: std::io::Result<std::time::SystemTime>| -> i64 {
                t.ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0)
            };
            Self {
                size: if is_dir { 0 } else { md.len() as i64 },
                mtime: secs(md.modified()),
                ctime: secs(md.created()),
                atime: secs(md.accessed()),
                mode: 0,
                uid: 0,
                gid: 0,
                // Not the allocated size: getting that on Windows needs
                // GetCompressedFileSizeW, which belongs in the source, not here.
                disk: if is_dir { 0 } else { md.len() as i64 },
                items: -1,
            }
        }
    }
}

/// One entry as a source produces it.
///
/// `path` is always `/`-separated, including on Windows, so that everything
/// above this — ancestor tokens, the query language, the wire protocol — has a
/// single separator to reason about. Converting back is the source's job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: EntryId,
    pub path: String,
    pub is_dir: bool,
    pub meta: Meta,
}

impl Entry {
    /// The final path component.
    pub fn name(&self) -> &str {
        match self.path.rfind('/') {
            Some(i) => &self.path[i + 1..],
            None => &self.path,
        }
    }

    /// The parent directory, without a trailing slash. Empty at the root.
    pub fn parent(&self) -> &str {
        match self.path.rfind('/') {
            Some(0) => "/",
            Some(i) => &self.path[..i],
            None => "",
        }
    }

    /// Lowercase extension without the dot.
    pub fn ext(&self) -> String {
        ext_of(self.name())
    }

    pub fn kind(&self) -> Kind {
        kind_of(self.is_dir, &self.ext(), self.meta.mode)
    }
}

/// The coarse category a row is drawn and filtered by.
///
/// The numeric values are part of the on-disk index format: changing one
/// silently reinterprets every existing index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    File = 0,
    Dir = 1,
    Code = 2,
    Image = 3,
    Archive = 4,
    Doc = 5,
    Exec = 6,
    Media = 7,
}

impl Kind {
    pub const ALL: [Kind; 8] = [
        Kind::File,
        Kind::Dir,
        Kind::Code,
        Kind::Image,
        Kind::Archive,
        Kind::Doc,
        Kind::Exec,
        Kind::Media,
    ];

    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(v: u8) -> Option<Kind> {
        Kind::ALL.get(v as usize).copied()
    }

    /// The English message id for this kind. Not a display string: the
    /// frontend passes it through a [`Catalog`] to get the user's language.
    ///
    /// [`Catalog`]: crate::text::Catalog
    pub fn msgid(self) -> &'static str {
        match self {
            Kind::File => "File",
            Kind::Dir => "Folder",
            Kind::Code => "Code",
            Kind::Image => "Image",
            Kind::Archive => "Archive",
            Kind::Doc => "Document",
            Kind::Exec => "Executable",
            Kind::Media => "Media",
        }
    }

    /// Parse the value of a `kind:` query term.
    ///
    /// Turkish spellings are accepted alongside English on purpose: this tool
    /// is used in Turkish, and `kind:klasör` should work next to `kind:folder`.
    /// The input is expected to be already case-folded.
    pub fn from_name(folded: &str) -> Option<Kind> {
        Some(match folded {
            "dir" | "folder" | "klasor" | "klasör" => Kind::Dir,
            "code" | "kod" => Kind::Code,
            "image" | "img" | "pic" | "gorsel" | "görsel" => Kind::Image,
            "archive" | "arsiv" | "arşiv" | "zip" => Kind::Archive,
            "doc" | "document" | "belge" => Kind::Doc,
            "exec" | "exe" | "bin" | "calistirilabilir" | "çalıştırılabilir" => Kind::Exec,
            "media" | "video" | "audio" | "medya" | "ses" => Kind::Media,
            "file" | "dosya" => Kind::File,
            _ => return None,
        })
    }
}

const CODE: &[&str] = &[
    "rs", "toml", "slint", "py", "js", "mjs", "cjs", "ts", "tsx", "jsx", "c", "h", "cpp", "hpp",
    "cc", "go", "java", "kt", "rb", "php", "sh", "bash", "zsh", "fish", "lua", "sql", "json",
    "yaml", "yml", "xml", "html", "htm", "css", "scss", "vue", "svelte", "dart", "swift", "cs",
    "ini", "cfg", "conf", "make", "cmake", "gradle", "nix", "zig", "hs", "ml", "el", "vim",
];
const IMAGE: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "ico", "tiff", "tif", "avif", "heic", "psd",
    "xcf", "afphoto", "afdesign", "raw", "cr2", "nef", "dng",
];
const ARCHIVE: &[&str] = &[
    "zip", "tar", "gz", "xz", "bz2", "zst", "7z", "rar", "deb", "rpm", "appimage", "iso", "pkg",
    "jar", "cab", "lz4", "tgz",
];
const DOC: &[&str] = &[
    "pdf", "doc", "docx", "odt", "xls", "xlsx", "ods", "ppt", "pptx", "odp", "txt", "md", "rtf",
    "epub", "csv", "tex", "djvu", "mobi",
];
const MEDIA: &[&str] = &[
    "mp4", "mkv", "avi", "mov", "webm", "wmv", "flv", "m4v", "mpg", "mpeg", "mp3", "flac", "wav",
    "ogg", "opus", "m4a", "aac", "wma", "aiff", "mid",
];
/// Executable by extension: Windows, and unix when the permission bit is
/// unreadable.
const EXEC_EXT: &[&str] = &["exe", "bat", "cmd", "com", "ps1", "msi", "appimage", "run"];

/// Extension of a file name: case-folded, without the dot.
///
/// A name that *starts* with a dot (`.bashrc`) has no extension. Folding rather
/// than lowercasing matters because the `ext:` query term is folded too, so the
/// Turkish `İ`/`I`/`ı`/`i` distinction has to disappear identically on both
/// sides or `ext:JPG` misses `photo.jpg` in a Turkish locale.
pub fn ext_of(name: &str) -> String {
    crate::text::DefaultFolder::of(ext_str(name))
}

/// The same extension, as it is spelled and without allocating.
///
/// Separate because an index tests `ext:` once a row and folding into a fresh
/// `String` a million times a query is the kind of cost that does not show up
/// anywhere except the total. Callers that can fold into a buffer of their own
/// use this and stay identical to [`ext_of`] by construction — the rule for
/// what counts as an extension lives here and only here.
pub fn ext_str(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() && ext.len() <= 12 => ext,
        _ => "",
    }
}

/// Classify an entry. `mode` of zero means "unknown", in which case
/// executability falls back to the extension.
pub fn kind_of(is_dir: bool, ext: &str, mode: i64) -> Kind {
    if is_dir {
        return Kind::Dir;
    }
    if CODE.contains(&ext) {
        return Kind::Code;
    }
    if IMAGE.contains(&ext) {
        return Kind::Image;
    }
    if ARCHIVE.contains(&ext) {
        return Kind::Archive;
    }
    if DOC.contains(&ext) {
        return Kind::Doc;
    }
    if MEDIA.contains(&ext) {
        return Kind::Media;
    }
    if mode & 0o111 != 0 || (mode == 0 && EXEC_EXT.contains(&ext)) {
        return Kind::Exec;
    }
    Kind::File
}

/// Permission bits in `drwxr-xr-x` form. Empty when `mode` is zero.
pub fn mode_string(mode: i64) -> String {
    if mode == 0 {
        return String::new();
    }
    let m = mode as u32;
    let type_ch = match m & 0o170000 {
        0o040000 => b'd',
        0o120000 => b'l',
        0o060000 => b'b',
        0o020000 => b'c',
        0o010000 => b'p',
        0o140000 => b's',
        _ => b'-',
    };
    let mut b = [b'-'; 10];
    b[0] = type_ch;
    for (i, shift) in [6, 3, 0].into_iter().enumerate() {
        let bits = (m >> shift) & 0o7;
        let base = 1 + i * 3;
        if bits & 0o4 != 0 {
            b[base] = b'r';
        }
        if bits & 0o2 != 0 {
            b[base + 1] = b'w';
        }
        if bits & 0o1 != 0 {
            b[base + 2] = b'x';
        }
    }
    // setuid, setgid and sticky turn the matching `x` into s/S or t/T.
    for (bit, idx, set, unset) in [
        (0o4000u32, 3usize, b's', b'S'),
        (0o2000, 6, b's', b'S'),
        (0o1000, 9, b't', b'T'),
    ] {
        if m & bit != 0 {
            b[idx] = if b[idx] == b'x' { set } else { unset };
        }
    }
    String::from_utf8_lossy(&b).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, is_dir: bool, mode: i64) -> Entry {
        Entry {
            id: EntryId::path_hash(SourceId(0), path),
            path: path.into(),
            is_dir,
            meta: Meta {
                mode,
                ..Meta::UNKNOWN
            },
        }
    }

    #[test]
    fn path_components() {
        let e = entry("/home/u/Projeler/main.rs", false, 0o100644);
        assert_eq!(e.name(), "main.rs");
        assert_eq!(e.parent(), "/home/u/Projeler");
        assert_eq!(e.ext(), "rs");
        assert_eq!(e.kind(), Kind::Code);
        assert_eq!(entry("/etc", true, 0).parent(), "/");
        assert_eq!(entry("relative", false, 0).parent(), "");
    }

    #[test]
    fn extension_rules() {
        assert_eq!(ext_of("main.RS"), "rs");
        assert_eq!(ext_of("arşiv.TAR.GZ"), "gz");
        assert_eq!(
            ext_of(".bashrc"),
            "",
            "a name starting with a dot has no extension"
        );
        assert_eq!(ext_of("LICENSE"), "");
        assert_eq!(ext_of("file."), "");
    }

    #[test]
    fn kinds() {
        assert_eq!(kind_of(true, "", 0o40755), Kind::Dir);
        assert_eq!(kind_of(false, "png", 0o100644), Kind::Image);
        // The permission bit is how an extensionless binary is recognised.
        assert_eq!(kind_of(false, "", 0o100755), Kind::Exec);
        assert_eq!(kind_of(false, "", 0o100644), Kind::File);
        // With an unknown mode the extension decides instead.
        assert_eq!(kind_of(false, "exe", 0), Kind::Exec);
        assert_eq!(Kind::from_name("klasör"), Some(Kind::Dir));
        assert_eq!(Kind::from_name("folder"), Some(Kind::Dir));
        assert_eq!(Kind::from_name("nope"), None);
        assert_eq!(Kind::from_u8(Kind::Media.as_u8()), Some(Kind::Media));
    }

    #[test]
    fn mode_strings() {
        assert_eq!(mode_string(0o100644), "-rw-r--r--");
        assert_eq!(mode_string(0o40755), "drwxr-xr-x");
        assert_eq!(mode_string(0o120777), "lrwxrwxrwx");
        assert_eq!(mode_string(0o104755), "-rwsr-xr-x");
        assert_eq!(mode_string(0), "");
    }

    #[test]
    fn path_hash_is_stable_and_spread() {
        let a = EntryId::path_hash(SourceId(1), "/home/u/a.txt");
        assert_eq!(a, EntryId::path_hash(SourceId(1), "/home/u/a.txt"));
        assert_ne!(a, EntryId::path_hash(SourceId(1), "/home/u/b.txt"));
        assert_ne!(a, EntryId::path_hash(SourceId(2), "/home/u/a.txt"));
    }
}
