//! Every field the language has, written down once.
//!
//! The parser, the highlighter, the completions and the reference all read this
//! table. A field that is not here does not exist anywhere; a field that is here
//! is offered to the user and understood when they type it.

use scour_core::Kind;

use crate::parse::split_cmp;
use crate::time::parse_time_value;

/// What a field does with the text after its colon.
///
/// Tells the highlighter whether a value is usable: `size:abc` is a search for
/// the literal text "size:abc".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Takes {
    /// Any text, folded and matched. Empty is not useful but not an error.
    Text,
    /// A path. Empty is refused: a scope that matches nothing is not a scope.
    Path,
    /// A list of extensions separated by `;`, each with an optional dot.
    Ext,
    /// An optional comparison and a size with a binary unit.
    Size,
    /// A relative window, a calendar day, or either with a comparison.
    Time,
    /// One of the names in [`Kind::from_name`].
    Kind,
    /// Nothing at all: `file:` and `folder:` are complete as written.
    Nothing,
}

/// One field: its canonical spelling, what else it answers to, and what it
/// accepts.
#[derive(Debug, Clone, Copy)]
pub struct Field {
    /// The spelling the reference and the completions use.
    pub name: &'static str,
    /// Everything else that means the same thing, including the Turkish
    /// spellings. Folded before comparison, so case never appears here.
    pub aliases: &'static [&'static str],
    pub takes: Takes,
    /// One line of English, a message id.
    pub about: &'static str,
    /// What to show someone who has typed the field; empty when it takes nothing.
    pub example: &'static str,
}

