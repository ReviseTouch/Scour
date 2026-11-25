use super::*;
use scour_ui::disks::{DiskIn, SourceIn, Tile, tiles};
use scour_ui::lens::place_caps;

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
    assert_eq!(all.cap.hidden, 0);
    let d = fit(q, cap, all.width - 1.0, HEAD_W, 10.0, &mut m).unwrap();
    assert!(d.width < all.width);
    assert!(d.cap.hidden > 0);
    assert!(
        d.bits
            .iter()
            .any(|b| b.text == format!("+{}", d.cap.hidden))
    );
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
