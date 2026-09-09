//! What an entry is. One `stat` yields every field of [`Meta`] at once, so adding a
//! column costs nothing at scan time: it reads one more field of a syscall already
//! made.

use serde::{Deserialize, Serialize};

/// Which source produced an entry. Assigned by configuration order, stable for
/// the lifetime of an index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceId(pub u32);

/// How a source names an entry so it can be found again: `(dev, ino)` on POSIX, the
/// path where nothing else is stable, a version token for an object store. The
/// filesystem source uses `PathHash` — a row is a *name*, and its identity is its path.
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

    /// Identity derived from the path, which is what a filesystem entry has.
    pub fn path_hash(source: SourceId, path: &str) -> Self {
        Self {
            source,
            key: Key::PathHash(path_digest(source, path)),
        }
    }
}

/// A stable 64-bit digest of a source and a path: FxHash's finaliser inlined, since
/// this crate takes no dependencies. Eight bytes at a time, not one. It reaches the
/// index file, so changing it invalidates every index on disk.
pub fn path_digest(source: SourceId, path: &str) -> u64 {
    const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;
    let mut h: u64 = u64::from(source.0);
    for chunk in path.as_bytes().chunks(8) {
        let mut buf = [0u8; 8];
        buf[..chunk.len()].copy_from_slice(chunk);
        h = (h.rotate_left(5) ^ u64::from_le_bytes(buf)).wrapping_mul(SEED);
    }
    h
}

/// Everything measurable about an entry. All of it comes from one `stat`,
/// except `items`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Meta {
    pub size: i64,
    /// Modification time, unix epoch seconds.
    pub mtime: i64,
    /// Birth time where the filesystem keeps one and `st_ctime` where it does not;
    /// creation time on Windows. The query language names it `dc:` on both.
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
    /// Entries directly inside a directory; `-1` for files and when unknown. Nothing
    /// writes anything else yet: one `stat` does not count children, and the walk
    /// builds a directory's row before it has seen them.
    pub items: i64,
    /// Names this file has — `st_nlink`, one for almost everything. A file with four
    /// names is four rows, so each carries `disk / links` and a sum over a tree is the
    /// space it occupies; that agrees with `du` on the total, not on which name pays.
    pub links: i64,
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
        links: 1,
    };

    /// When the file came into being, if anything recorded it.
    #[cfg(unix)]
    fn birth(md: &std::fs::Metadata) -> Option<i64> {
        md.created()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs() as i64)
    }

    /// Convert from `std::fs::Metadata`. `size` is forced to zero for a directory.
    pub fn from_std(md: &std::fs::Metadata, is_dir: bool) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Self {
                size: if is_dir { 0 } else { md.size() as i64 },
                mtime: md.mtime(),
                // Birth time where the filesystem keeps one, `st_ctime` only where it
                // does not: `st_ctime` equalled `mtime` on 95% of a sample here, which
                // is what made sorting by creation look dead.
                ctime: Self::birth(md).unwrap_or_else(|| md.ctime()),
                atime: md.atime(),
                mode: md.mode() as i64,
                uid: md.uid() as i64,
                gid: md.gid() as i64,
                disk: md.blocks() as i64 * 512,
                items: -1,
                links: md.nlink().max(1) as i64,
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
                // Not the allocated size: that needs GetCompressedFileSizeW, which
                // belongs in the source rather than here.
                disk: if is_dir { 0 } else { md.len() as i64 },
                items: -1,
                links: 1,
            }
        }
    }
}

/// One entry as a source produces it. `path` is always `/`-separated, on Windows too,
/// so everything above has one separator to reason about; converting back is the
/// source's job.
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

    /// Lowercase extension without the dot. A directory has none — [`ext_of`] answers
    /// only the name question, and `Trabzon 2. Grup` is a folder, not a file of type
    /// ` grup`. A dot in a folder name is ordinary on a volume written from Windows.
    pub fn ext(&self) -> String {
        if self.is_dir {
            return String::new();
        }
        ext_of(self.name())
    }

    pub fn kind(&self) -> Kind {
        kind_of(self.is_dir, self.name(), self.meta.mode)
    }
}

