//! What a preview panel shows, and in what order.
//!
//! A third column beside the list, holding one row's picture — or the head of
//! it, when it is text — and the facts about it. Everything's preview pane,
//! and the same idea: the panel is a *mode*, not a glance. It stays where it
//! is and keeps up with whatever the selection lands on.
//!
//! **The facts are here because two faces draw them.** A window and a browser
//! page listing the same file's properties in a different order, or calling
//! its date something different from the column heading above it, is two
//! vocabularies for one thing — and the columns already went through this. The
//! msgids below are the column headings' own, deliberately.

/// One line of the fact list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fact {
    /// A short, stable name — what a frontend keys its own value off.
    pub id: &'static str,
    /// The English label, and the msgid the catalogue is asked for.
    ///
    /// The same string the column of that name uses: a panel that said
    /// "Changed" under a column headed "Modified" would be naming one field
    /// twice.
    pub msgid: &'static str,
}

/// Every fact, in the order a panel lists them.
///
/// **Where it is comes first.** The panel is open because somebody is deciding
/// whether this is the file they meant, and for a search result the answer is
/// nearly always the path — two files with the same name in different folders
/// is the case the whole program exists for.
///
/// `size` is a folder's item count when the row is a folder; a folder has a
/// size and it is not one anybody means. See [`self::size_of`].
pub const FACTS: &[Fact] = &[
    Fact {
        id: "where",
        msgid: "Location",
    },
    Fact {
        id: "kind",
        msgid: "Kind",
    },
    Fact {
        id: "size",
        msgid: "Size",
    },
    Fact {
        id: "modified",
        msgid: "Modified",
    },
    Fact {
        id: "created",
        msgid: "Created",
    },
    Fact {
        id: "read",
        msgid: "Accessed",
    },
    Fact {
        id: "mode",
        msgid: "Mode",
    },
    Fact {
        id: "owner",
        msgid: "Owner",
    },
];

/// What the size line is called for this row.
///
/// A folder's own size is the size of its directory entry — four kilobytes,
/// the same for a folder holding two files and one holding two hundred
/// thousand. What anybody means by "how big is this folder" is either what is
/// in it or what it weighs, and the panel answers the first because it is the
/// one the index has to hand.
pub fn size_msgid(is_dir: bool) -> &'static str {
    if is_dir { "Contents" } else { "Size" }
}

/// How wide the panel is, in logical pixels, and what it may be dragged to.
///
/// **Three hundred and eighty**, which is the width at which a 256-pixel
/// thumbnail has margins and a path wraps to two lines rather than five. The
/// bounds are what keeps the list usable: under two hundred the panel shows a
/// picture and no facts, and past six hundred the list it is beside stops
/// being a list.
pub const PANEL_WIDE: u32 = 380;
pub const PANEL_MIN: u32 = 200;
pub const PANEL_MAX: u32 = 600;

/// Below this many pixels of window there is no room for three columns, and
/// the panel takes the whole width instead.
///
/// The list is what you came from; the panel is what you are reading.
pub const NARROW: u32 = 900;

/// One row's facts, as the pieces a panel needs to write them.
///
/// **Primitives, because this crate has no dependencies and should not get
/// any.** Turning a `mode` into `drwxr-xr-x` and a `uid` into a name is
/// `scour-core`'s, and both faces already call it; what is left — the order,
/// the labels, the punctuation of a size and a date — is here, so that a
/// window and a terminal writing the same eight lines cannot write them
/// differently.
#[derive(Debug, Clone, Copy)]
pub struct Facts<'a> {
    /// The folder holding it, which is what "where is this" means for a
    /// search result.
    pub folder: &'a str,
    /// The kind, already in the reader's language.
    pub kind: &'a str,
    pub is_dir: bool,
    pub size: u64,
    /// What a folder holds, already in the reader's language — `3 öğe`. Empty
    /// when nothing counted it, which is the ordinary case for a folder.
    pub items: &'a str,
    pub mtime: i64,
    pub ctime: i64,
    pub atime: i64,
    /// `drwxr-xr-x`, from [`scour_core::mode_string`].
    pub mode: &'a str,
    /// `hasan · hasan`, from [`scour_core::owner_name`].
    pub owner: &'a str,
}

