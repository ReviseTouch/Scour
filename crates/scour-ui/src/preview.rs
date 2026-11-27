//! What a preview panel shows, and in what order: a third column holding one
//! row's picture, or the head of it when it is text, and the facts about it.
//! It is a mode, not a glance, and keeps up with the selection. The msgids
//! below are the column headings' own, so two faces cannot end up with two
//! vocabularies for one field.
//!
//! Four layers, top to bottom, always in this order: what it is and where
//! (the name, the folder as parts a press searches), what can be done (three
//! buttons and the menu, never moving), what is in it, and the facts (one
//! line, the rest behind [`DETAILS`]). Nothing in it is animated.

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

/// The one line of facts always shown, by [`FACTS`] id: what it is, how big,
/// when it changed. The rest are [`DETAILS`], behind a press that is kept.
pub const SUMMARY: [&str; 3] = ["kind", "size", "modified"];
/// Behind "Details": the change again as a date, with everything else.
pub const DETAILS: [&str; 6] = ["where", "modified", "created", "read", "mode", "owner"];
/// The summary's last part: `{when}` as the list's own Modified column says it.
pub const CHANGED: &str = "modified {when}";
/// The press that opens and shuts the rest, the menu's own word for it.
pub const MORE: &str = "Details";
/// The fourth button, after the three: the right-click menu, all of it.
pub const MENU_SAID: &str = "The whole menu";

/// The buttons under the name, by [`crate::menu`] id, for what is selected:
/// always three, then the whole menu behind `⋯`, so nothing moves from one
/// row to the next and nothing appears under the pointer. Their words are the
/// menu's own. Many rows copy names, not files: a browser cannot hold a file.
pub fn buttons(is_dir: bool, many: bool) -> [&'static str; 3] {
    match (many, is_dir) {
        (true, _) => ["folders", "copy-path", "copy-name"],
        (false, true) => ["search-here", "open", "copy-path"],
        (false, false) => ["open", "folder", "copy-path"],
    }
}

/// The menu's line for a button, so the two cannot say different things.
pub fn button(id: &str, is_dir: bool, many: bool) -> Option<&'static crate::menu::Item> {
    use crate::menu::When;
    crate::menu::ITEMS.iter().find(|it| {
        it.id == id
            && match it.when {
                When::Many => many,
                When::One => !many,
                When::File => !many && !is_dir,
                When::Folder => !many && is_dir,
            }
    })
}

/// A folder's contents, from what the index already knows and with no file
/// read: one `usage` for the heaviest children, one search for the newest.
pub const HEAVIEST: usize = 6;
pub const NEWEST: usize = 5;
pub const FOLDER_SAID: &str = "{n} items · {size}";
pub const HEAVIEST_SAID: &str = "Heaviest";
pub const NEWEST_SAID: &str = "Newest";
/// A press on any of them, or on a part of the path: [`crate::menu`]'s
/// `search-here`, for that folder.
pub const SEARCH_THERE: &str = "Search this folder";

/// What cannot be drawn: the kind's picture, this, and the desktop's own
/// previewer where there is one.
pub const CANNOT: &str = "This kind of file cannot be shown here.";
pub const NATIVE: &str = "Open in the system previewer";

/// Several rows: how many, then each kind and how many of it, the bytes of the
/// files among them, and across how many folders.
pub const MANY: &str = "{n} selected";
pub const MANY_BYTES: &str = "{size} in all";
pub const MANY_FOLDERS: &str = "in {n} folders";
pub const MANY_FOLDER: &str = "in one folder";

/// Each kind once, with how many of it, the commonest first and a tie in the
/// order met: `Document 2 · Picture 1`.
pub fn tally<'a>(kinds: impl IntoIterator<Item = &'a str>) -> Vec<(&'a str, usize)> {
    let mut out: Vec<(&str, usize)> = Vec::new();
    for k in kinds {
        match out.iter_mut().find(|(seen, _)| *seen == k) {
            Some((_, n)) => *n += 1,
            None => out.push((k, 1)),
        }
    }
    // Stable: a tie keeps the order the rows came in.
    out.sort_by_key(|x| std::cmp::Reverse(x.1));
    out
}