/// The coarse category a row is drawn and filtered by. The numeric values are part of
/// the on-disk index format: never renumber a variant, never reuse a discriminant, and
/// bump `FORMAT` in `scour-index-native` when a number's meaning changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[repr(u8)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Nothing matched. The honest unknown, and about one entry in sixteen.
    File = 0,
    Dir = 1,
    /// Source a person wrote.
    Code = 2,
    Image = 3,
    /// A container of other files: archives, OS packages, disk and VM images.
    Archive = 4,
    /// Something a person reads.
    Doc = 5,
    /// Machine code the OS can load and run.
    Exec = 6,
    /// **Retired**, kept so a `media` search still reads an index written before
    /// [`Kind::Audio`] and [`Kind::Video`]. Never produced by [`kind_of`].
    Media = 7,
    Audio = 8,
    Video = 9,
    /// Machine-produced and tool-regenerable: deleting it costs time, not information.
    /// Half of every file on a developer's machine.
    Build = 10,
    /// Structured machine-readable content: serialisation, tabular data,
    /// schemas, databases, model weights, logs.
    Data = 11,
    /// Settings that change how a program behaves, plus certificates and keys.
    Config = 12,
    Font = 13,
}

impl Kind {
    /// Every kind, indexed by discriminant. [`Kind::Media`] is in here because
    /// an old index can still hold a 7; it is not in [`Kind::OFFERED`].
    pub const ALL: [Kind; 14] = [
        Kind::File,
        Kind::Dir,
        Kind::Code,
        Kind::Image,
        Kind::Archive,
        Kind::Doc,
        Kind::Exec,
        Kind::Media,
        Kind::Audio,
        Kind::Video,
        Kind::Build,
        Kind::Data,
        Kind::Config,
        Kind::Font,
    ];

    /// The kinds a frontend offers, in the order a list should show them:
    /// what a person looks for first, then what a machine made.
    pub const OFFERED: [Kind; 13] = [
        Kind::Dir,
        Kind::Code,
        Kind::Doc,
        Kind::Image,
        Kind::Data,
        Kind::Config,
        Kind::Archive,
        Kind::Exec,
        Kind::Audio,
        Kind::Video,
        Kind::Font,
        Kind::Build,
        Kind::File,
    ];

    pub fn as_u8(self) -> u8 {
        self as u8
    }

    pub fn from_u8(v: u8) -> Option<Kind> {
        Kind::ALL.get(v as usize).copied()
    }

    /// The English message id for this kind, not a display string: a frontend passes
    /// it through a [`Catalog`](crate::text::Catalog) to get the user's language.
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
            Kind::Audio => "Audio",
            Kind::Video => "Video",
            Kind::Build => "Build output",
            Kind::Data => "Data",
            Kind::Config => "Configuration",
            Kind::Font => "Font",
        }
    }

    /// How this kind is spelled in a `kind:` term. Never [`Kind::msgid`]: a label may
    /// be two words and be translated, a token may be neither.
    pub fn token(self) -> &'static str {
        match self {
            Kind::File => "file",
            Kind::Dir => "folder",
            Kind::Code => "code",
            Kind::Image => "image",
            Kind::Archive => "archive",
            Kind::Doc => "doc",
            Kind::Exec => "exec",
            Kind::Media => "media",
            Kind::Audio => "audio",
            Kind::Video => "video",
            Kind::Build => "build",
            Kind::Data => "data",
            Kind::Config => "config",
            Kind::Font => "font",
        }
    }

    /// Parse the value of a `kind:` query term into the kinds it stands for. A set,
    /// because `media` means audio, video or the retired 7, and `text` is a group alias
    /// with no discriminant of its own. The input is expected already case-folded.
    pub fn from_name(folded: &str) -> Option<&'static [Kind]> {
        Some(match folded {
            "dir" | "folder" | "klasor" | "klasör" => &[Kind::Dir],
            "code" | "source" | "kod" | "kaynak" => &[Kind::Code],
            "image" | "img" | "pic" | "gorsel" | "görsel" | "resim" => &[Kind::Image],
            "archive" | "arsiv" | "arşiv" | "zip" => &[Kind::Archive],
            "doc" | "document" | "belge" => &[Kind::Doc],
            // `bin` is here because it was accepted before the table split and
            // removing a spelling breaks a query somebody has written down.
            "exec" | "exe" | "bin" | "executable" | "calistirilabilir" | "çalıştırılabilir" => {
                &[Kind::Exec]
            }
            "audio" | "ses" | "muzik" | "müzik" => &[Kind::Audio],
            "video" | "film" => &[Kind::Video],
            "media" | "medya" => &[Kind::Audio, Kind::Video, Kind::Media],
            "build" | "derleme" | "cikti" | "çıktı" => &[Kind::Build],
            "data" | "veri" => &[Kind::Data],
            "config" | "conf" | "settings" | "ayar" | "yapilandirma" | "yapılandırma" => {
                &[Kind::Config]
            }
            "font" | "yazitipi" | "yazıtipi" => &[Kind::Font],
            "file" | "dosya" => &[Kind::File],
            // Everything a person reads or writes as text, in one word.
            "text" | "metin" => &[Kind::Code, Kind::Doc, Kind::Data, Kind::Config],
            _ => return None,
        })
    }
}