impl Facts<'_> {
    /// What this fact says, or nothing when there is nothing to say.
    ///
    /// An empty answer is not a line: a volume that does not record read times
    /// would otherwise show a row headed `Accessed` with a blank beside it for
    /// ever, and a date of zero is not a date.
    pub fn value(&self, id: &str, decimal: char) -> String {
        match id {
            "where" => self.folder.to_owned(),
            "kind" => self.kind.to_owned(),
            "size" if self.is_dir => self.items.to_owned(),
            "size" => crate::format::size(self.size, decimal),
            "modified" => stamp(self.mtime),
            "created" => stamp(self.ctime),
            "read" => stamp(self.atime),
            "mode" => self.mode.to_owned(),
            "owner" => self.owner.to_owned(),
            _ => String::new(),
        }
    }

    /// Every fact that has something behind it, as (msgid, value).
    ///
    /// The msgid rather than the label: the caller has the catalogue, and
    /// which of `Size` and `Contents` a row wants depends on what it is.
    pub fn lines(&self, decimal: char) -> Vec<(&'static str, String)> {
        FACTS
            .iter()
            .filter_map(|fact| {
                let value = self.value(fact.id, decimal);
                let msgid = if fact.id == "size" {
                    size_msgid(self.is_dir)
                } else {
                    fact.msgid
                };
                (!value.is_empty()).then_some((msgid, value))
            })
            .collect()
    }
}

/// A date a person can read, and nothing at all when there is no date.
fn stamp(when: i64) -> String {
    if when <= 0 {
        return String::new();
    }
    crate::format::stamp(when)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_fact_has_a_short_stable_name_and_a_label() {
        let mut seen: Vec<&str> = Vec::new();
        for fact in FACTS {
            assert!(!fact.id.is_empty() && fact.id.len() <= 12, "{}", fact.id);
            assert!(!fact.msgid.is_empty(), "{}", fact.id);
            assert!(!seen.contains(&fact.id), "{} is listed twice", fact.id);
            seen.push(fact.id);
        }
    }

    /// The labels are the columns' own, and that is the point of them.
    #[test]
    fn the_labels_are_the_column_headings() {
        let columns: Vec<&str> = crate::COLUMNS.iter().map(|c| c.msgid).collect();
        for id in ["kind", "size", "modified"] {
            let fact = FACTS.iter().find(|f| f.id == id).expect("listed");
            assert!(
                columns.contains(&fact.msgid),
                "{id} calls itself {:?}, which no column does",
                fact.msgid
            );
        }
    }

    #[test]
    fn a_folder_is_asked_what_is_in_it_rather_than_how_big_it_is() {
        assert_eq!(size_msgid(true), "Contents");
        assert_eq!(size_msgid(false), "Size");
    }

    fn some() -> Facts<'static> {
        Facts {
            folder: "/home/hasan/Belgeler",
            kind: "Belge",
            is_dir: false,
            size: 5261,
            items: "",
            mtime: 1_700_000_000,
            ctime: 1_700_000_000,
            atime: 0,
            mode: "-rw-r--r--",
            owner: "hasan · hasan",
        }
    }

    /// A blank is not a line. A volume mounted `noatime` has no read time, and
    /// a row headed `Accessed` with nothing beside it says less than no row.
    #[test]
    fn a_fact_with_nothing_behind_it_is_left_out() {
        let lines = some().lines(',');
        assert!(lines.iter().any(|(id, _)| *id == "Modified"));
        assert!(
            !lines.iter().any(|(id, _)| *id == "Accessed"),
            "a zero timestamp is not a date"
        );
    }

    /// The order is the order of the question: where, then what, then how big.
    #[test]
    fn the_lines_come_in_the_tables_order() {
        let lines = some().lines(',');
        let ids: Vec<&str> = lines.iter().map(|(id, _)| *id).collect();
        assert_eq!(&ids[..3], &["Location", "Kind", "Size"]);
    }

    #[test]
    fn a_folder_says_what_is_in_it_where_a_file_says_its_size() {
        let mut f = some();
        f.is_dir = true;
        f.items = "3 öğe";
        let lines = f.lines(',');
        assert!(lines.contains(&("Contents", "3 öğe".to_owned())));
        assert!(!lines.iter().any(|(id, _)| *id == "Size"));
    }
}
