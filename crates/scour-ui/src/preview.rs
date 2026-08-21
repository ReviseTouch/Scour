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
}