/// Every extension that means one thing, and what it means. Sorted and binary
/// searched, which `the_tables_are_sorted_and_hold_each_extension_once` enforces. An
/// extension belongs here only when it means one thing; `.obj` is left out.
const EXT_TABLE: &[(&str, Kind)] = &[
    ("1", Kind::Doc), // man page
    ("3gp", Kind::Video),
    ("7z", Kind::Archive),
    ("a", Kind::Build),
    ("aac", Kind::Audio),
    ("aar", Kind::Archive),
    ("adoc", Kind::Doc),
    ("afdesign", Kind::Image),
    ("afphoto", Kind::Image),
    ("aif", Kind::Audio),
    ("aiff", Kind::Audio),
    ("apk", Kind::Archive),
    ("appimage", Kind::Exec),
    ("arb", Kind::Data), // Flutter message catalogue
    ("asciidoc", Kind::Doc),
    ("asm", Kind::Code),
    ("ass", Kind::Doc), // subtitles
    ("avi", Kind::Video),
    ("avif", Kind::Image),
    ("avro", Kind::Data),
    ("awk", Kind::Code),
    ("azw3", Kind::Doc),
    ("bash", Kind::Code),
    ("bat", Kind::Exec),
    ("bc", Kind::Build), // LLVM bitcode
    ("bmp", Kind::Image),
    ("bz2", Kind::Archive),
    ("c", Kind::Code),
    ("cab", Kind::Archive),
    ("cc", Kind::Code),
    ("cer", Kind::Config),
    ("cfg", Kind::Config),
    ("cjs", Kind::Code),
    ("class", Kind::Build),
    ("clj", Kind::Code),
    ("cljs", Kind::Code),
    ("cmake", Kind::Code),
    ("cmd", Kind::Exec),
    ("com", Kind::Exec),
    ("comp", Kind::Code), // compute shader
    ("conf", Kind::Config),
    ("cpp", Kind::Code),
    ("cr", Kind::Code),
    ("cr2", Kind::Image),
    ("crate", Kind::Archive),
    ("crt", Kind::Config),
    ("cs", Kind::Code),
    ("css", Kind::Code),
    ("csv", Kind::Data),
    ("cxx", Kind::Code),
    ("d", Kind::Build), // make dependency list, not the D language
    ("dart", Kind::Code),
    ("dat", Kind::Data),
    ("db", Kind::Data),
    ("db3", Kind::Data),
    ("deb", Kind::Archive),
    ("der", Kind::Config),
    ("dex", Kind::Build),
    ("digest", Kind::Build),
    ("dill", Kind::Build), // Dart kernel
    ("djvu", Kind::Doc),
    ("dll", Kind::Exec),
    ("dmg", Kind::Archive),
    ("dng", Kind::Image),
    ("doc", Kind::Doc),
    ("docx", Kind::Doc),
    ("dtd", Kind::Data),
    ("dwo", Kind::Build), // split DWARF
    ("dylib", Kind::Exec),
    ("el", Kind::Code),
    ("eml", Kind::Doc),
    ("env", Kind::Config),
    ("eot", Kind::Font),
    ("epub", Kind::Doc),
    ("erl", Kind::Code),
    ("ex", Kind::Code),
    ("exe", Kind::Exec),
    ("exs", Kind::Code),
    ("fish", Kind::Code),
    ("flac", Kind::Audio),
    ("flv", Kind::Video),
    ("fon", Kind::Font),
    ("frag", Kind::Code), // fragment shader
    ("fs", Kind::Code),
    ("gcda", Kind::Build),
    ("gch", Kind::Build),
    ("gcno", Kind::Build),
    ("gem", Kind::Archive),
    ("gguf", Kind::Data),
    ("gif", Kind::Image),
    ("glsl", Kind::Code),
    ("gn", Kind::Code),
    ("gni", Kind::Code),
    ("go", Kind::Code),
    ("gradle", Kind::Code),
    ("groovy", Kind::Code),
    ("gz", Kind::Archive),
    ("h", Kind::Code),
    ("h5", Kind::Data),
    ("hcl", Kind::Code),
    ("heic", Kind::Image),
    ("hlsl", Kind::Code),
    ("hpp", Kind::Code),
    ("hrl", Kind::Code),
    ("hs", Kind::Code),
    ("htm", Kind::Doc),
    ("html", Kind::Doc),
    ("hxx", Kind::Code),
    ("ico", Kind::Image),
    ("idl", Kind::Code),
    ("ilk", Kind::Build),
    ("img", Kind::Archive), // disk image
    ("ini", Kind::Config),
    ("ipynb", Kind::Code),
    ("iso", Kind::Archive),
    ("jar", Kind::Archive),
    ("java", Kind::Code),
    ("jks", Kind::Config),
    ("jl", Kind::Code),
    ("jpeg", Kind::Image),
    ("jpg", Kind::Image),
    ("js", Kind::Code),
    ("json", Kind::Data),
    ("jsonl", Kind::Data),
    ("jsx", Kind::Code),
    ("keystore", Kind::Config),
    ("kt", Kind::Code),
    ("kts", Kind::Code),
    ("less", Kind::Code),
    ("lib", Kind::Build),
    ("lock", Kind::Data),
    ("log", Kind::Data),
    ("lua", Kind::Code),
    ("lz4", Kind::Archive),
    ("m", Kind::Code), // Objective-C, and MATLAB; both are source
    ("m3u", Kind::Audio),
    ("m3u8", Kind::Audio),
    ("m4a", Kind::Audio),
    ("m4v", Kind::Video),
    ("make", Kind::Code),
    ("map", Kind::Build), // source map
    ("mbox", Kind::Doc),
    ("md", Kind::Doc),
    ("metal", Kind::Code),
    ("mid", Kind::Audio),
    ("midi", Kind::Audio),
    ("mjs", Kind::Code),
    ("mk", Kind::Code),
    ("mkv", Kind::Video),
    ("ml", Kind::Code),
    ("mli", Kind::Code),
    ("mm", Kind::Code),
    ("mo", Kind::Build), // compiled gettext catalogue
    ("mobi", Kind::Doc),
    ("mov", Kind::Video),
    ("mp3", Kind::Audio),
    ("mp4", Kind::Video),
    ("mpeg", Kind::Video),
    ("mpg", Kind::Video),
    ("msg", Kind::Doc),
    ("msi", Kind::Exec),
    ("mts", Kind::Code), // TypeScript module, not MPEG transport stream
    ("nef", Kind::Image),
    ("nim", Kind::Code),
    ("nix", Kind::Code),
    ("npy", Kind::Data),
    ("npz", Kind::Data),
    ("o", Kind::Build),
    ("odp", Kind::Doc),
    ("ods", Kind::Doc),
    ("odt", Kind::Doc),
    ("ogg", Kind::Audio),
    ("ogv", Kind::Video),
    ("onnx", Kind::Data),
    ("opus", Kind::Audio),
    ("org", Kind::Doc),
    ("otf", Kind::Font),
    ("pack", Kind::Data), // git packfile
    ("parquet", Kind::Data),
    ("pbxproj", Kind::Config),
    ("pcf", Kind::Font),
    ("pch", Kind::Build),
    ("pdb", Kind::Build), // debug symbols
    ("pdf", Kind::Doc),
    ("pem", Kind::Config),
    ("pfx", Kind::Config),
    ("php", Kind::Code),
    ("pickle", Kind::Data),
    ("pkg", Kind::Archive),
    ("pkl", Kind::Data),
    ("pl", Kind::Code),
    ("plist", Kind::Config),
    ("pls", Kind::Audio),
    ("pm", Kind::Code),
    ("png", Kind::Image),
    ("po", Kind::Data), // message catalogue source
    ("pom", Kind::Config),
    ("pot", Kind::Data),
    ("ppt", Kind::Doc),
    ("pptx", Kind::Doc),
    ("properties", Kind::Config),
    ("proto", Kind::Code),
    ("ps1", Kind::Code),
    ("psd", Kind::Image),
    ("pt", Kind::Data), // model weights
    ("pth", Kind::Data),
    ("py", Kind::Code),
    ("pyc", Kind::Build),
    ("pyi", Kind::Code),
    ("pyx", Kind::Code),
    ("qcow2", Kind::Archive),
    ("qm", Kind::Build), // compiled Qt catalogue
    ("qml", Kind::Code),
    ("r", Kind::Code),
    ("rar", Kind::Archive),
    ("raw", Kind::Image),
    ("rb", Kind::Code),
    ("resx", Kind::Data),
    ("rlib", Kind::Build),
    ("rmeta", Kind::Build),
    ("rpm", Kind::Archive),
    ("rs", Kind::Code),
    ("rst", Kind::Doc),
    ("rtf", Kind::Doc),
    ("run", Kind::Exec),
    ("s", Kind::Code), // assembly
    ("safetensors", Kind::Data),
    ("sass", Kind::Code),
    ("scala", Kind::Code),
    ("scss", Kind::Code),
    ("sh", Kind::Code),
    ("slint", Kind::Code),
    ("so", Kind::Exec),
    ("sol", Kind::Code),
    ("sql", Kind::Code),
    ("sqlite", Kind::Data),
    ("sqlite3", Kind::Data),
    ("squashfs", Kind::Archive),
    ("srt", Kind::Doc),
    ("storyboard", Kind::Code),
    ("sub", Kind::Doc),
    ("svelte", Kind::Code),
    ("svg", Kind::Image),
    ("swift", Kind::Code),
    ("tar", Kind::Archive),
    ("tex", Kind::Doc),
    ("tf", Kind::Code),
    ("tgz", Kind::Archive),
    ("thrift", Kind::Code),
    ("tif", Kind::Image),
    ("tiff", Kind::Image),
    ("timestamp", Kind::Build),
    ("toml", Kind::Config),
    ("ts", Kind::Code), // TypeScript, not MPEG transport stream
    ("tsv", Kind::Data),
    ("tsx", Kind::Code),
    ("ttc", Kind::Font),
    ("ttf", Kind::Font),
    ("txt", Kind::Doc),
    ("ui", Kind::Code), // Qt Designer / GTK Builder
    ("vb", Kind::Code),
    ("vdi", Kind::Archive),
    ("vert", Kind::Code),
    ("vhd", Kind::Archive),
    ("vhdx", Kind::Archive),
    ("vim", Kind::Code),
    ("vmdk", Kind::Archive),
    ("vob", Kind::Video),
    ("vtt", Kind::Doc),
    ("vue", Kind::Code),
    ("wasm", Kind::Exec),
    ("wat", Kind::Code),
    ("wav", Kind::Audio),
    ("webm", Kind::Video),
    ("webp", Kind::Image),
    ("wgsl", Kind::Code),
    ("whl", Kind::Archive),
    ("wit", Kind::Code),
    ("wma", Kind::Audio),
    ("wmv", Kind::Video),
    ("woff", Kind::Font),
    ("woff2", Kind::Font),
    ("xcconfig", Kind::Config),
    ("xcf", Kind::Image),
    ("xhtml", Kind::Doc),
    ("xliff", Kind::Data),
    ("xls", Kind::Doc),
    ("xlsx", Kind::Doc),
    ("xml", Kind::Data),
    ("xsd", Kind::Data),
    ("xz", Kind::Archive),
    ("yaml", Kind::Config),
    ("yml", Kind::Config),
    ("zig", Kind::Code),
    ("zip", Kind::Archive),
    ("zsh", Kind::Code),
    ("zst", Kind::Archive),
];

