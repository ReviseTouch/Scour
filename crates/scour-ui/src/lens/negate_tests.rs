use super::super::machine::{HOME, SYSTEM, machine};
use super::super::{Caret, exclusion_caps};
use super::*;

const NONE_OF: &str = "!under:/etc !under:~ !under:/opt !under:/usr !under:/var !under:/mnt/depo";

#[test]
fn a_group_turned_round_is_one_exclusion_each_and_back() {
    let q = format!("rapor {SYSTEM}|under:/mnt/depo ext:pdf");
    let (out, at) = negate_places(&q, 10);
    assert_eq!(out, format!("rapor {NONE_OF} ext:pdf"));
    assert_eq!(&out[..at], format!("rapor {NONE_OF}"));
    let (back, at) = unnegate_places(&out, out.len() - 9);
    assert_eq!(back, q);
    assert_eq!(&back[..at], format!("rapor {SYSTEM}|under:/mnt/depo"));
}

#[test]
fn quotes_spaces_and_empty_members() {
    let q = r#"under:"/mnt/depo/My Docs" | under:~"#;
    let (out, _) = negate_places(q, 0);
    assert_eq!(out, r#"!under:"/mnt/depo/My Docs" !under:~"#);
    assert_eq!(
        unnegate_places(&out, 0).0,
        r#"under:"/mnt/depo/My Docs"|under:~"#
    );
    assert_eq!(negate_places("under:/a|under:", 0).0, "!under:/a");
    // Nothing of the kind at the caret: left as it is.
    assert_eq!(
        negate_places("rapor under:/a", 2),
        ("rapor under:/a".into(), 2)
    );
    assert_eq!(negate_places("under:/a|ext:pdf", 0).0, "under:/a|ext:pdf");
    assert_eq!(
        unnegate_places("!under:/a|!under:/b", 0).0,
        "!under:/a|!under:/b"
    );
}

#[test]
fn exclusions_side_by_side_are_one_block() {
    let spans = |q: &str| -> Vec<Vec<usize>> {
        exclusion_blocks(q)
            .iter()
            .map(|b| b.iter().map(|t| t.start).collect())
            .collect()
    };
    assert_eq!(
        spans("!under:/a !under:/b rapor !under:/c"),
        [vec![0, 10], vec![26]]
    );
    assert!(spans("!under:/a|under:/b").is_empty());
    assert_eq!(spans("!under:/a !ext:pdf !under:/b"), [vec![0], vec![19]]);
}

#[test]
fn an_excluded_disk_is_one_part_of_one_negated_capsule() {
    let q = format!("rapor {NONE_OF} !under:/tmp");
    let caps = exclusion_caps(&q, HOME, &machine(), None);
    assert_eq!(caps.len(), 1);
    let names: Vec<&str> = caps[0].parts.iter().map(|p| p.spot.name.as_str()).collect();
    assert_eq!(names, ["CACHY", "Depo", "/tmp"]);
    assert!(caps[0].parts.iter().all(|p| p.spot.not));
    assert_eq!(
        &q[caps[0].start..caps[0].end],
        format!("{NONE_OF} !under:/tmp")
    );
    // Editing one root takes its disk apart, as in a group.
    let tilde = q.find('~').unwrap() + 1;
    let caps = exclusion_caps(&q, HOME, &machine(), Some(Caret::editing(tilde)));
    assert_eq!(caps[0].parts.len(), 7);
    assert!(caps[0].parts[1].raw);
}
