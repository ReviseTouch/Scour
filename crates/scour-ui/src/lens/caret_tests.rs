use super::super::machine::{HOME, SYSTEM, machine};
use super::super::{place_caps_at, terms};
use super::*;

fn cap(q: &str, caret: Option<Caret>) -> PlaceCap {
    place_caps_at(q, HOME, &machine(), caret).remove(0)
}

fn walk(c: &PlaceCap, q: &str, forward: bool) -> Vec<usize> {
    let mut at = if forward { c.start } else { c.end };
    let mut out = vec![at];
    while let Some(next) = c.step(q, at, forward) {
        out.push(next);
        at = next;
    }
    out
}

#[test]
fn the_arrows_cross_a_collapsed_disk_in_one_step() {
    let q = format!("rapor {SYSTEM}|under:/mnt/depo ext:pdf");
    let c = cap(&q, Some(Caret::resting(6 + "under:/etc".len())));
    let var_end = q.find("|under:/mnt").unwrap();
    assert_eq!(walk(&c, &q, true), [6, var_end, c.end]);
    assert_eq!(walk(&c, &q, false), [c.end, var_end, 6]);
    assert_eq!(
        c.step(&q, c.end, true),
        None,
        "→ at the end leaves the capsule"
    );
    assert_eq!(c.step(&q, 3, true), None, "outside it, the face moves");
}

#[test]
fn a_block_of_hidden_parts_is_one_stop() {
    let q = "under:/a|under:/b|under:/c|under:/d|under:/e";
    let ends: Vec<usize> = terms(q).iter().map(|t| t.end).collect();
    let mut c = cap(q, None);
    c.fold(2);
    assert_eq!(c.blocks(), [(2, 3)]);
    assert_eq!(walk(&c, q, true), [0, ends[0], ends[1], ends[4]]);
    // The caret's part kept: two blocks, each one stop.
    let mut c = cap(q, Some(Caret::resting(ends[2])));
    c.fold(2);
    assert_eq!(c.blocks(), [(1, 1), (3, 2)]);
    assert_eq!(walk(&c, q, true), [0, ends[0], ends[1], ends[2], ends[4]]);
}

#[test]
fn a_raw_part_stops_at_every_letter_and_never_inside_a_collapsed_one() {
    let q = "under:/mnt/depo|under:~/Müzik";
    let c = cap(q, Some(Caret::editing(q.len())));
    let raw = terms(q)[1].clone();
    let stops = c.stops(q);
    let letters: Vec<usize> = (raw.start..=raw.end)
        .filter(|&o| q.is_char_boundary(o))
        .collect();
    assert_eq!(stops[..2], [0, "under:/mnt/depo".len()]);
    assert_eq!(stops[2..], letters[..]);
    assert!(!stops.contains(&3), "not inside Depo's letters");
    let u = q.find('ü').unwrap();
    assert_eq!(
        c.step(q, u, true),
        Some(u + 'ü'.len_utf8()),
        "no stop inside a letter"
    );
}

#[test]
fn a_lens_opens_by_itself_only_where_the_person_edits() {
    let rests = |q: &str| opens_by_itself(q, Caret::resting(q.len()));
    let types = |q: &str| opens_by_itself(q, Caret::editing(q.len()));
    let q = "rapor under:~/Belgeler";
    assert!(!rests(q) && types(q));
    assert!(opens_by_itself(q, Caret::resting(q.len() - 1)), "inside it");
    assert!(rests("rapor under:"), "nothing chosen yet");
    assert!(!rests(SYSTEM) && !rests("rapor !under:/tmp"));
    assert!(types("rapor !under:/tmp"));
    // Not a capsule: as before.
    assert!(opens_by_itself("!under:/a|under:/b", Caret::resting(9)));
    assert!(opens_by_itself("under:/a|ext:pdf", Caret::resting(8)));
    assert!(rests("rapor dm:7d") && !rests("rapor"));
}

#[test]
fn the_edited_term_is_said_once() {
    let q = "rapor under:~/Bel";
    assert_eq!(
        edited_term(q, Caret::editing(q.len())),
        Some(terms(q)[1].clone())
    );
    assert_eq!(edited_term(q, Caret::resting(q.len())), None);
    assert_eq!(edited_term(q, Caret::resting(14)).map(|t| t.start), Some(6));
    assert!(edited_term("under:", Caret::resting(6)).is_some());
    assert_eq!(
        edited_term(q, Caret::editing(6)),
        None,
        "before its first letter"
    );
}