/// `.bin` is the documented exception to the rule above: it means three things, and
/// is [`Kind::Build`] because 65,569 on the measured corpus were all tool caches.
const BIN_IS_BUILD: (&str, Kind) = ("bin", Kind::Build);

/// Names with no extension at all, exactly as spelled. Sorted, like [`EXT_TABLE`].
const NAME_TABLE: &[(&str, Kind)] = &[
    (".bash_profile", Kind::Config),
    (".bashrc", Kind::Config),
    (".cargo-ok", Kind::Build),
    (".dockerignore", Kind::Config),
    (".editorconfig", Kind::Config),
    (".env", Kind::Config),
    (".gitattributes", Kind::Config),
    (".gitconfig", Kind::Config),
    (".gitignore", Kind::Config),
    (".gitmodules", Kind::Config),
    (".npmignore", Kind::Config),
    (".npmrc", Kind::Config),
    (".profile", Kind::Config),
    (".vimrc", Kind::Config),
    (".zshrc", Kind::Config),
    ("authors", Kind::Doc),
    ("brewfile", Kind::Code),
    ("contributors", Kind::Doc),
    ("gemfile", Kind::Code),
    ("install", Kind::Doc),
    ("justfile", Kind::Code),
    ("news", Kind::Doc),
    ("notice", Kind::Doc),
    ("pkgbuild", Kind::Code),
    ("procfile", Kind::Code),
    ("rakefile", Kind::Code),
    ("todo", Kind::Doc),
    ("vagrantfile", Kind::Code),
];

