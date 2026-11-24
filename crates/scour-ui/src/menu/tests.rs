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
