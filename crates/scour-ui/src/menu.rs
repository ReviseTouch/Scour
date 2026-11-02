//! What the right-click menu offers, in one table every face reads, so three
//! menus cannot drift apart — as [`COLUMNS`](crate::COLUMNS) does not.
//!
//! Everything here is safe to press but one: [`Weight::Careful`] items are
//! reversible, and permanent deletion — which is not — says so in its name,
//! asks first, and stands behind the trash so a hand reaches the trash first.
//!
//! A menu is drawn in two parts: a strip of a few short words across the top
//! (the items with a [`Item::short`]) and the list below it. The trash is in
//! the strip; permanent deletion stays at the foot of the list, behind a rule.

/// How dangerous an item is: the only thing a face renders differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    /// Press it and find out; nothing is lost.
    Plain,
    /// Reversible, but it changes something. Drawn apart, in the warning colour.
    Careful,
    /// Starts more programs, or takes long enough to notice. Dimmed, and it
    /// asks first — the trailing `…` says so.
    Heavy,
}

/// Which shape of selection an item belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// One row, and it is a file.
    File,
    /// One row, and it is a directory.
    Folder,
    /// One row, either kind.
    One,
    /// More than one row.
    Many,
}

/// One line of the menu.
#[derive(Debug, Clone, Copy)]
pub struct Item {
    /// What a face sends back when it is pressed. Never translated.
    pub id: &'static str,
    /// The English source string, and therefore the catalogue key. `{n}` is the
    /// number of rows selected, punctuated by whoever draws it.
    pub msgid: &'static str,
    /// The shortcut to print at the right, in a person's notation; empty if none.
    pub key: &'static str,
    /// Items with the same number sit together and a rule is drawn where it
    /// changes; an ordering, not a count.
    pub group: u8,
    pub weight: Weight,
    pub when: When,
    /// Faces that cannot perform this one, because of the platform rather than
    /// a gap: a browser cannot put a *file* on the clipboard.
    pub except: &'static [crate::faces::Face],
    /// Which drawing stands beside it, by name; each face holds its own set.
    pub icon: &'static str,
    /// Set for the few that sit in the strip across the top of the menu, a
    /// press away, rather than in its list: the strip's own short word, and
    /// the catalogue key for it. Empty for everything in the list.
    pub short: &'static str,
}