/// Names with no extension that only *begin* a known word: `license-mit`,
/// `readme.old`. Checked after the exact table; the first match in this order wins.
const NAME_PREFIX: &[(&str, Kind)] = &[
    ("changelog", Kind::Doc),
    ("changes", Kind::Doc),
    ("copying", Kind::Doc),
    ("dockerfile", Kind::Code),
    ("licence", Kind::Doc),
    ("license", Kind::Doc),
    ("makefile", Kind::Code),
    ("readme", Kind::Doc),
];

/// javadoc's fixed filenames: generated pages with no dot in their stem. Sorted.
const GENERATED_PAGES: &[&str] = &[
    "allclasses",
    "allclasses-frame",
    "allclasses-index",
    "allclasses-noframe",
    "allpackages-index",
    "constant-values",
    "deprecated-list",
    "element-list",
    "help-doc",
    "index-all",
    "member-search-index",
    "overview-frame",
    "overview-summary",
    "overview-tree",
    "package-frame",
    "package-search-index",
    "package-summary",
    "package-tree",
    "package-use",
    "serialized-form",
    "type-search-index",
];

/// Extension of a file name: case-folded, without the dot; a name starting with a dot
/// has none. Folded rather than lowercased because `ext:` is folded too, or `ext:JPG`
/// misses `photo.jpg` in a Turkish locale.
pub fn ext_of(name: &str) -> String {
    crate::text::DefaultFolder::of(ext_str(name))
}