/// The language, as data.
pub const FIELDS: &[Field] = &[
    Field {
        name: "ext",
        aliases: &[],
        takes: Takes::Ext,
        about: "extension is this, or any of these separated by ;",
        example: "ext:pdf",
    },
    Field {
        name: "path",
        aliases: &[],
        takes: Takes::Text,
        about: "the whole path contains this",
        example: "path:src/api",
    },
    Field {
        name: "under",
        // Not `altında`: folding maps both i's to one letter, so it arrives
        // spelled `altinda`.
        aliases: &["in", "altinda"],
        takes: Takes::Path,
        about: "anywhere below this folder, at any depth",
        example: "under:/home",
    },
    Field {
        name: "parent",
        aliases: &["child", "children"],
        takes: Takes::Path,
        about: "directly inside this folder, one level down",
        example: "parent:/etc",
    },
    Field {
        name: "file",
        aliases: &["files"],
        takes: Takes::Nothing,
        about: "files only",
        example: "",
    },
    Field {
        name: "folder",
        aliases: &["folders", "dir"],
        takes: Takes::Nothing,
        about: "folders only",
        example: "",
    },
    Field {
        name: "size",
        aliases: &[],
        takes: Takes::Size,
        about: "size, in binary units, with an optional comparison",
        example: "size:>1mb",
    },
    Field {
        name: "kind",
        aliases: &["type", "tur", "tür"],
        takes: Takes::Kind,
        about: "one broad type of file",
        example: "kind:image",
    },
    Field {
        name: "dm",
        aliases: &["modified"],
        takes: Takes::Time,
        about: "when it was last modified",
        example: "dm:7d",
    },
    Field {
        name: "dc",
        aliases: &["created"],
        takes: Takes::Time,
        about: "when it was created",
        example: "dc:2026-01-31",
    },
    Field {
        name: "da",
        aliases: &["accessed"],
        takes: Takes::Time,
        about: "when it was last read",
        example: "da:>2026-01-01",
    },
    Field {
        name: "content",
        aliases: &["text", "icerik", "içerik"],
        takes: Takes::Text,
        about: "the document's contents contain this, if contents are indexed",
        example: "content:invoice",
    },
    // ---- what a filesystem knows and nothing else does --------------------
    // `kind:` is what a file is (a document, an image); `node:` is what the
    // filesystem made it — only `node:` tells a symlink from what it points at.
    Field {
        name: "node",
        aliases: &["dugum"],
        takes: Takes::Text,
        about: "what the filesystem made it: f file, d folder, l symlink, s socket, p fifo, b block, c char",
        example: "node:l",
    },
    Field {
        name: "perm",
        aliases: &["mode", "izin"],
        takes: Takes::Text,
        about: "permission bits: 644 exactly, -200 all of these, /222 any of these",
        example: "perm:644",
    },
    Field {
        name: "suid",
        aliases: &[],
        takes: Takes::Nothing,
        about: "runs as its owner",
        example: "",
    },
    Field {
        name: "sgid",
        aliases: &[],
        takes: Takes::Nothing,
        about: "runs as its group",
        example: "",
    },
    Field {
        name: "sticky",
        aliases: &[],
        takes: Takes::Nothing,
        about: "only the owner may delete what is inside",
        example: "",
    },
    Field {
        name: "ww",
        aliases: &["writable"],
        takes: Takes::Nothing,
        about: "anyone may write to it",
        example: "",
    },
    Field {
        name: "user",
        aliases: &["owner"],
        takes: Takes::Text,
        about: "owned by this user, by name or by number",
        example: "user:root",
    },
    Field {
        name: "group",
        aliases: &[],
        takes: Takes::Text,
        about: "owned by this group, by name or by number",
        example: "group:root",
    },
    Field {
        name: "depth",
        aliases: &["derinlik"],
        takes: Takes::Size,
        about: "how many components the path has, counted from the root; a bare number means exactly",
        example: "depth:<=4",
    },
    Field {
        name: "regex",
        aliases: &["re"],
        takes: Takes::Text,
        about: "the name matches this regular expression",
        example: "regex:^[0-9]{4}-",
    },
    // ---- Everything's spellings ------------------------------------------
    // Its documented names for questions this index can answer. `dupe:` and the
    // Windows `attrib:` letters are absent rather than approximated.
    Field {
        name: "empty",
        aliases: &["bos"],
        takes: Takes::Nothing,
        about: "nothing in it: a file of no bytes, or a folder with no entries",
        example: "",
    },
    Field {
        name: "startwith",
        aliases: &["startswith", "prefix"],
        takes: Takes::Text,
        about: "the name begins with this",
        example: "startwith:IMG_",
    },
    Field {
        name: "endwith",
        aliases: &["endswith", "suffix"],
        takes: Takes::Text,
        about: "the name ends with this",
        example: "endwith:.pdf",
    },
    Field {
        name: "len",
        aliases: &["namelen"],
        takes: Takes::Size,
        about: "how many characters the name has",
        example: "len:>40",
    },
    Field {
        name: "case",
        aliases: &["cased"],
        takes: Takes::Text,
        about: "the name contains this, spelled exactly like this",
        example: "case:README",
    },
    // The type macros, which are `kind:` under other names.
    Field {
        name: "audio",
        aliases: &["ses"],
        takes: Takes::Nothing,
        about: "sound files",
        example: "",
    },
    Field {
        name: "video",
        aliases: &[],
        takes: Takes::Nothing,
        about: "video files",
        example: "",
    },
    Field {
        name: "pic",
        aliases: &["image", "gorsel"],
        takes: Takes::Nothing,
        about: "pictures",
        example: "",
    },
    Field {
        name: "doc",
        aliases: &["belge"],
        takes: Takes::Nothing,
        about: "documents",
        example: "",
    },
    Field {
        name: "exe",
        aliases: &[],
        takes: Takes::Nothing,
        about: "programs",
        example: "",
    },
    Field {
        name: "zip",
        aliases: &["archive", "arsiv"],
        takes: Takes::Nothing,
        about: "archives",
        example: "",
    },
];

/// The values `kind:` accepts, in display order, spelled as [`Kind::from_name`]
/// wants them and not as [`Kind::msgid`] does, so a completion always parses.
/// `media` still parses but is not offered.
pub const KIND_VALUES: &[&str] = &[
    "folder", "code", "doc", "image", "data", "config", "archive", "exec", "audio", "video",
    "font", "build", "file",
];

/// Common time values, for the same reason.
pub const TIME_VALUES: &[&str] = &["today", "yesterday", "7d", "30d", "1y"];

/// Find a field by any of its spellings. The name must already be folded.
pub fn lookup(folded: &str) -> Option<&'static Field> {
    FIELDS
        .iter()
        .find(|f| f.name == folded || f.aliases.contains(&folded))
}