/// The whole menu, in the order it is drawn. Order is meaning: the first group
/// is under the pointer when the menu opens, and a rule sits before the
/// permanent deletion so a travelling hand stops short of it.
pub const ITEMS: &[Item] = &[
    // ---- one row: open ---------------------------------------------------
    Item {
        id: "open",
        msgid: "Open",
        key: "Enter",
        group: 0,
        weight: Weight::Plain,
        when: When::One,
        except: &[],
        icon: "open",
        short: "",
    },
    Item {
        id: "open-with",
        msgid: "Open with…",
        key: "",
        group: 0,
        weight: Weight::Plain,
        when: When::File,
        except: &[],
        icon: "apps",
        short: "",
    },
    Item {
        id: "folder",
        msgid: "Open its folder",
        key: "Ctrl+Enter",
        group: 0,
        weight: Weight::Plain,
        when: When::File,
        except: &[],
        icon: "folder",
        short: "",
    },
    Item {
        id: "folder",
        msgid: "Open the folder above",
        key: "Ctrl+Enter",
        group: 0,
        weight: Weight::Plain,
        when: When::Folder,
        except: &[],
        icon: "up",
        short: "",
    },
    // ---- one row: ask the index something else ---------------------------
    Item {
        id: "search-here",
        msgid: "Search this folder",
        key: "Alt+↓",
        group: 1,
        weight: Weight::Plain,
        when: When::File,
        except: &[],
        icon: "search",
        short: "",
    },
    Item {
        id: "search-here",
        msgid: "Search inside it",
        key: "Alt+↓",
        group: 1,
        weight: Weight::Plain,
        when: When::Folder,
        except: &[],
        icon: "search",
        short: "",
    },
    Item {
        id: "search-kind",
        msgid: "Search the same kind",
        key: "",
        group: 1,
        weight: Weight::Plain,
        when: When::File,
        except: &[],
        icon: "kind",
        short: "",
    },
    Item {
        id: "duplicates",
        msgid: "Find its duplicates",
        key: "",
        group: 1,
        weight: Weight::Plain,
        when: When::File,
        except: &[],
        icon: "dupes",
        short: "",
    },
    Item {
        id: "usage",
        msgid: "What it weighs",
        key: "",
        group: 1,
        weight: Weight::Plain,
        when: When::Folder,
        except: &[],
        icon: "weight",
        short: "",
    },
    Item {
        id: "skip",
        msgid: "Skip it in searches…",
        key: "",
        group: 1,
        weight: Weight::Plain,
        when: When::Folder,
        except: &[],
        icon: "skip",
        short: "",
    },
    // ---- one row: the clipboard ------------------------------------------
    Item {
        id: "copy-file",
        msgid: "Copy the file",
        key: "",
        group: 2,
        weight: Weight::Plain,
        when: When::File,
        except: &[crate::faces::Face::Page],
        icon: "copy",
        short: "File",
    },
    Item {
        id: "copy-path",
        msgid: "Copy the path",
        key: "Ctrl+C",
        group: 2,
        weight: Weight::Plain,
        when: When::One,
        except: &[],
        icon: "path",
        short: "Path",
    },
    Item {
        id: "copy-name",
        msgid: "Copy the name",
        key: "Ctrl+Shift+C",
        group: 2,
        weight: Weight::Plain,
        when: When::One,
        except: &[],
        icon: "name",
        short: "Name",
    },
    Item {
        id: "send",
        msgid: "Send to…",
        key: "",
        group: 2,
        weight: Weight::Plain,
        when: When::One,
        except: &[],
        icon: "send",
        short: "",
    },
    // The page and the terminal open the window's drag tray; the window
    // drags the row itself.
    Item {
        id: "drag",
        msgid: "Drag as a file…",
        key: "",
        group: 2,
        weight: Weight::Plain,
        when: When::One,
        except: &[crate::faces::Face::Window],
        icon: "drag",
        short: "",
    },
    Item {
        id: "details",
        msgid: "Details",
        key: "Space",
        group: 2,
        weight: Weight::Plain,
        when: When::One,
        except: &[],
        icon: "info",
        short: "",
    },
    // ---- one row: change it ----------------------------------------------
    Item {
        id: "rename",
        msgid: "Rename…",
        key: "F2",
        group: 3,
        weight: Weight::Careful,
        when: When::One,
        except: &[],
        icon: "rename",
        short: "Rename",
    },
    Item {
        id: "trash",
        msgid: "Move to trash",
        key: "Delete",
        group: 3,
        weight: Weight::Careful,
        when: When::One,
        except: &[],
        icon: "trash",
        short: "Trash",
    },
    Item {
        id: "delete",
        msgid: "Delete permanently…",
        key: "Shift+Delete",
        group: 3,
        weight: Weight::Careful,
        when: When::One,
        except: &[],
        icon: "delete",
        short: "",
    },
    // ---- a selection ------------------------------------------------------
    Item {
        id: "folders",
        msgid: "Open their folders",
        key: "",
        group: 0,
        weight: Weight::Plain,
        when: When::Many,
        except: &[],
        icon: "folder",
        short: "",
    },
    Item {
        id: "open-all",
        msgid: "Open all {n}…",
        key: "",
        group: 0,
        weight: Weight::Heavy,
        when: When::Many,
        except: &[],
        icon: "open",
        short: "",
    },
    Item {
        id: "copy-file",
        msgid: "Copy {n} files",
        key: "",
        group: 1,
        weight: Weight::Plain,
        when: When::Many,
        except: &[crate::faces::Face::Page],
        icon: "copy",
        short: "File",
    },
    Item {
        id: "copy-path",
        msgid: "Copy {n} paths",
        key: "Ctrl+C",
        group: 1,
        weight: Weight::Plain,
        when: When::Many,
        except: &[],
        icon: "path",
        short: "Path",
    },
    Item {
        id: "copy-name",
        msgid: "Copy {n} names",
        key: "Ctrl+Shift+C",
        group: 1,
        weight: Weight::Plain,
        when: When::Many,
        except: &[],
        icon: "name",
        short: "Name",
    },
    Item {
        id: "drag",
        msgid: "Drag {n} as files…",
        key: "",
        group: 1,
        weight: Weight::Plain,
        when: When::Many,
        except: &[crate::faces::Face::Window],
        icon: "drag",
        short: "",
    },
    Item {
        id: "send",
        msgid: "Send {n} to…",
        key: "",
        group: 1,
        weight: Weight::Plain,
        when: When::Many,
        except: &[],
        icon: "send",
        short: "",
    },
    Item {
        id: "csv",
        msgid: "Export as CSV…",
        key: "",
        group: 1,
        weight: Weight::Plain,
        when: When::Many,
        except: &[],
        icon: "csv",
        short: "",
    },
    Item {
        id: "trash",
        msgid: "Move {n} to trash",
        key: "Delete",
        group: 2,
        weight: Weight::Careful,
        when: When::Many,
        except: &[],
        icon: "trash",
        short: "Trash",
    },
    Item {
        id: "delete",
        msgid: "Delete {n} permanently…",
        key: "Shift+Delete",
        group: 2,
        weight: Weight::Careful,
        when: When::Many,
        except: &[],
        icon: "delete",
        short: "",
    },
    Item {
        id: "clear",
        msgid: "Drop the selection",
        key: "Esc",
        group: 3,
        weight: Weight::Plain,
        when: When::Many,
        except: &[],
        icon: "clear",
        short: "",
    },
];

