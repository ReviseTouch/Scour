use super::*;
use slint::Model;

#[test]
fn the_icon_spells_the_extension_and_only_one_that_fits() {
    assert_eq!(super::badge_of("tahliye.pdf"), "PDF");
    assert_eq!(super::badge_of("main.slint"), "SLINT");
    assert_eq!(super::badge_of("yedek.tar.gz"), "GZ");
    assert_eq!(super::badge_of(".bashrc"), "");
    assert_eq!(super::badge_of("Makefile"), "");
    assert_eq!(super::badge_of("notes.markdown"), "");
    assert_eq!(super::badge_of("a.b c"), "");
}

fn page(n: usize) -> Vec<Row> {
    (0..n).map(|_| Row::default()).collect()
}

fn named(paths: &[&str]) -> Vec<Row> {
    paths
        .iter()
        .map(|p| Row {
            path: (*p).into(),
            ..Row::default()
        })
        .collect()
}

#[test]
fn the_list_is_as_long_as_the_result_not_as_the_page() {
    let rows = Rows::default();
    rows.put(0, page(SPAN), Vec::new(), 2_500_000);
    assert_eq!(rows.row_count(), 2_500_000);
    assert_eq!(rows.held(), SPAN);
}

// The notifications actually delivered to Slint's model peer.
#[derive(Default)]
struct Notifications(RefCell<Vec<(char, usize, usize)>>);

impl slint::private_unstable_api::re_exports::ModelChangeListener for Notifications {
    fn row_changed(self: std::pin::Pin<&Self>, _: usize) {}
    fn row_added(self: std::pin::Pin<&Self>, at: usize, n: usize) {
        self.0.borrow_mut().push(('+', at, n));
    }
    fn row_removed(self: std::pin::Pin<&Self>, at: usize, n: usize) {
        self.0.borrow_mut().push(('-', at, n));
    }
    fn reset(self: std::pin::Pin<&Self>) {
        self.0.borrow_mut().push(('r', 0, 0));
    }
}

#[test]
fn first_population_is_lazy_but_later_growth_preserves_the_viewport() {
    use slint::private_unstable_api::re_exports::ModelChangeListenerContainer;
    let rows = std::rc::Rc::new(Rows::default());
    let lines = Lines::new(std::rc::Rc::clone(&rows));
    lines.per_line(4);
    let table = Box::pin(ModelChangeListenerContainer::<Notifications>::default());
    let grid = Box::pin(ModelChangeListenerContainer::<Notifications>::default());
    rows.model_tracker()
        .attach_peer(table.as_ref().model_peer());
    lines
        .model_tracker()
        .attach_peer(grid.as_ref().model_peer());
    rows.put(0, page(SPAN), Vec::new(), 5_000_000);
    lines.sync();
    assert_eq!(*table.as_ref().get().0.borrow(), [('r', 0, 0)]);
    assert_eq!(*grid.as_ref().get().0.borrow(), [('r', 0, 0)]);
    rows.set_total(5_000_004);
    lines.sync();
    assert_eq!(table.as_ref().get().0.borrow()[1], ('+', 5_000_000, 4));
    assert_eq!(grid.as_ref().get().0.borrow()[1], ('+', 1_250_000, 1));
    rows.set_total(5_000_000);
    lines.sync();
    assert_eq!(table.as_ref().get().0.borrow()[2], ('-', 5_000_000, 4));
    assert_eq!(grid.as_ref().get().0.borrow()[2], ('-', 1_250_000, 1));
    assert_eq!(rows.held(), SPAN);
}

// Which slots were told they changed.
#[derive(Default)]
struct Told(RefCell<Vec<usize>>);

impl slint::private_unstable_api::re_exports::ModelChangeListener for Told {
    fn row_changed(self: std::pin::Pin<&Self>, slot: usize) {
        self.0.borrow_mut().push(slot);
    }
    fn row_added(self: std::pin::Pin<&Self>, _: usize, _: usize) {}
    fn row_removed(self: std::pin::Pin<&Self>, _: usize, _: usize) {}
    fn reset(self: std::pin::Pin<&Self>) {}
}

fn distinct(n: usize) -> Vec<Row> {
    let paths: Vec<String> = (0..n).map(|i| format!("/r/{i}")).collect();
    named(&paths.iter().map(String::as_str).collect::<Vec<_>>())
}

