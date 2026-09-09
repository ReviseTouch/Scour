//! What a preview panel shows, and in what order: a third column holding one
//! row's picture, or the head of it when it is text, and the facts about it.
//! It is a mode, not a glance, and keeps up with the selection. The msgids
//! below are the column headings' own, so two faces cannot end up with two
//! vocabularies for one field.

/// One line of the fact list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fact {
    /// A short, stable name — what a frontend keys its own value off.
    pub id: &'static str,
    /// The English label and the msgid, the same string the column of that
    /// name uses.
    pub msgid: &'static str,
}

/// Every fact, in the order a panel lists them. Where it is comes first: two
/// files with the same name in different folders is the case being decided.
/// `size` is a folder's item count when the row is a folder.
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

/// What the size line is called for this row. A folder's own size is its
/// directory entry — 4 KiB whatever is inside — so a folder is asked what it
/// holds instead.
pub fn size_msgid(is_dir: bool) -> &'static str {
    if is_dir { "Contents" } else { "Size" }
}

/// How wide the panel is, in logical pixels, and what it may be dragged to.
/// 380 is where a 256-pixel thumbnail has margins and a path wraps to two lines;
/// under 200 there is no room for facts, over 600 the list stops being one.
pub const PANEL_WIDE: u32 = 380;
pub const PANEL_MIN: u32 = 200;
pub const PANEL_MAX: u32 = 600;

/// Below this many pixels of window there is no room for three columns, and the
/// panel takes the whole width instead.
pub const NARROW: u32 = 900;

/// One row's facts, as primitives: this crate has no dependencies, so turning a
/// `mode` into `drwxr-xr-x` stays in `scour-core` and only the order, labels and
/// punctuation live here.
#[derive(Debug, Clone, Copy)]
pub struct Facts<'a> {
    /// The folder holding it: "where is this" for a search result.
    pub folder: &'a str,
    /// The kind, already in the reader's language.
    pub kind: &'a str,
    pub is_dir: bool,
    pub size: u64,
    /// What a folder holds, in the reader's language — `3 öğe`. Empty when
    /// nothing counted it, which is the ordinary case.
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
    /// What this fact says, or nothing when there is nothing to say: a date of
    /// zero is not a date, and an empty answer is not a line.
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

    /// Every fact that has something behind it, as (msgid, value). The caller
    /// has the catalogue, and `Size` or `Contents` depends on the row.
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

    /// A blank is not a line: a `noatime` volume has no read time to show.
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
