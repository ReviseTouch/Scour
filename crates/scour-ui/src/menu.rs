//! What the right-click menu offers, in one table every face reads, so three
//! menus cannot drift apart — as [`COLUMNS`](crate::COLUMNS) does not.
//!
//! Everything here is safe to press: [`Weight::Careful`] items are reversible,
//! and permanent deletion is not in the table — it needs a held modifier.

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
}

/// The whole menu, in the order it is drawn. Order is meaning: the first group
/// is under the pointer when the menu opens, and a rule sits before the
/// wastebasket so a travelling hand stops short of it.
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
    },
    Item {
        id: "open-with",
        msgid: "Open with…",
        key: "",
        group: 0,
        weight: Weight::Plain,
        when: When::File,
        except: &[],
    },
    Item {
        id: "folder",
        msgid: "Open its folder",
        key: "Ctrl+Enter",
        group: 0,
        weight: Weight::Plain,
        when: When::File,
        except: &[],
    },
    Item {
        id: "folder",
        msgid: "Open the folder above",
        key: "Ctrl+Enter",
        group: 0,
        weight: Weight::Plain,
        when: When::Folder,
        except: &[],
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
    },
    Item {
        id: "search-here",
        msgid: "Search inside it",
        key: "Alt+↓",
        group: 1,
        weight: Weight::Plain,
        when: When::Folder,
        except: &[],
    },
    Item {
        id: "search-kind",
        msgid: "Search the same kind",
        key: "",
        group: 1,
        weight: Weight::Plain,
        when: When::File,
        except: &[],
    },
    Item {
        id: "duplicates",
        msgid: "Find its duplicates",
        key: "",
        group: 1,
        weight: Weight::Plain,
        when: When::File,
        except: &[],
    },
    Item {
        id: "usage",
        msgid: "What it weighs",
        key: "",
        group: 1,
        weight: Weight::Plain,
        when: When::Folder,
        except: &[],
    },
    Item {
        id: "skip",
        msgid: "Skip it in searches…",
        key: "",
        group: 1,
        weight: Weight::Plain,
        when: When::Folder,
        except: &[],
    },
    // ---- one row: the clipboard ------------------------------------------
    Item {
        id: "copy-path",
        msgid: "Copy the path",
        key: "Ctrl+C",
        group: 2,
        weight: Weight::Plain,
        when: When::One,
        except: &[],
    },
    Item {
        id: "copy-name",
        msgid: "Copy the name",
        key: "Ctrl+Shift+C",
        group: 2,
        weight: Weight::Plain,
        when: When::One,
        except: &[],
    },
    Item {
        id: "copy-file",
        msgid: "Copy the file",
        key: "",
        group: 2,
        weight: Weight::Plain,
        when: When::File,
        except: &[crate::faces::Face::Page],
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
    },
    Item {
        id: "trash",
        msgid: "Move to the wastebasket",
        key: "Delete",
        group: 3,
        weight: Weight::Careful,
        when: When::One,
        except: &[],
    },
    // ---- one row: look closer --------------------------------------------
    Item {
        id: "details",
        msgid: "Details",
        key: "Space",
        group: 4,
        weight: Weight::Plain,
        when: When::One,
        except: &[],
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
    },
    Item {
        id: "open-all",
        msgid: "Open all {n}…",
        key: "",
        group: 0,
        weight: Weight::Heavy,
        when: When::Many,
        except: &[],
    },
    Item {
        id: "copy-path",
        msgid: "Copy {n} paths",
        key: "Ctrl+C",
        group: 1,
        weight: Weight::Plain,
        when: When::Many,
        except: &[],
    },
    Item {
        id: "copy-name",
        msgid: "Copy {n} names",
        key: "Ctrl+Shift+C",
        group: 1,
        weight: Weight::Plain,
        when: When::Many,
        except: &[],
    },
    Item {
        id: "copy-file",
        msgid: "Copy {n} files",
        key: "",
        group: 1,
        weight: Weight::Plain,
        when: When::Many,
        except: &[crate::faces::Face::Page],
    },
    Item {
        id: "csv",
        msgid: "Export as CSV…",
        key: "",
        group: 1,
        weight: Weight::Plain,
        when: When::Many,
        except: &[],
    },
    Item {
        id: "trash",
        msgid: "Move {n} to the wastebasket",
        key: "Delete",
        group: 2,
        weight: Weight::Careful,
        when: When::Many,
        except: &[],
    },
    Item {
        id: "clear",
        msgid: "Drop the selection",
        key: "Esc",
        group: 3,
        weight: Weight::Plain,
        when: When::Many,
        except: &[],
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

/// Where a rule goes: true when this item starts a new group. Given to the
/// faces, so none of them draws a line above the first item.
pub fn rule_before(previous: Option<&Item>, item: &Item) -> bool {
    previous.is_some_and(|p| p.group != item.group)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rule the table exists to keep.
    #[test]
    fn nothing_in_the_menu_is_irreversible() {
        for item in ITEMS {
            assert!(
                item.id != "delete" && !item.msgid.to_lowercase().contains("permanent"),
                "{} is in the menu and cannot be undone",
                item.id
            );
        }
        // The one thing that changes the disk says where it went.
        let trash: Vec<_> = ITEMS.iter().filter(|i| i.id == "trash").collect();
        assert_eq!(trash.len(), 2, "one for a row, one for a selection");
        for t in trash {
            assert_eq!(t.weight, Weight::Careful);
            assert!(t.msgid.contains("wastebasket"), "{}", t.msgid);
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
