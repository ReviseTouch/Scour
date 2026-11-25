use super::*;

fn term(q: &str, at: usize) -> Term {
    lens::term_at(q, at).unwrap()
}

#[test]
fn a_capsule_is_counted_without_every_term_drawn_in_it_and_their_bars() {
    let q = "rapor under:/etc|under:~|under:/mnt/depo ext:pdf";
    // The place group: its whole run.
    assert_eq!(without(q, 6, 40), "rapor ext:pdf");
    // A single term, and a member of an OR run with its `|`.
    assert_eq!(
        without(q, 41, 48),
        "rapor under:/etc|under:~|under:/mnt/depo"
    );
    let or = "rapor ext:pdf|dm:today size:>1mb";
    assert_eq!(without(or, 6, 13), "rapor dm:today size:>1mb");
}

#[test]
fn only_a_filter_with_something_in_it_or_a_group_is_counted() {
    let q = "rapor ext:pdf kind: under:/a|under:/b";
    assert!(!counted(q, &term(q, 2)), "a plain word");
    assert!(counted(q, &term(q, 9)));
    assert!(!counted(q, &term(q, 18)), "an empty filter");
    let group = Term {
        start: 20,
        end: q.len(),
        not: false,
        field: Some("under"),
        value: String::new(),
    };
    assert!(counted(q, &group));
    let reads = "rapor ext:pdf content:fatura";
    assert!(!counted(reads, &term(reads, 9)), "the query reads files");
}

#[test]
fn a_rescue_marks_the_capsules_whose_terms_it_names_and_that_find_something() {
    let q = "rapor ext:xlsx dm:today kind:doc";
    let rescue = vec![
        ("ext:xlsx".to_string(), String::new(), Some((17, false))),
        ("dm:today".to_string(), String::new(), Some((23, false))),
        ("kind:doc".to_string(), String::new(), Some((0, false))),
    ];
    let at = |s: &str| q.find(s).unwrap();
    let mark = |s: &str| rescuer(q, at(s), at(s) + s.len(), &rescue);
    assert_eq!(mark("ext:xlsx"), Some((17, false)));
    assert_eq!(mark("dm:today"), Some((23, false)));
    assert_eq!(mark("kind:doc"), None, "leaving it out finds nothing");
    assert_eq!(mark("rapor"), None, "not asked");
}

#[test]
fn a_run_joined_by_a_bar_is_framed_unless_one_capsule_holds_it() {
    let q = "rapor|fatura ext:pdf|dm:today under:/a|under:/b";
    let group = (q.find("under:/a").unwrap(), q.len());
    let runs = or_runs(q, &[group]);
    let said: Vec<&str> = runs.iter().map(|&(a, b)| &q[a..b]).collect();
    assert_eq!(said, ["rapor|fatura", "ext:pdf|dm:today"]);
    assert!(or_runs("rapor ext:pdf", &[]).is_empty());
}

#[test]
fn a_rescue_answer_lands_only_in_the_list_it_was_asked_for() {
    let asked = (7, "rapor ext:xlsx");
    assert!(current(asked, 7, "rapor ext:xlsx"));
    assert!(
        !current(asked, 6, "rapor ext:xlsx"),
        "asked for an older query"
    );
    assert!(!current(asked, 7, "rapor ext:xls"), "the query moved on");
}
