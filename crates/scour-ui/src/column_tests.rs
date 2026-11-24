/// The columns as they were the default until the kind moved beside the
/// name: the layout is tested with a fixed column between two that grow.
const FIVE: &[&str] = &["name", "kind", "path", "mtime", "size"];

use super::*;

/// Every default is a column that exists: a typo starts a window with four
/// columns and no complaint.
#[test]
fn the_defaults_all_name_real_columns() {
    for id in FIVE {
        assert!(column(id).is_some(), "`{id}` is not a column");
    }
}

/// Every column can be drawn: a floor above the width it asks for would
/// mean a column that starts out already squeezed.
#[test]
fn no_floor_is_above_the_width_it_guards() {
    for c in COLUMNS {
        assert!(
            c.min > 0,
            "`{}` has no floor and can be squeezed away",
            c.id
        );
        assert!(
            c.min <= c.width,
            "`{}`: floor {} over width {}",
            c.id,
            c.min,
            c.width
        );
        if c.max > 0 {
            assert!(c.max >= c.width, "`{}`: ceiling below its own width", c.id);
            assert!(c.near > 0, "`{}` has a ceiling it can never reach", c.id);
        }
        // A share at both ends or neither: one of the two left at zero
        // collapses the column to its floor at that end of the range.
        assert_eq!(
            c.near == 0,
            c.far == 0,
            "`{}` stretches at one end of the range only",
            c.id
        );
    }
    // Percentages of one row, and they must read as such at both ends.
    for word in ["near", "far"] {
        let total: u32 = FIVE
            .iter()
            .filter_map(|id| column(id))
            .map(|c| u32::from(if word == "near" { c.near } else { c.far }))
            .sum();
        assert_eq!(total, 100, "the {word} shares add up to {total}%, not 100");
    }
    // Something has to be able to absorb, or no window ever fits exactly.
    assert!(
        COLUMNS.iter().any(|c| c.near > 0 && c.max == 0),
        "nothing can take up the slack"
    );
}

/// The invariant: the columns add up to the room, whatever the window does.
#[test]
fn the_widths_add_up_to_the_room_at_every_size() {
    for avail in (200..3600).step_by(7) {
        let w = lay_out(FIVE, |_| None, avail);
        assert_eq!(
            w.iter().sum::<u32>(),
            avail,
            "{avail}px: {w:?} does not add up"
        );
    }
}

/// A name dragged wide must not push the date and the size off the edge.
#[test]
fn a_name_dragged_far_too_wide_does_not_push_the_date_and_size_off() {
    let avail = 900;
    let w = lay_out(FIVE, |id| (id == "name").then_some(4000), avail);
    assert_eq!(w.iter().sum::<u32>(), avail);
    for (c, got) in COLUMNS.iter().filter(|c| FIVE.contains(&c.id)).zip(&w) {
        assert!(*got >= c.min, "`{}` fell to {got}, under its floor", c.id);
    }
    // And the two on the right are still readable rather than a sliver.
    let mtime = w[FIVE.iter().position(|i| *i == "mtime").unwrap()];
    let size = w[FIVE.iter().position(|i| *i == "size").unwrap()];
    assert!(mtime >= column("mtime").unwrap().min);
    assert!(size >= column("size").unwrap().min);
}

/// A wide screen is the location's, not the name's.
#[test]
fn the_name_stops_at_its_ceiling_and_the_location_takes_the_rest() {
    let w = lay_out(FIVE, |_| None, 2400);
    let at = |id: &str| w[FIVE.iter().position(|i| *i == id).unwrap()];
    assert_eq!(at("name"), column("name").unwrap().max, "the name ran on");
    assert!(
        at("path") > at("name"),
        "a path elides worse than a name and got less room: {} vs {}",
        at("path"),
        at("name")
    );
    // The columns that never grow are exactly the width they asked for.
    for id in ["kind", "mtime", "size"] {
        assert_eq!(at(id), column(id).unwrap().width, "`{id}` grew");
    }
}