/// Can this field use this value as written?
///
/// Calls the same value parsers the parser does, so a value that colours as
/// usable is one that will be used.
pub fn accepts(field: &Field, folded_value: &str, now: i64) -> bool {
    match field.takes {
        Takes::Nothing => true,
        Takes::Text => !folded_value.is_empty(),
        Takes::Path => !folded_value.is_empty(),
        Takes::Ext => folded_value
            .split(';')
            .map(|e| e.trim_start_matches('.'))
            .any(|e| !e.is_empty()),
        Takes::Size => parse_size_value(folded_value).is_some(),
        Takes::Time => parse_time_value(folded_value, now).is_some(),
        Takes::Kind => Kind::from_name(folded_value).is_some(),
    }
}

/// The numeric half of `size:`, without building a [`Match`](scour_core::Match).
pub(crate) fn parse_size_value(v: &str) -> Option<i64> {
    let (_, rest) = split_cmp(v);
    let rest = rest.trim();
    let (num, mult) = if let Some(n) = rest.strip_suffix("tb") {
        (n, 1024_i64.pow(4))
    } else if let Some(n) = rest.strip_suffix("gb") {
        (n, 1024_i64.pow(3))
    } else if let Some(n) = rest.strip_suffix("mb") {
        (n, 1024 * 1024)
    } else if let Some(n) = rest.strip_suffix("kb") {
        (n, 1024)
    } else if let Some(n) = rest.strip_suffix('b') {
        (n, 1)
    } else {
        (rest, 1)
    };
    let value: f64 = num.trim().parse().ok()?;
    Some((value * mult as f64) as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_spelling_is_unique() {
        let mut seen: Vec<&str> = Vec::new();
        for f in FIELDS {
            for name in std::iter::once(&f.name).chain(f.aliases) {
                assert!(!seen.contains(name), "{name} is listed twice");
                seen.push(name);
            }
        }
    }

    #[test]
    fn every_spelling_is_already_folded() {
        // The table is compared against folded input; a capital never matches.
        for f in FIELDS {
            for name in std::iter::once(&f.name).chain(f.aliases) {
                assert_eq!(
                    *name,
                    scour_core::DefaultFolder::of(name),
                    "{name} is not in folded form"
                );
            }
        }
    }

    #[test]
    fn every_offered_kind_value_parses() {
        // A completion the parser reads as plain text is worse than none.
        for v in KIND_VALUES {
            assert!(Kind::from_name(v).is_some(), "kind:{v} does not parse");
        }
    }

    #[test]
    fn every_example_uses_its_own_field() {
        for f in FIELDS {
            if f.example.is_empty() {
                continue;
            }
            assert!(
                f.example.starts_with(&format!("{}:", f.name)),
                "{} has an example for a different field",
                f.name
            );
        }
    }

    #[test]
    fn accepts_agrees_with_the_parser_about_bad_values() {
        let size = lookup("size").unwrap();
        assert!(accepts(size, "1mb", 0));
        assert!(accepts(size, ">1mb", 0));
        assert!(!accepts(size, "abc", 0), "size:abc falls back to text");
        let kind = lookup("kind").unwrap();
        assert!(accepts(kind, "image", 0));
        assert!(!accepts(kind, "zurna", 0), "kind:zurna falls back to text");
        let under = lookup("under").unwrap();
        assert!(!accepts(under, "", 0), "an empty scope is not a scope");
    }

    #[test]
    fn turkish_spellings_resolve() {
        use scour_core::DefaultFolder;
        assert_eq!(lookup("tür").map(|f| f.name), Some("kind"));
        assert_eq!(lookup("içerik").map(|f| f.name), Some("content"));
        // `ü` and `ç` survive folding and must be listed; `ı` does not.
        assert_eq!(
            lookup(&DefaultFolder::of("altında")).map(|f| f.name),
            Some("under")
        );
    }

    #[test]
    fn field_names_are_case_insensitive() {
        use scour_core::DefaultFolder;
        assert_eq!(
            lookup(&DefaultFolder::of("sizE")).map(|f| f.name),
            Some("size")
        );
        assert_eq!(
            lookup(&DefaultFolder::of("EXT")).map(|f| f.name),
            Some("ext")
        );
    }
}