/// The menu for one shape of selection, in order. `is_dir` describes the row
/// under the pointer, which only matters when one row is picked.
pub fn items_for(
    selected: usize,
    is_dir: bool,
    face: crate::faces::Face,
) -> impl Iterator<Item = &'static Item> {
    let many = selected > 1;
    ITEMS
        .iter()
        .filter(move |i| !i.except.contains(&face))
        .filter(move |i| match i.when {
            When::Many => many,
            When::One => !many,
            When::File => !many && !is_dir,
            When::Folder => !many && is_dir,
        })
}

/// Every drawing an [`Item::icon`] names: a face holds one of each.
pub const ICONS: &[&str] = &[
    "open", "apps", "folder", "up", "search", "kind", "dupes", "weight", "skip", "path", "name",
    "copy", "drag", "send", "rename", "trash", "delete", "info", "csv", "clear",
];

/// The drawing by that name, as an SVG drawn in black at 24 px with a 1.6
/// stroke: [`ICONS`], plus `stack` (a selection's heading) and `chevron` (an
/// item that opens beside the menu). The window reads the same files by path;
/// the page is handed these.
pub fn icon_svg(name: &str) -> Option<&'static str> {
    Some(match name {
        "open" => include_str!("../icons/menu/open.svg"),
        "apps" => include_str!("../icons/menu/apps.svg"),
        "folder" => include_str!("../icons/menu/folder.svg"),
        "up" => include_str!("../icons/menu/up.svg"),
        "search" => include_str!("../icons/menu/search.svg"),
        "kind" => include_str!("../icons/menu/kind.svg"),
        "dupes" => include_str!("../icons/menu/dupes.svg"),
        "weight" => include_str!("../icons/menu/weight.svg"),
        "skip" => include_str!("../icons/menu/skip.svg"),
        "path" => include_str!("../icons/menu/path.svg"),
        "name" => include_str!("../icons/menu/name.svg"),
        "copy" => include_str!("../icons/menu/copy.svg"),
        "drag" => include_str!("../icons/menu/drag.svg"),
        "send" => include_str!("../icons/menu/send.svg"),
        "rename" => include_str!("../icons/menu/rename.svg"),
        "trash" => include_str!("../icons/menu/trash.svg"),
        "delete" => include_str!("../icons/menu/delete.svg"),
        "info" => include_str!("../icons/menu/info.svg"),
        "csv" => include_str!("../icons/menu/csv.svg"),
        "clear" => include_str!("../icons/menu/clear.svg"),
        "stack" => include_str!("../icons/menu/stack.svg"),
        "chevron" => include_str!("../icons/menu/chevron.svg"),
        _ => return None,
    })
}