/// A drag is honoured exactly: past the name's ceiling, past half the
/// room, and whatever it is beside. The location pays for it first.
#[test]
fn a_dragged_column_is_as_wide_as_it_was_dragged() {
    let at = |w: &[u32], id: &str| w[FIVE.iter().position(|i| *i == id).unwrap()];
    let free = lay_out(FIVE, |_| None, 1100);

    // Wider than its automatic ceiling and than any share it would get.
    let w = lay_out(FIVE, |id| (id == "name").then_some(640), 1100);
    assert_eq!(at(&w, "name"), 640, "the drag was not honoured: {w:?}");
    assert_eq!(w.iter().sum::<u32>(), 1100);
    // The location gave the room; the fixed columns did not move.
    assert!(at(&w, "path") < at(&free, "path"));
    for id in ["kind", "mtime", "size"] {
        assert_eq!(at(&w, id), at(&free, id), "`{id}` paid for the name");
    }

    // Narrower than it would have been is honoured too, and the location
    // takes the room.
    let w = lay_out(FIVE, |id| (id == "name").then_some(150), 1100);
    assert_eq!(at(&w, "name"), 150);
    assert!(at(&w, "path") > at(&free, "path"));

    // A fixed column and a stretching one dragged at once: both kept.
    let w = lay_out(
        FIVE,
        |id| match id {
            "name" => Some(500),
            "kind" => Some(200),
            _ => None,
        },
        1300,
    );
    assert_eq!((at(&w, "name"), at(&w, "kind")), (500, 200), "{w:?}");
    assert_eq!(w.iter().sum::<u32>(), 1300);
}

/// A drag stops where the rest are on their floors: the location goes
/// first, then the dragged column itself, and the sum is the room at
/// every size.
#[test]
fn a_drag_past_the_room_stops_at_the_floors() {
    let at = |w: &[u32], id: &str| w[FIVE.iter().position(|i| *i == id).unwrap()];
    let pinned = |id: &str| (id == "name").then_some(800);
    for avail in (400..3000).step_by(13) {
        let w = lay_out(FIVE, pinned, avail);
        assert_eq!(w.iter().sum::<u32>(), avail, "at {avail}px: {w:?}");
        assert!(at(&w, "name") <= 800, "a drag grew at {avail}px");
        // While the name is short of what was asked, the location is on
        // its floor: the dragged column is the second to give way.
        if at(&w, "name") < 800 && avail >= floor_width(FIVE) {
            assert_eq!(
                at(&w, "path"),
                column("path").unwrap().min,
                "at {avail}px: {w:?}"
            );
        }
    }
}

/// Every stretching column dragged: the row is still filled to the edge.
#[test]
fn with_every_stretching_column_dragged_the_row_is_still_full() {
    let w = lay_out(
        FIVE,
        |id| match id {
            "name" => Some(300),
            "path" => Some(300),
            _ => None,
        },
        1800,
    );
    assert_eq!(w.iter().sum::<u32>(), 1800, "{w:?}");
}

/// What the sliding shares are for: the two text columns trade places as
/// the window grows, and they do it without a step.
#[test]
fn the_name_leads_in_a_narrow_window_and_the_location_in_a_wide_one() {
    let at = |w: &[u32], id: &str| w[FIVE.iter().position(|i| *i == id).unwrap()];

    let tight = lay_out(FIVE, |_| None, NARROW);
    assert!(
        at(&tight, "name") >= at(&tight, "path"),
        "narrow: name {} is not leading the location {}",
        at(&tight, "name"),
        at(&tight, "path")
    );
    let roomy = lay_out(FIVE, |_| None, 2400);
    assert!(
        at(&roomy, "path") > at(&roomy, "name") * 2,
        "wide: the location {} has not overtaken the name {}",
        at(&roomy, "path"),
        at(&roomy, "name")
    );

    // No step anywhere between: one pixel of window is a pixel or two of
    // column. The 4 is integer rounding on a share carried in parts per
    // thousand, not a threshold.
    let mut last = lay_out(FIVE, |_| None, 500);
    for avail in 501..3000 {
        let now = lay_out(FIVE, |_| None, avail);
        for (i, id) in FIVE.iter().enumerate() {
            let step = now[i].abs_diff(last[i]);
            assert!(step <= 4, "`{id}` jumped {step}px at {avail}px");
        }
        last = now;
    }
}

/// What a person dragged is kept, and never quietly grown back.
#[test]
fn a_dragged_width_is_not_a_hole_to_pour_space_into() {
    let w = lay_out(FIVE, |id| (id == "name").then_some(160), 2400);
    assert_eq!(w[0], 160, "the name was grown past what was chosen");
    assert_eq!(w.iter().sum::<u32>(), 2400);
    // A remembered width from a narrower window is pulled up to the floor.
    let w = lay_out(FIVE, |id| (id == "size").then_some(3), 1400);
    let size = w[FIVE.iter().position(|i| *i == "size").unwrap()];
    assert_eq!(size, column("size").unwrap().min);
}