/// The same extension, as spelled and without allocating, for a caller folding into
/// its own buffer. The rule for what counts as an extension lives here and only here.
pub fn ext_str(name: &str) -> &str {
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() && !ext.is_empty() && ext.len() <= 12 => ext,
        _ => "",
    }
}

/// Is this HTML page one a documentation tool wrote? A dot inside the stem says so:
/// 84.9% of 189,785 HTML files on the measured corpus had one, all sampled rustdoc or
/// dartdoc. javadoc's pages have no dot and are listed by name instead.
fn is_generated_page(name: &str, ext_len: usize) -> bool {
    let stem = &name[..name.len() - ext_len - 1];
    stem.contains('.')
        || GENERATED_PAGES
            .binary_search(&crate::text::DefaultFolder::of(stem).as_str())
            .is_ok()
}

fn table_lookup(table: &[(&str, Kind)], folded: &str) -> Option<Kind> {
    table
        .binary_search_by_key(&folded, |(k, _)| k)
        .ok()
        .map(|i| table[i].1)
}

/// Classify an entry, in this order: `is_dir` first — on macOS `.app` and `.xcodeproj`
/// are directories — then the extension, then the bare name exactly and by prefix, then
/// the executable bit, which never overrides an extension. `mode` of zero is unknown.
pub fn kind_of(is_dir: bool, name: &str, mode: i64) -> Kind {
    if is_dir {
        return Kind::Dir;
    }
    let ext = ext_str(name);
    if ext.is_empty() {
        // Folded rather than lowercased: `LİCENSE` has to reach `license` in a
        // Turkish locale, and `tolower` under `tr_TR` turns it into `lıcense`.
        let folded = crate::text::DefaultFolder::of(name);
        if let Some(k) = table_lookup(NAME_TABLE, &folded) {
            return k;
        }
        if let Some((_, k)) = NAME_PREFIX.iter().find(|(p, _)| folded.starts_with(p)) {
            return *k;
        }
    } else {
        let folded = crate::text::DefaultFolder::of(ext);
        if (folded == "html" || folded == "htm") && is_generated_page(name, ext.len()) {
            return Kind::Build;
        }
        if folded == BIN_IS_BUILD.0 {
            return BIN_IS_BUILD.1;
        }
        if let Some(k) = table_lookup(EXT_TABLE, &folded) {
            return k;
        }
    }
    if mode & 0o111 != 0 {
        return Kind::Exec;
    }
    Kind::File
}

/// Would *opening* this start a program rather than show it? Not [`kind_of`]: a shell
/// script is `Code` and still runs. The execute bit counts only where the name says
/// nothing — on an `ntfs3` mount with `fmask=0022` every file carries it.
pub fn runs_when_opened(name: &str, mode: i64) -> bool {
    if kind_of(false, name, mode) == Kind::Exec {
        return true;
    }
    matches!(
        crate::text::DefaultFolder::of(ext_str(name)).as_str(),
        // Executed by the desktop, or by the shell it hands them to.
        "desktop" | "sh" | "bash" | "zsh" | "ksh" | "fish" | "ps1"
        // Windows, where there is no bit to ask about.
        | "scr" | "lnk" | "vbs" | "wsf" | "jar"
    )
}

/// Which of the two id tables to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    User,
    Group,
}