/// The strip across the top: the items that have a short word, in order.
pub fn strip(items: &[&'static Item]) -> Vec<&'static Item> {
    items
        .iter()
        .copied()
        .filter(|i| !i.short.is_empty())
        .collect()
}

/// The list under the strip: everything else, in order. Rules are worked out
/// over this, not over the whole menu: the strip has none.
pub fn list(items: &[&'static Item]) -> Vec<&'static Item> {
    items
        .iter()
        .copied()
        .filter(|i| i.short.is_empty())
        .collect()
}

/// Where a rule goes: true when this item starts a new group. Given to the
/// faces, so none of them draws a line above the first item.
pub fn rule_before(previous: Option<&Item>, item: &Item) -> bool {
    previous.is_some_and(|p| p.group != item.group)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule the table exists to keep: one item cannot be undone, and it
    /// says so, asks first, and stands right behind the trash for each shape.
    #[test]
    fn the_one_irreversible_item_says_so_asks_first_and_follows_the_trash() {
        for (at, item) in ITEMS.iter().enumerate() {
            let permanent = item.msgid.to_lowercase().contains("permanent");
            assert_eq!(item.id == "delete", permanent, "{}", item.msgid);
            if item.id != "delete" {
                continue;
            }
            assert!(
                item.msgid.ends_with('…'),
                "{} does not ask first",
                item.msgid
            );
            assert_eq!(item.weight, Weight::Careful);
            let before = &ITEMS[at - 1];
            assert!(
                before.id == "trash" && before.when == item.when && before.group == item.group,
                "{} does not stand behind the trash",
                item.msgid
            );
        }
        assert_eq!(ITEMS.iter().filter(|i| i.id == "delete").count(), 2);
        // The one thing that changes the disk says where it went.
        let trash: Vec<_> = ITEMS.iter().filter(|i| i.id == "trash").collect();
        assert_eq!(trash.len(), 2, "one for a row, one for a selection");
        for t in trash {
            assert_eq!(t.weight, Weight::Careful);
            assert!(t.msgid.contains("trash"), "{}", t.msgid);
        }
    }

    /// A face switches on `id`, so two items that mean different things must
    /// not answer to the same word.
    #[test]
    fn an_id_always_means_the_same_thing() {
        for item in ITEMS {
            for other in ITEMS {
                if item.id == other.id && item.when != other.when {
                    // Same action, different wording per shape: allowed.
                    continue;
                }
                if std::ptr::eq(item, other) {
                    continue;
                }
                assert!(
                    item.id != other.id || item.when != other.when,
                    "{} appears twice for the same shape",
                    item.id
                );
            }
        }
    }

    #[test]
    fn every_shape_gets_a_menu_and_none_of_them_starts_with_a_rule() {
        for (selected, is_dir) in [(1usize, false), (1, true), (12, false)] {
            let items: Vec<_> = items_for(selected, is_dir, crate::faces::Face::Window).collect();
            assert!(items.len() >= 5, "{selected}/{is_dir}: {}", items.len());
            assert!(!rule_before(None, items[0]));
            // Groups only go forwards, so no rule is drawn twice.
            let mut last = items[0].group;
            for i in &items[1..] {
                assert!(i.group >= last, "{} went backwards", i.id);
                last = i.group;
            }
        }
    }

    /// The strip holds a handful, has a drawing for each, and never holds the
    /// one item that cannot be undone; and every item names a drawing there is.
    #[test]
    fn the_strip_is_short_and_permanent_deletion_stays_in_the_list() {
        for name in ICONS.iter().chain(&["stack", "chevron"]) {
            assert!(
                icon_svg(name).is_some_and(|s| s.starts_with("<svg")),
                "{name}"
            );
        }
        for item in ITEMS {
            assert!(
                ICONS.contains(&item.icon),
                "{}: no drawing called {}",
                item.id,
                item.icon
            );
            assert!(item.short.is_empty() || item.id != "delete");
        }
        for face in [
            crate::faces::Face::Page,
            crate::faces::Face::Window,
            crate::faces::Face::Terminal,
        ] {
            for (selected, is_dir) in [(1usize, false), (1, true), (12, false)] {
                let all: Vec<_> = items_for(selected, is_dir, face).collect();
                let top = strip(&all);
                assert!(
                    (3..=5).contains(&top.len()),
                    "{face:?} {selected}/{is_dir}: {}",
                    top.len()
                );
                assert!(top.iter().any(|i| i.id == "trash"));
                let rest = list(&all);
                assert!(rest.iter().any(|i| i.id == "delete"));
            }
        }
    }

    /// The counted items are the ones that must carry `{n}`, and only those.
    #[test]
    fn a_number_is_promised_exactly_where_there_is_one() {
        for item in ITEMS {
            let counted = item.msgid.contains("{n}");
            match item.when {
                When::Many => {
                    if item.id == "folders" || item.id == "clear" {
                        assert!(!counted, "{}: no number to show", item.id);
                    }
                }
                _ => assert!(!counted, "{} is one row and promises a count", item.id),
            }
        }
    }
}
