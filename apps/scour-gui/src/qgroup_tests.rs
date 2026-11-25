use super::*;
use scour_ui::disks::{DiskIn, SourceIn, Tile, tiles};
use scour_ui::lens::{Caret, place_caps};

const HOME: &str = "/home/hasan";
const SYSTEM: &str = "under:/etc|under:~|under:/opt|under:/usr|under:/var";

fn machine() -> Vec<Tile> {
    let owned = |r: &[&str]| r.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let (slash, depo) = (owned(&["/"]), owned(&["/mnt/depo"]));
    let disk = |id, label, kind, roots, state| DiskIn {
        id,
        label,
        kind,
        fs: "fs",
        roots,
        size: 1,
        state,
        live: false,
        walking: false,
    };
    let disks = [
        disk("cachy", "CACHY", "system", &slash, "manual"),
        disk("depo", "Depo", "fixed", &depo, "configured"),
    ];
    let (home, sistem) = (owned(&[HOME]), owned(&["/usr", "/etc", "/opt", "/var"]));
    let sources = [
        SourceIn {
            name: "home",
            roots: &home,
        },
        SourceIn {
            name: "sistem",
            roots: &sistem,
        },
        SourceIn {
            name: "depo",
            roots: &depo,
        },
    ];
    tiles(&disks, &sources, |_| None)
}

fn caps(q: &str, caret: Option<usize>) -> Vec<PlaceCap> {
    place_caps(q, HOME, &machine(), caret)
}

/// Eight pixels a letter, whatever the face.
fn m(s: &str, _: i32) -> f32 {
    s.chars().count() as f32 * 8.0
}

const HEAD_W: f32 = 80.0;

#[test]
fn the_system_disk_is_one_chip_and_every_offset_is_at_its_edges() {
    let q = format!("rapor {SYSTEM} ext:pdf");
    let cap = &caps(&q, None)[0];
    let d = fit(&q, cap, 1000.0, HEAD_W, 10.0, &mut m).unwrap();
    // The dot and the name in a chip, after the header.
    let chip = PAD + DOT + DOT_GAP + 5.0 * 8.0 + PAD;
    assert_eq!(d.width, HEAD_W + chip + END);
    let words: Vec<&str> = d
        .bits
        .iter()
        .map(|b| b.text.as_str())
        .filter(|t| !t.is_empty())
        .collect();
    assert_eq!(words, ["CACHY"]);
    assert_eq!(d.dx.len(), SYSTEM.len() + 1);
    assert_eq!(d.dx[0], 0.0);
    assert!(d.dx[1..].iter().all(|&x| x == HEAD_W + chip));
    assert_eq!(d.zones.len(), 1);
    assert_eq!(d.zones[0].2, Aim::At(cap.end));
}

#[test]
fn a_narrow_room_folds_the_rest_into_a_count() {
    let q = "under:~/a|under:~/b|under:~/c|under:~/d";
    let cap = &caps(q, None)[0];
    let all = fit(q, cap, 1000.0, HEAD_W, 10.0, &mut m).unwrap();
    assert_eq!(hidden(&all), 0);
    let d = fit(q, cap, all.width - 1.0, HEAD_W, 10.0, &mut m).unwrap();
    assert!(d.width < all.width);
    assert!(hidden(&d) > 0);
    // Too narrow for even one part: drawn as its terms.
    assert!(fit(q, cap, HEAD_W, HEAD_W, 10.0, &mut m).is_none());
}

#[test]
fn the_member_being_edited_is_letters_in_a_well() {
    let q = format!("{SYSTEM}|under:/mnt/depo|under:~/Projeler");
    let caret = q.len() - 2;
    let cap = &caps(&q, Some(caret))[0];
    let d = fit(&q, cap, 2000.0, HEAD_W, 10.0, &mut m).unwrap();
    let v = q.rfind("~/").unwrap();
    assert_eq!(d.letters, [(v, q.len())]);
    let well = d.bits.iter().find(|b| b.kind == 4).unwrap();
    let at = |o: usize| d.dx[o - cap.start];
    assert_eq!(at(v), well.x + WELL_PAD);
    assert_eq!(at(v + 3), well.x + WELL_PAD + 30.0);
    // Offsets before it are at Depo's trailing edge, never past the well.
    assert!(at(v - 7) <= well.x);
    let last = d.zones.last().unwrap();
    assert_eq!(last.2, Aim::Letters(v, q.len()));
}