/// The name behind a numeric id, from this machine. Read once and kept: an export of
/// two million rows asks two million times. An id with no name answers as itself,
/// which is also what happens where the tables are absent, as on Windows.
pub fn owner_name(which: Owner, id: i64) -> String {
    use std::collections::HashMap;
    use std::sync::OnceLock;
    static USERS: OnceLock<HashMap<i64, String>> = OnceLock::new();
    static GROUPS: OnceLock<HashMap<i64, String>> = OnceLock::new();
    let table = |file: &str| -> HashMap<i64, String> {
        std::fs::read_to_string(file)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| {
                let mut f = line.split(':');
                let name = f.next()?.to_owned();
                let id = f.nth(1)?.parse::<i64>().ok()?;
                Some((id, name))
            })
            .collect()
    };
    let map = match which {
        Owner::User => USERS.get_or_init(|| table("/etc/passwd")),
        Owner::Group => GROUPS.get_or_init(|| table("/etc/group")),
    };
    map.get(&id).cloned().unwrap_or_else(|| id.to_string())
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
    fn a_directory_has_no_extension() {
        // Real names, off the Windows volume. A dot in a folder name is
        // ordinary there, and the last one is not a type.
        for name in ["TRABZON.MÜZEKKERE.CEVABI", "Trabzon 2. Grup", "mod.rs"] {
            let dir = entry(&format!("/mnt/depo/{name}"), true, 0o40755);
            assert_eq!(dir.ext(), "", "a directory named {name} has no extension");
            let file = entry(&format!("/mnt/depo/{name}"), false, 0o100644);
            assert_ne!(file.ext(), "", "a file named {name} still has one");
        }
    }

    #[test]
    fn kinds() {
        assert_eq!(kind_of(true, "src", 0o40755), Kind::Dir);
        assert_eq!(kind_of(false, "photo.png", 0o100644), Kind::Image);
        // The permission bit is how an extensionless binary is recognised.
        assert_eq!(kind_of(false, "scourd", 0o100755), Kind::Exec);
        assert_eq!(kind_of(false, "stderr", 0o100644), Kind::File);
        // And it never overrides an extension: 3,408 executable `.png` files counted.
        assert_eq!(kind_of(false, "photo.png", 0o100755), Kind::Image);
        // With an unknown mode — Windows — the extension still decides.
        assert_eq!(kind_of(false, "setup.exe", 0), Kind::Exec);
        assert_eq!(Kind::from_name("klasör"), Some(&[Kind::Dir][..]));
        assert_eq!(Kind::from_name("folder"), Some(&[Kind::Dir][..]));
        assert_eq!(Kind::from_name("nope"), None);
        assert_eq!(Kind::from_u8(Kind::Media.as_u8()), Some(Kind::Media));
    }

    #[test]
    fn the_tables_are_sorted_and_hold_each_extension_once() {
        // Binary-searched: an unsorted row is an extension that stops being found.
        for (what, table) in [("ext", EXT_TABLE), ("name", NAME_TABLE)] {
            for pair in table.windows(2) {
                assert!(
                    pair[0].0 < pair[1].0,
                    "{what} table: {:?} is not before {:?}",
                    pair[0].0,
                    pair[1].0
                );
            }
        }
        for pair in GENERATED_PAGES.windows(2) {
            assert!(pair[0] < pair[1], "pages: {:?} then {:?}", pair[0], pair[1]);
        }
        // The documented exception must not also be in the table, or which one
        // wins would depend on the order of two `if`s.
        assert!(table_lookup(EXT_TABLE, BIN_IS_BUILD.0).is_none());
    }

    /// The execute bit answers only where the name does not: an `ntfs3` mount with
    /// `fmask=0022` gives every file on it `0755`, PDFs included.
    #[test]
    fn an_execute_bit_on_a_document_does_not_make_it_a_program() {
        for name in [
            "License.pdf",
            "notlar.docx",
            "yedek.zip",
            "resim.png",
            "a.txt",
        ] {
            assert!(
                !runs_when_opened(name, 0o755),
                "{name} at 0755 must still be something to open, not to run"
            );
        }
        // Where the name says nothing, the bit is all there is — as in `kind_of`.
        assert!(runs_when_opened("scourd", 0o755));
        assert!(!runs_when_opened("LICENSE", 0o644));
    }

    /// The type column and the double-click are decided by different functions, and
    /// they must not drift.
    #[test]
    fn every_executable_kind_runs_when_opened() {
        for name in [
            "a.exe",
            "a.msi",
            "a.appimage",
            "a.run",
            "a.bat",
            "a.cmd",
            "a.com",
        ] {
            assert_eq!(kind_of(false, name, 0o644), Kind::Exec, "{name}");
            assert!(runs_when_opened(name, 0o644), "{name}");
        }
        // And the ones that are not `Exec` and run anyway, without the bit: a script
        // is `Code` and a launcher is a text file.
        for name in ["setup.sh", "start.desktop", "build.ps1", "thing.lnk"] {
            assert_ne!(kind_of(false, name, 0o644), Kind::Exec, "{name}");
            assert!(runs_when_opened(name, 0o644), "{name}");
        }
    }

    #[test]
    fn every_kind_can_be_asked_for_by_its_own_token() {
        // A query built out of `msgid()` searches for literal text: `Executable`
        // folds to a word the parser does not take.
        for k in Kind::ALL {
            let token = crate::text::DefaultFolder::of(k.token());
            let got = Kind::from_name(&token)
                .unwrap_or_else(|| panic!("kind:{} does not parse", k.token()));
            assert!(got.contains(&k), "kind:{} does not mean {k:?}", k.token());
        }
        // A retired kind is still decodable and still searchable, so an index
        // written before the split answers `kind:media` the way it always did.
        assert!(
            Kind::from_name("media")
                .expect("media")
                .contains(&Kind::Media)
        );
        assert!(
            !Kind::OFFERED.contains(&Kind::Media),
            "retired, not offered"
        );
    }

    #[test]
    fn generated_documentation_is_build_output_and_a_written_page_is_not() {
        // Worth 9.8 percentage points on the measured corpus: 83.7% of the
        // HTML there is rustdoc or javadoc.
        assert_eq!(kind_of(false, "struct.Segment.html", 0), Kind::Build);
        assert_eq!(kind_of(false, "mod.rs.html", 0), Kind::Build);
        assert_eq!(kind_of(false, "package-summary.html", 0), Kind::Build);
        assert_eq!(kind_of(false, "toString.html", 0), Kind::Doc);
        assert_eq!(kind_of(false, "index.html", 0), Kind::Doc);
        assert_eq!(kind_of(false, "about.htm", 0), Kind::Doc);
    }

    #[test]
    fn an_extensionless_name_is_still_worth_asking_about() {
        assert_eq!(kind_of(false, "LICENSE-MIT", 0), Kind::Doc);
        assert_eq!(kind_of(false, "README", 0), Kind::Doc);
        assert_eq!(kind_of(false, "Makefile", 0), Kind::Code);
        assert_eq!(kind_of(false, ".gitignore", 0), Kind::Config);
        assert_eq!(kind_of(false, ".cargo-ok", 0), Kind::Build);
        // `LİCENSE` has to reach `license`. Lowercasing under `tr_TR` gives
        // `lıcense` and misses, which is the bug the folder exists to avoid.
        assert_eq!(kind_of(false, "LİCENSE", 0), Kind::Doc);
        // And the fingerprints deliberately left unrecognised stay unknown:
        // every prefix rule that would catch them is a plausible real name.
        assert_eq!(kind_of(false, "build-script-build", 0), Kind::File);
        assert_eq!(kind_of(false, "lib-hashbrown", 0), Kind::File);
    }

    #[test]
    fn a_version_number_is_not_an_extension_even_though_it_looks_like_one() {
        // `ext_str` takes the rightmost dot, so `license-apache-2.0` has extension
        // `0`. Purely numeric extensions are 0.458% of this corpus, all versions, and
        // they land in `File`, which is right by accident.
        assert_eq!(ext_str("license-apache-2.0"), "0");
        assert_eq!(kind_of(false, "license-apache-2.0", 0), Kind::File);
        assert_eq!(kind_of(false, "2.2.20", 0), Kind::File);
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

#[cfg(all(test, unix))]
mod birth_tests {
    use super::Meta;

    /// `dc:` is when the file was born, not when it last changed. `st_ctime` moves on
    /// a chmod, a rename or a new link, so for a file written once it is the
    /// modification time again — 95% of a sample here had the two equal.
    #[test]
    fn changing_a_file_does_not_change_when_it_was_created() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join("scour-birth-probe");
        std::fs::write(&path, b"hello").expect("write");
        let born = Meta::from_std(&std::fs::metadata(&path).expect("stat"), false).ctime;

        // A second, because these are whole seconds: inside one, nothing moves.
        std::thread::sleep(std::time::Duration::from_millis(1_100));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).expect("chmod");

        let after = std::fs::metadata(&path).expect("stat");
        let now = Meta::from_std(&after, false).ctime;
        let _ = std::fs::remove_file(&path);

        // Only where the filesystem keeps a birth time; where it does not, `st_ctime`
        // is the fallback and moving is correct.
        if Meta::birth(&after).is_none() {
            eprintln!("no birth time here; `st_ctime` is the fallback and it moved, as it should");
            return;
        }
        assert_eq!(born, now, "a chmod is not a new file");
        assert!(
            {
                use std::os::unix::fs::MetadataExt;
                after.ctime() > now
            },
            "the status-change time did move, so the test moved something"
        );
    }
}