/// The point of the slots: a step down hands one slot a new row and leaves
/// the rest alone, so nothing on screen is rebuilt.
#[test]
fn a_step_down_moves_one_slot_and_tells_no_other() {
    use slint::private_unstable_api::re_exports::ModelChangeListenerContainer;
    let rows = std::rc::Rc::new(Rows::default());
    rows.put(0, distinct(SPAN), Vec::new(), 10_000);
    let slots = Slots::new(&rows);
    slots.place(0, 10);
    assert_eq!(slots.row_count(), 10);
    for k in 0..10 {
        assert_eq!(slots.row_data(k).map(|s| s.i), Some(k as i32));
    }
    let told = Box::pin(ModelChangeListenerContainer::<Told>::default());
    slots
        .model_tracker()
        .attach_peer(told.as_ref().model_peer());
    slots.place(1, 10);
    assert_eq!(
        *told.as_ref().get().0.borrow(),
        [0],
        "row 10 went to slot 0"
    );
    let s = slots.row_data(0).expect("slot");
    assert_eq!((s.i, s.row.path.as_str()), (10, "/r/10"));
    assert_eq!(
        slots.row_data(1).map(|s| s.i),
        Some(1),
        "slot 1 kept its row"
    );
}

#[test]
fn a_changed_row_tells_the_slot_showing_it_and_slots_past_the_end_are_empty() {
    use slint::private_unstable_api::re_exports::ModelChangeListenerContainer;
    let rows = std::rc::Rc::new(Rows::default());
    rows.put(0, distinct(5), Vec::new(), 5);
    let slots = Slots::new(&rows);
    slots.place(0, 8);
    assert_eq!(slots.row_data(4).map(|s| s.i), Some(4));
    assert_eq!(slots.row_data(6).map(|s| s.i), Some(-1), "no row 6");
    let told = Box::pin(ModelChangeListenerContainer::<Told>::default());
    slots
        .model_tracker()
        .attach_peer(told.as_ref().model_peer());
    rows.mark_picked(&["/r/3".to_string()].into_iter().collect());
    assert_eq!(*told.as_ref().get().0.borrow(), [3]);
}

/// The first page of a question is cut to a screenful and later read whole:
/// the rows that brings are the rest of the page, not files that appeared.
#[test]
fn a_page_read_whole_after_a_cut_brings_no_arrivals() {
    let rows = Rows::default();
    rows.put(0, distinct(28), Vec::new(), 10_000);
    let arrived = rows.put(0, distinct(SPAN), Vec::new(), 10_000);
    assert!(!arrived, "nothing new appeared");
    assert_eq!(rows.row_data(100).map(|r| r.fresh), Some(false));
}

#[test]
fn a_page_landing_in_place_does_not_make_the_view_re_measure() {
    // The live refresh: same query, same length, new rows. A reset here
    // rebuilds the list and re-clamps the viewport.
    let rows = Rows::default();
    rows.put(0, page(SPAN), Vec::new(), 10_000);
    let after_first = rows.resets();
    rows.put(0, page(SPAN), Vec::new(), 10_000);
    rows.put(1, page(SPAN), Vec::new(), 10_000);
    assert_eq!(
        rows.resets(),
        after_first,
        "no reset while the length holds"
    );

    // And when the length really does change, the view has to be told.
    rows.put(2, page(SPAN), Vec::new(), 20_000);
    assert_eq!(rows.resets(), after_first + 1);
}

#[test]
fn the_exact_count_lengthens_the_list_it_does_not_reload_it() {
    // The search counts to its cap; the exact total follows a moment later.
    let rows = Rows::default();
    rows.put(0, page(SPAN), Vec::new(), 1_000);
    rows.set_total(2_481_902);
    assert_eq!(rows.row_count(), 2_481_902);
    assert_eq!(rows.held(), SPAN, "the page it was showing is intact");
    rows.set_total(2_481_902);
    assert_eq!(rows.resets(), 2, "and saying it twice costs nothing");
}

#[test]
fn a_row_outside_the_pages_in_hand_is_asked_for_once() {
    let rows = Rows::default();
    rows.put(0, page(SPAN), Vec::new(), 10_000);
    assert!(
        rows.row_data(SPAN + 44).is_some(),
        "drawn blank, not left a hole"
    );
    rows.row_data(SPAN + 45);
    assert_eq!(
        rows.wanted(),
        Some(SPAN + 44),
        "the first miss, not the last"
    );
    assert_eq!(rows.wanted(), None, "taken, so one fetch per miss");
}