#[test]
fn backspace_and_delete_at_an_edge_take_the_whole_part() {
    let q = format!("rapor {SYSTEM}|under:/mnt/depo x");
    let depo = q.find("under:/mnt").unwrap();
    let c = caps(&q, Some(depo));
    let (out, _) = backspace(&q, HOME, &c, depo).unwrap();
    assert_eq!(out, "rapor under:/mnt/depo x");
    let (out, _) = delete(&q, HOME, &c, depo).unwrap();
    assert_eq!(out, format!("rapor {SYSTEM} x"));
    // Among a member's letters the keys edit it.
    let end = depo + "under:/mnt/depo".len();
    let c = caps(&q, Some(end));
    assert!(backspace(&q, HOME, &c, end).is_none());
    // Delete at the group's first letter takes its first part.
    let c = caps(&q, Some(6));
    assert_eq!(
        delete(&q, HOME, &c, 6).unwrap().0,
        "rapor under:/mnt/depo x"
    );
}

/// CACHY · Depo · a folder, the caret resting at its end after a press.
fn three() -> String {
    format!("rapor {SYSTEM}|under:/mnt/depo|under:~/Belgeler/rapor ext:pdf")
}

#[test]
fn a_caret_resting_at_the_end_leaves_every_part_whole() {
    let q = three();
    let end = q.len() - " ext:pdf".len();
    let caps = scour_ui::lens::place_caps_at(&q, HOME, &machine(), Some(Caret::resting(end)));
    let names: Vec<(&str, bool)> = caps[0]
        .parts
        .iter()
        .map(|p| (p.spot.name.as_str(), p.raw))
        .collect();
    assert_eq!(names[..2], [("CACHY", false), ("Depo", false)]);
    assert!(!caps[0].parts[2].raw, "the folder too: nobody is typing");
    let editing = scour_ui::lens::place_caps_at(&q, HOME, &machine(), Some(Caret::editing(end)));
    assert!(editing[0].parts[2].raw, "typing at its end opens it");
}

#[test]
fn the_arrows_stop_once_a_part_and_never_touch_the_text() {
    let q = three();
    let before = q.clone();
    let caps = scour_ui::lens::place_caps_at(&q, HOME, &machine(), None);
    let cap = &caps[0];
    let mut at = cap.start;
    let mut stops = vec![at];
    while let Some(next) = cap.step(&q, at, true) {
        at = next;
        stops.push(at);
    }
    // The start, then the trailing edge of CACHY, Depo and the folder.
    assert_eq!(stops.len(), 4);
    assert_eq!(*stops.last().unwrap(), cap.end);
    let back: Vec<usize> =
        std::iter::successors(Some(cap.end), |&a| cap.step(&q, a, false)).collect();
    assert_eq!(back.into_iter().rev().collect::<Vec<_>>(), stops);
    assert_eq!(q, before);
}

#[test]
fn each_block_of_hidden_parts_has_its_own_plus() {
    let q = "under:~/a|under:~/b|under:~/c|under:~/d|under:~/e";
    let mut cap = caps(q, None).remove(0);
    // The caret's part shown past the fold: two blocks hidden, either side.
    cap.parts[3].caret = true;
    cap.fold(2);
    let d = lay(q, cap.clone(), HEAD_W, 10.0, &mut m);
    let plus: Vec<&str> = d
        .bits
        .iter()
        .filter(|b| b.text.starts_with('+'))
        .map(|b| b.text.as_str())
        .collect();
    assert_eq!(cap.blocks().len(), 2);
    assert_eq!(plus, ["+2", "+1"]);
}

#[test]
fn the_pill_s_exclusion_turns_a_group_into_one_block_and_back() {
    let q = "rapor under:/mnt/depo|under:~/a ext:pdf";
    let t = |a: usize, b: usize, not: bool| Term {
        start: a,
        end: b,
        not,
        field: Some("under"),
        value: String::new(),
    };
    let (neg, end) = whole(q, &t(6, 31, false), "neg").unwrap();
    assert_eq!(neg, "rapor !under:/mnt/depo !under:~/a ext:pdf");
    let (back, _) = whole(&neg, &t(6, end, true), "neg").unwrap();
    assert_eq!(back, q);
}