/// Below every floor at once, they all give way together.
#[test]
fn too_narrow_for_the_floors_cramps_them_rather_than_losing_any() {
    let floor = floor_width(FIVE);
    for avail in 10..floor {
        let w = lay_out(FIVE, |_| None, avail);
        assert_eq!(w.iter().sum::<u32>(), avail, "at {avail}px: {w:?}");
        assert!(w.iter().all(|x| *x > 0), "a column vanished at {avail}px");
        // Still in the same proportions: the location is the widest.
        assert_eq!(
            w.iter().position(|x| *x == *w.iter().max().unwrap()),
            Some(2),
            "at {avail}px the location stopped being the widest: {w:?}"
        );
    }
}

/// Short names under long locations: the room beside the names goes to
/// the locations, and only as much as they need.
#[test]
fn short_names_hand_their_room_to_long_locations() {
    let at = |id| FIVE.iter().position(|i| *i == id).unwrap();
    let mut w = lay_out(FIVE, |_| None, 1100);
    let before = w.clone();
    let need = |id: &str| match id {
        "name" => Some(120),
        "path" => Some(before[at("path")] + 150),
        _ => None,
    };
    fit_to(FIVE, |_| None, need, &mut w);
    assert_eq!(w.iter().sum::<u32>(), 1100);
    assert_eq!(
        w[at("path")],
        before[at("path")] + 150,
        "{before:?} → {w:?}"
    );
    assert_eq!(w[at("name")], before[at("name")] - 150);
    // The fixed columns are not touched.
    assert_eq!(w[at("mtime")], before[at("mtime")]);
}

/// The other way round, and never under the name's own floor.
#[test]
fn long_names_take_from_short_locations_down_to_the_floor() {
    let at = |id| FIVE.iter().position(|i| *i == id).unwrap();
    let mut w = lay_out(FIVE, |_| None, 1000);
    let before = w.clone();
    let need = |id: &str| match id {
        "name" => Some(5000),
        "path" => Some(10),
        _ => None,
    };
    fit_to(FIVE, |_| None, need, &mut w);
    assert_eq!(w.iter().sum::<u32>(), 1000);
    assert!(w[at("path")] >= column("path").unwrap().min);
    assert!(w[at("path")] < before[at("path")], "{before:?} → {w:?}");
    // Up to its ceiling and no further, or until the location is on its floor.
    let name = column("name").unwrap();
    assert!(
        w[at("name")] == name.max.max(before[at("name")])
            || w[at("path")] == column("path").unwrap().min,
        "{before:?} → {w:?}"
    );
}

/// A drag is kept exactly, whatever the rows hold.
#[test]
fn a_dragged_column_neither_gives_nor_takes() {
    let at = |id| FIVE.iter().position(|i| *i == id).unwrap();
    let chosen = |id: &str| (id == "name").then_some(600);
    let mut w = lay_out(FIVE, chosen, 1300);
    let before = w.clone();
    fit_to(
        FIVE,
        chosen,
        |id| Some(if id == "path" { 2000 } else { 40 }),
        &mut w,
    );
    assert_eq!(w, before);
    assert_eq!(w[at("name")], 600);
}

/// Whatever is measured, the total stays the room.
#[test]
fn fitting_keeps_the_total_at_every_size() {
    for avail in (200..3000).step_by(37) {
        for (n, p) in [(10, 3000), (3000, 10), (300, 300), (0, 0), (700, 1200)] {
            let mut w = lay_out(FIVE, |_| None, avail);
            let need = |id: &str| match id {
                "name" => Some(n),
                "path" => Some(p),
                _ => None,
            };
            fit_to(FIVE, |_| None, need, &mut w);
            assert_eq!(w.iter().sum::<u32>(), avail, "{avail}px {n}/{p}: {w:?}");
        }
    }
}

/// Ids are unique, because the settings file keys widths by them.
#[test]
fn no_id_is_used_twice() {
    let mut seen: Vec<&str> = Vec::new();
    for c in COLUMNS {
        assert!(!seen.contains(&c.id), "`{}` appears twice", c.id);
        seen.push(c.id);
    }
}

/// Numbers are the ones that right-align, and only those — a rule the type
/// cannot check.
#[test]
fn only_the_sizes_are_right_aligned() {
    for c in COLUMNS {
        let numeric = c.id == "size" || c.id == "disk";
        assert_eq!(
            c.align == Align::End,
            numeric,
            "`{}` is aligned the wrong way",
            c.id
        );
    }
}