#[test]
fn what_is_asked_for_is_where_the_eye_is_then_where_it_is_going() {
    let rows = Rows::default();
    rows.put(0, page(SPAN), Vec::new(), 10_000);
    // The screen is covered, so the next page is the one ahead of it.
    assert_eq!(rows.next_page(0, 24, true, true), Some(1));
    rows.asking(1, 0);
    assert_eq!(
        rows.next_page(0, 24, true, true),
        None,
        "and it is not asked for twice"
    );
    rows.put(1, page(SPAN), Vec::new(), 10_000);
    // Now ahead is held too, so nothing is wanted until the eye moves.
    assert_eq!(rows.next_page(0, 24, true, true), None);
    // Two pages down, what is on screen wins over what is beside it.
    assert_eq!(rows.next_page(SPAN * 3, SPAN * 3 + 24, true, true), Some(3));
    // At the top of the list there is nothing behind to fetch.
    rows.put(2, page(SPAN), Vec::new(), 10_000);
    rows.put(3, page(SPAN), Vec::new(), 10_000);
    assert_eq!(rows.next_page(0, 24, true, true), None);
}

#[test]
fn nothing_is_guessed_at_while_a_page_is_expensive() {
    // Deep in a result a page costs a walk of everything above it, so only
    // what is on screen is fetched.
    let rows = Rows::default();
    rows.put(9, page(SPAN), Vec::new(), 4_000_000);
    assert_eq!(rows.next_page(9 * SPAN, 9 * SPAN + 24, false, true), None);
    assert_eq!(
        rows.next_page(9 * SPAN, 9 * SPAN + 24, true, true),
        Some(10)
    );
    assert_eq!(
        rows.next_page(20 * SPAN, 20 * SPAN + 24, false, true),
        Some(20),
        "but the page being looked at is not a guess"
    );
}

#[test]
fn a_page_the_index_has_moved_past_is_re_read_only_where_it_is_seen() {
    let rows = Rows::default();
    rows.put(0, page(SPAN), Vec::new(), 10_000);
    rows.put(1, page(SPAN), Vec::new(), 10_000);
    rows.mark(7);
    // On screen: re-read, because what it shows may be out of date.
    assert_eq!(rows.next_page(0, 24, true, true), Some(0));
    rows.asking(0, 7);
    rows.put(0, page(SPAN), Vec::new(), 10_000);
    // Off screen: left alone, or a moving index refetches every page seen.
    assert_eq!(rows.next_page(0, 24, true, true), None);
}

#[test]
fn scrolling_back_over_something_already_seen_asks_for_nothing() {
    // The point of keeping pages: going back over what was read asks nothing.
    let rows = Rows::default();
    for p in 0..8 {
        rows.put(p, page(SPAN), Vec::new(), 10_000);
    }
    // Page 7 still wants the one after it: the fetch ahead of the eye.
    assert_eq!(rows.next_page(7 * SPAN, 7 * SPAN + 24, true, true), Some(8));
    for p in (0..7).rev() {
        let first = p * SPAN;
        assert_eq!(
            rows.next_page(first, first + 24, true, true),
            None,
            "page {p} was already read, and so were both beside it"
        );
    }
}

#[test]
fn only_so_many_pages_are_kept() {
    let rows = Rows::default();
    for p in 0..scour_page::KEPT + 4 {
        rows.put(p, page(SPAN), Vec::new(), 100_000);
    }
    assert_eq!(rows.held(), scour_page::KEPT * SPAN);
    assert!(
        rows.next_page(0, 24, true, true).is_some(),
        "the oldest went first"
    );
    assert!(
        rows.next_page(
            scour_page::KEPT * SPAN,
            scour_page::KEPT * SPAN + 24,
            true,
            true
        )
        .is_none(),
        "and the newest stayed"
    );
}

#[test]
fn a_page_nobody_had_read_holds_no_arrivals() {
    // "New" is measured against this page's own last read, not against
    // whichever page was fetched last.
    let rows = Rows::default();
    assert!(
        !rows.put(0, named(&["/a", "/b"]), Vec::new(), 10_000),
        "the first read of a page is not an arrival"
    );
    assert!(
        !rows.put(7, named(&["/c", "/d"]), Vec::new(), 10_000),
        "nor is the first read of another page"
    );
    assert!(!rows.row_data(7 * SPAN).unwrap().fresh);

    // But a page that comes back holding something it did not before is
    // exactly what the wash is for.
    assert!(rows.put(7, named(&["/c", "/new", "/d"]), Vec::new(), 10_000));
    assert!(!rows.row_data(7 * SPAN).unwrap().fresh, "/c was here");
    assert!(rows.row_data(7 * SPAN + 1).unwrap().fresh, "/new was not");
    assert!(!rows.row_data(7 * SPAN + 2).unwrap().fresh, "/d was here");

    // And the same page unchanged says nothing at all.
    assert!(!rows.put(7, named(&["/c", "/new", "/d"]), Vec::new(), 10_000));
}