/// A picture's caption: its size in pixels and its extension, `2560 × 1600 · PNG`.
pub fn picture_said(width: u32, height: u32, name: &str) -> String {
    let ext = name
        .rsplit_once('.')
        .map(|(_, e)| e.to_uppercase())
        .unwrap_or_default();
    let mut said = format!("{width} × {height}");
    if !ext.is_empty() {
        said.push_str(" · ");
        said.push_str(&ext);
    }
    said
}

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
        self.these(FACTS.iter().map(|f| f.id), decimal)
    }

    /// What "Details" opens on, in [`DETAILS`]'s order.
    pub fn details(&self, decimal: char) -> Vec<(&'static str, String)> {
        self.these(DETAILS, decimal)
    }

    /// The summary's kind and size, as said: the third part, the change, is
    /// the face's own column format put through [`CHANGED`].
    pub fn summary(&self, decimal: char) -> Vec<String> {
        SUMMARY[..2]
            .iter()
            .map(|id| self.value(id, decimal))
            .filter(|v| !v.is_empty())
            .collect()
    }

    fn these<'i>(
        &self,
        ids: impl IntoIterator<Item = &'i str>,
        decimal: char,
    ) -> Vec<(&'static str, String)> {
        ids.into_iter()
            .filter_map(|id| {
                let fact = FACTS.iter().find(|f| f.id == id)?;
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

    /// One line always; the rest behind a press, the change among them again
    /// as a date. Between them, every fact is somewhere.
    #[test]
    fn the_summary_and_the_details_cover_every_fact() {
        let f = some();
        assert_eq!(f.summary(','), ["Belge", "5,14 KiB"]);
        let ids: Vec<&str> = f.details(',').iter().map(|(id, _)| *id).collect();
        assert_eq!(ids, ["Location", "Modified", "Created", "Mode", "Owner"]);
        for fact in FACTS {
            assert!(
                SUMMARY.contains(&fact.id) || DETAILS.contains(&fact.id),
                "{} is shown nowhere",
                fact.id
            );
        }
    }

    /// Three buttons and the menu, each the menu's own line: nothing a
    /// button says can differ from what the menu says for the same act.
    #[test]
    fn the_buttons_are_the_menus_own_lines() {
        for (dir, many) in [(false, false), (true, false), (false, true)] {
            for id in buttons(dir, many) {
                let it = button(id, dir, many).unwrap_or_else(|| panic!("{id} {dir} {many}"));
                assert!(it.except.is_empty(), "{id} cannot be done in every face");
                assert_eq!(it.weight, crate::menu::Weight::Plain, "{id} is not plain");
            }
        }
        assert_eq!(button("open", false, false).map(|i| i.key), Some("Enter"));
        assert_eq!(
            button("search-here", true, false).map(|i| i.msgid),
            Some("Search inside it")
        );
        assert_eq!(
            button("copy-path", false, true).map(|i| i.msgid),
            Some("Copy {n} paths")
        );
    }

    #[test]
    fn several_rows_are_counted_by_kind_the_commonest_first() {
        let t = tally([
            "Picture", "Document", "Document", "Code", "Picture", "Document",
        ]);
        assert_eq!(t, [("Document", 3), ("Picture", 2), ("Code", 1)]);
        assert_eq!(tally(["A", "B"]), [("A", 1), ("B", 1)]);
        assert!(tally([]).is_empty());
    }

    #[test]
    fn a_picture_says_its_pixels_and_its_extension() {
        assert_eq!(picture_said(2560, 1600, "a.b.png"), "2560 × 1600 · PNG");
        assert_eq!(picture_said(10, 20, "noext"), "10 × 20");
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
