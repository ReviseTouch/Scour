//! What the right-click menu offers, in one table every face reads.
//!
//! **Why a table rather than three menus.** There are four faces and the menu
//! belongs in three of them. Written out where each one draws, they would
//! agree on the day they were written and drift on every day after: an item
//! added to the window and not the page, a shortcut printed in one and not the
//! other, a word translated twice into two different words. The columns went
//! this way for the same reason and have not drifted since — see [`COLUMNS`].
//!
//! [`COLUMNS`]: crate::COLUMNS
//!
//! ## The rule this table is shaped around
//!
//! **What a menu offers should be safe to press.** The browser face wrote that
//! down before there was anything unsafe in it, and the sentence is what makes
//! a delete admissible at all: [`Weight::Careful`] items are reversible, and
//! the only irreversible thing this program can do is not here. Permanent
//! deletion is reached by holding a modifier, which is a decision rather than a
//! slip, and it asks.
//!
//! ## What is only here because there is an index
//!
//! The middle group of a file's menu — search this folder, the same kind, find
//! its duplicates — is what a file manager cannot offer. Not because nobody
//! thought of it: because answering any of them means walking a disk, and by
//! the time it answered the person would have typed it themselves. Here they
//! are the index being asked a different question about a row already on
//! screen, and they cost what a search costs.

/// How dangerous an item is, which is the only thing a face has to render
/// differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Weight {
    /// Press it and find out; nothing is lost.
    Plain,
    /// Reversible, but it changes something. Drawn apart, and in the colour a
    /// face uses for warnings.
    Careful,
    /// It will start more programs, or take long enough to notice. Drawn
    /// dimmed, and it asks before it acts — the trailing `…` says so.
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
    /// The English source string, and therefore the catalogue key.
    ///
    /// `{n}` is the number of rows selected, punctuated by whoever draws it —
    /// the catalogue is the caller's and so is the thousands separator.
    pub msgid: &'static str,
    /// The shortcut to print at the right, already in the notation a person
    /// reads. Empty where there is none.
    pub key: &'static str,
    /// Items with the same number sit together; a rule is drawn where the
    /// number changes. Not a count of anything — only an ordering.
    pub group: u8,
    pub weight: Weight,
    pub when: When,
    /// Faces that cannot perform this one.
    ///
    /// **Written beside the item because the reason is a fact about the
    /// platform, not a gap somebody will close.** A browser cannot put a file
    /// on the clipboard — it can put text there, and an image's bytes, and
    /// nothing that another program will paste as a file. Leaving the item in
    /// the page greyed would promise something that is never coming; leaving
    /// the whole item out of the table would take it from the window and the
    /// terminal, which can.
    pub except: &'static [crate::faces::Face],
}

/// The whole menu, in the order it is drawn.
///
/// **Order is meaning here.** The first group is what most presses are for, so
/// it is under the pointer when the menu opens. The last thing before the
/// wastebasket is a rule, so that a hand travelling down the list stops at a
/// boundary rather than arriving at the delete by momentum.
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

/// The menu for one shape of selection, in order.
///
/// `selected` is how many rows are picked and `is_dir` describes the row the
/// pointer is on — which only matters when there is one.
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

/// Where a rule goes: true when this item starts a new group.
///
/// Given to the faces rather than left to each of them, because "draw a line
/// when the number changes" written in three places is three chances to draw
/// a line above the first item.
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
        // And the one thing that changes the disk says where it went, so that
        // a reader knows it can be got back.
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
                    // Same action, different wording for a folder or a
                    // selection — allowed, and the reason ids repeat at all.
                    // What is not allowed is the same id in the same shape.
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
            // Groups only ever go forwards, so a rule is never drawn twice
            // between the same pair.
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