#[test]
fn the_arrival_flags_come_off_what_is_held_and_nothing_else() {
    let rows = Rows::default();
    // Read twice, the second time with a row the first did not have.
    rows.put(5, named(&["/a", "/b", "/c", "/d"]), Vec::new(), 2_000_000);
    assert!(rows.put(5, named(&["/a", "/new", "/c", "/d"]), Vec::new(), 2_000_000));
    let before = rows.resets();
    rows.clear_fresh();
    assert!(!rows.row_data(5 * SPAN + 1).unwrap().fresh);
    assert_eq!(rows.resets(), before, "clearing a flag is not a re-measure");
    assert_eq!(rows.wanted(), None, "and it asks for nothing");
}

#[test]
fn a_selection_knows_what_it_holds_and_what_it_weighs() {
    // Weights come with the page and never go through Slint, whose numbers
    // are 32-bit; the size column holds text like `1.30 MiB`.
    let rows = Rows::default();
    rows.put(
        3,
        named(&["/a/one.txt", "/a/two.txt", "/b"]),
        vec![1_000, 3_000_000_000, 0],
        10_000,
    );
    let pick = rows.pick_at(3 * SPAN + 1).expect("in hand");
    assert_eq!(pick.path, "/a/two.txt");
    assert_eq!(pick.bytes, 3_000_000_000, "past what an i32 holds");
    assert_eq!(pick.folder(), "/a");
    assert_eq!(rows.pick_at(3 * SPAN + 9), None, "past the page's end");
    assert_eq!(rows.pick_at(0), None, "a page that is not in hand");
    // A path at the root has the root for a folder, not an empty string.
    assert_eq!(rows.pick_at(3 * SPAN + 2).unwrap().folder(), "/");
}

#[test]
fn what_is_picked_stays_picked_while_it_scrolls_away_and_back() {
    // The selection is by path; the rows it paints come and go with pages.
    let rows = Rows::default();
    rows.put(0, named(&["/a", "/b", "/c"]), Vec::new(), 10_000);
    let picked: std::collections::HashSet<String> = ["/b".to_string()].into_iter().collect();
    rows.mark_picked(&picked);
    assert!(rows.row_data(1).unwrap().picked);
    assert!(!rows.row_data(0).unwrap().picked);
    // The page is read again — a live refresh — and the mark is put back.
    rows.put(0, named(&["/a", "/b", "/c"]), Vec::new(), 10_000);
    rows.mark_picked(&picked);
    assert!(rows.row_data(1).unwrap().picked);
    // And dropping the selection unpaints it.
    rows.mark_picked(&std::collections::HashSet::new());
    assert!(!rows.row_data(1).unwrap().picked);
}

#[test]
fn the_row_a_list_of_millions_calls_four_thousand_is_the_right_file() {
    // A page index read as though the page began at row zero opens the
    // wrong file, two hundred rows away.
    let rows = Rows::default();
    rows.put(
        20,
        named(&["/a/one.txt", "/a/two.txt"]),
        Vec::new(),
        2_000_000,
    );
    assert_eq!(rows.path_at(20 * SPAN + 1).as_deref(), Some("/a/two.txt"));
    assert_eq!(rows.path_at(20 * SPAN + 2), None, "past the page's end");
    assert_eq!(rows.path_at(0), None, "a page that is not in hand");
}

#[test]
fn a_new_question_empties_the_pages() {
    let rows = Rows::default();
    rows.put(0, named(&["/old"]), Vec::new(), 10_000);
    rows.empty();
    assert_eq!(rows.path_at(0), None);
    assert_eq!(rows.next_page(0, 24, true, true), Some(0));
}

#[test]
fn find_path_tracks_a_row_as_arrivals_push_it_down_and_across_pages() {
    let rows = Rows::default();
    rows.put(0, named(&["/a/doc.docx", "/a/old.txt"]), Vec::new(), 10_000);
    assert_eq!(rows.find_path("/a/doc.docx"), Some(0));

    rows.put(
        0,
        named(&["/a/new1.docx", "/a/new2.docx", "/a/doc.docx", "/a/old.txt"]),
        Vec::new(),
        10_000,
    );
    assert_eq!(rows.find_path("/a/doc.docx"), Some(2));

    rows.put(1, named(&["/a/later.docx"]), Vec::new(), 10_000);
    assert_eq!(rows.find_path("/a/later.docx"), Some(SPAN));
    assert_eq!(rows.find_path("/a/missing.docx"), None);
}
