use super::super::machine::{HOME, SYSTEM, machine};
use super::*;

fn caps(q: &str, caret: Option<usize>) -> Vec<PlaceCap> {
    place_caps(q, HOME, &machine(), caret)
}

/// Each part as its tile's name, or its crumbs joined, `[raw]` when raw.
fn said(cap: &PlaceCap) -> Vec<String> {
    cap.parts
        .iter()
        .map(|p| match (p.raw, p.spot.tile) {
            (true, _) => format!("[{}]", p.spot.terms.len()),
            (_, Some(_)) => p.spot.name.clone(),
            _ => p
                .crumbs
                .iter()
                .map(|c| c.label.as_str())
                .collect::<Vec<_>>()
                .join(" › "),
        })
        .collect()
}

#[test]
fn the_system_disk_is_one_part_of_one_capsule() {
    let q = format!("rapor {SYSTEM} ext:pdf");
    let c = caps(&q, None);
    assert_eq!(c.len(), 1);
    assert_eq!(&q[c[0].start..c[0].end], SYSTEM);
    assert_eq!(said(&c[0]), ["CACHY"]);
    assert_eq!(c[0].parts[0].spot.kind, Some("system"));
    assert_eq!(c[0].parts[0].spot.terms.len(), 5);
}

#[test]
fn folders_are_crumbs_in_tilde_form() {
    let q = r#"under:~/Projeler/Scour|under:"/mnt/depo/My Docs"|under:/ rapor"#;
    let c = &caps(q, None)[0];
    assert_eq!(
        said(c),
        ["~ › Projeler › Scour", "/ › mnt › depo › My Docs", "/"]
    );
    let last = &c.parts[0].crumbs[2];
    assert_eq!(
        (last.label.as_str(), last.path.as_str()),
        ("Scour", "/home/hasan/Projeler/Scour")
    );
    assert_eq!(c.parts[0].crumbs[0].path, HOME);
}

#[test]
fn a_mixed_or_negated_run_is_not_one_capsule() {
    assert!(caps("under:/a|ext:pdf", None).is_empty());
    assert!(caps("!under:/a|under:/b", None).is_empty());
    assert!(caps("under:/a|parent:/b", None).is_empty());
    let c = caps("under:/a !under:/b under:/mnt/depo|under:/c", None);
    let spans: Vec<(usize, usize)> = c.iter().map(|c| (c.start, c.end)).collect();
    assert_eq!(spans, [(0, 8), (19, 43)]);
    assert_eq!(said(&c[1]), ["Depo", "/ › c"]);
}

#[test]
fn the_member_the_caret_is_in_is_shown_raw() {
    let q = format!("{SYSTEM}|under:/mnt/depo");
    let tilde = q.find("~").unwrap() + 1;
    let c = &caps(&q, Some(tilde))[0];
    assert_eq!(said(c), ["CACHY", "[1]", "Depo"]);
    assert!(c.parts[1].caret && !c.parts[0].caret);
    assert_eq!(c.parts[0].spot.terms.len(), 4);
    // Editing Depo's only root leaves no Depo part behind.
    let c = &caps(&q, Some(q.len()))[0];
    assert_eq!(said(c), ["CACHY", "[1]"]);
    // Before the run's first letter, the caret is in none of it.
    assert_eq!(said(&caps(&q, Some(0))[0]), ["CACHY", "Depo"]);
}

#[test]
fn an_empty_member_is_raw() {
    let c = &caps("rapor under:/mnt/depo|under:", None)[0];
    assert_eq!(said(c), ["Depo", "[1]"]);
    assert!(c.parts[1].crumbs.is_empty());
}

#[test]
fn every_offset_of_the_run_has_a_part() {
    let q = format!("rapor {SYSTEM}|under:/mnt/depo|under:~/x");
    let c = &caps(&q, None)[0];
    assert_eq!(said(c), ["CACHY", "Depo", "~ › x"]);
    assert_eq!(c.part_at(c.start - 1), None);
    assert_eq!(c.part_at(c.end + 1), None);
    assert_eq!(c.part_at(c.start), Some(0));
    let depo = q.find("under:/mnt").unwrap();
    assert_eq!(
        c.part_at(depo),
        Some(0),
        "the `|` before Depo is drawn after CACHY"
    );
    assert_eq!(c.part_at(depo + 1), Some(1));
    assert_eq!(c.part_at(c.end), Some(2));
    // An offset drawn in a part, once the caret is put there, is in that part's term.
    for at in c.start + 1..=c.end {
        let k = c.part_at(at).unwrap();
        let again = &caps(&q, Some(at))[0];
        if let Some(raw) = again.parts.iter().find(|p| p.caret) {
            let t = &raw.spot.terms[0];
            assert!(t.start < at && at <= t.end);
            assert!(c.parts[k].spot.terms.contains(t), "{at}");
        }
    }
}

#[test]
fn folding_keeps_the_caret_part_and_counts_the_rest() {
    let q = "under:/a|under:/b|under:/c|under:/d|under:/e";
    let mut c = caps(q, None).remove(0);
    c.fold(2);
    let shown: Vec<bool> = c.parts.iter().map(|p| p.shown).collect();
    assert_eq!(
        (shown, c.hidden),
        (vec![true, true, false, false, false], 3)
    );
    let mut c = caps(q, Some(q.len())).remove(0);
    c.fold(2);
    let shown: Vec<bool> = c.parts.iter().map(|p| p.shown).collect();
    assert_eq!(
        (shown, c.hidden),
        (vec![true, false, false, false, true], 3)
    );
    c.fold(9);
    assert_eq!(c.hidden, 0);
    c.fold(0);
    assert_eq!(c.hidden, 4);
}
