/// A page arriving while the result also changes length has to announce both,
/// or a view redraws the length and keeps the rows it had.
#[test]
fn a_page_that_lands_while_the_length_moves_still_says_which_rows_it_replaced() {
    let mut pages: Pages<u32> = Pages::default();
    pages.set_total(100);

    // The length held still, so the rows are reported.
    pages.asking(0);
    match pages.put(0, vec![7; SPAN], 100) {
        Change::Rows { from, to } => assert_eq!((from, to), (0, SPAN)),
        other => panic!("expected the rows, got {other:?}"),
    }

    // The same page again, with the total moved by one because something
    // appeared while it was in flight.
    pages.asking(0);
    match pages.put(0, vec![9; SPAN], 101) {
        Change::Length { was, now, rows } => {
            assert_eq!((was, now), (100, 101));
            assert_eq!(
                rows,
                Some((0, SPAN)),
                "the page's rows were replaced and nothing said so"
            );
        }
        other => panic!("expected a length change carrying its rows, got {other:?}"),
    }

    // A length that moves on its own has no page behind it and must not
    // claim one: the view would read rows out of a page it does not hold.
    match pages.set_total(140) {
        Change::Length { rows, .. } => assert_eq!(rows, None),
        other => panic!("expected a bare length change, got {other:?}"),
    }
}
use super::*;

fn page_of(n: usize, from: usize) -> Vec<usize> {
    (from..from + n).collect()
}

#[test]
fn a_row_belongs_to_the_page_that_covers_it() {
    assert_eq!(Pages::<usize>::page_of(0), 0);
    assert_eq!(Pages::<usize>::page_of(SPAN - 1), 0);
    assert_eq!(Pages::<usize>::page_of(SPAN), 1);
    assert_eq!(Pages::<usize>::start_of(3), 3 * SPAN);
}

#[test]
fn a_page_is_filed_where_the_answer_says_it_belongs() {
    // Page 4 was asked for and page 7 is now wanted; the answer is still 4.
    let mut p: Pages<usize> = Pages::default();
    p.asking(7);
    assert_eq!(
        p.put(4, page_of(SPAN, 4 * SPAN), 10_000),
        Change::Length {
            was: 0,
            now: 10_000,
            rows: Some((4 * SPAN, 5 * SPAN)),
        }
    );
    assert_eq!(p.at(4 * SPAN), Some(&(4 * SPAN)));
    assert_eq!(p.at(7 * SPAN), None, "page seven never arrived");
    assert_eq!(p.asked(), Some(7), "and is still out");
}

#[test]
fn nothing_is_asked_for_while_an_answer_is_on_its_way() {
    let mut p: Pages<usize> = Pages::default();
    p.set_total(10_000);
    assert_eq!(p.next_page(0, 40, false, false), Some(0));
    p.asking(0);
    assert_eq!(
        p.next_page(5_000, 5_040, false, false),
        None,
        "the eye moved, but one question at a time"
    );
    p.put(0, page_of(SPAN, 0), 10_000);
    assert_eq!(
        p.next_page(5_000, 5_040, false, false),
        Some(25),
        "and the next question is about where it got to"
    );
}

/// The first page of a question is cut to a screenful. Scrolled past
/// it, the rest of the page was never asked for: on an index that did not
/// move, the rows below the first screen stayed blank.
#[test]
fn a_page_cut_short_is_read_whole_once_the_eye_goes_past_it() {
    let mut p: Pages<usize> = Pages::default();
    p.put(0, page_of(28, 0), 10_000);
    assert_eq!(
        p.next_page(0, 20, false, false),
        None,
        "all on screen is here"
    );
    assert_eq!(
        p.next_page(20, 40, false, false),
        Some(0),
        "row 28 on is not"
    );
    p.put(0, page_of(SPAN, 0), 10_000);
    assert_eq!(p.next_page(20, 40, false, false), None);
    // A whole page and the next in sight: the next, not this one again.
    assert_eq!(p.next_page(150, 250, false, false), Some(1));
}

#[test]
fn a_page_short_of_the_count_twice_is_not_asked_for_a_third_time() {
    let mut p: Pages<usize> = Pages::default();
    p.put(0, page_of(28, 0), 10_000);
    assert_eq!(p.next_page(20, 40, false, false), Some(0));
    p.put(0, page_of(28, 0), 10_000);
    assert_eq!(
        p.next_page(20, 40, false, false),
        None,
        "the service has no more"
    );
}

#[test]
fn the_last_page_of_a_result_is_whole_however_short() {
    let mut p: Pages<usize> = Pages::default();
    p.put(2, page_of(17, 2 * SPAN), 2 * SPAN + 17);
    assert_eq!(p.next_page(2 * SPAN, 2 * SPAN + 40, false, false), None);
}

#[test]
fn a_page_ahead_is_fetched_only_when_a_page_is_cheap() {
    let mut p: Pages<usize> = Pages::default();
    p.set_total(10_000);
    p.put(0, page_of(SPAN, 0), 10_000);
    assert_eq!(
        p.next_page(0, 40, false, false),
        None,
        "all on screen is here"
    );
    assert_eq!(p.next_page(0, 40, true, false), Some(1), "one ahead");
}

#[test]
fn a_stale_page_is_re_read_only_when_there_is_time_for_it() {
    let mut p: Pages<usize> = Pages::default();
    p.set_total(1_000);
    p.put(0, page_of(SPAN, 0), 1_000);
    p.mark(7);
    assert_eq!(p.next_page(0, 40, false, false), None);
    assert_eq!(p.next_page(0, 40, false, true), Some(0));
}

#[test]
fn a_page_is_stamped_with_the_index_it_was_asked_at() {
    // The index moved in flight, so these rows are stale as they land and a
    // refresh has to offer them again.
    let mut p: Pages<usize> = Pages::default();
    p.set_total(1_000);
    p.mark(1);
    p.asking(0);
    p.mark(2);
    p.put(0, page_of(SPAN, 0), 1_000);
    assert_eq!(
        p.next_page(0, 40, false, true),
        Some(0),
        "read at revision one, and the index is at two"
    );
}

#[test]
fn the_least_looked_at_page_is_the_one_let_go() {
    let mut p: Pages<usize> = Pages::default();
    p.set_total(100_000);
    for page in 0..KEPT + 4 {
        p.put(page, page_of(SPAN, page * SPAN), 100_000);
    }
    assert!(!p.holds(0), "the first pages went");
    assert!(p.holds(KEPT + 3), "the last one is here");
    assert!(
        p.pages().count() <= KEPT,
        "and no more than {KEPT} are kept"
    );
}

#[test]
fn a_short_page_ends_the_list_only_when_it_is_short_by_both_measures() {
    let mut p: Pages<usize> = Pages::default();
    p.set_total(10_000);
    // A full page from a service whose ceiling is SPAN, asked for more.
    p.put(0, page_of(SPAN, 0), 10_000);
    assert_eq!(
        p.length(0, SPAN, SPAN + 56),
        10_000,
        "short of what was asked for, but not short for this service"
    );
    // Genuinely the end.
    assert_eq!(p.length(3, 40, SPAN), 3 * SPAN + 40);
}

#[test]
fn emptying_drops_the_pages_and_keeps_the_shape() {
    let mut p: Pages<usize> = Pages::default();
    p.put(0, page_of(SPAN, 0), 10_000);
    p.empty();
    assert_eq!(p.at(0), None, "the rows are gone");
    assert_eq!(p.total(), 10_000, "the length is not");
    assert_eq!(
        p.next_page(0, 24, true, true),
        Some(0),
        "and the first page is asked for again"
    );
}

#[test]
fn a_counting_cap_is_not_a_length() {
    let mut p: Pages<usize> = Pages::default();
    assert_eq!(
        p.set_total(1_000),
        Change::Length {
            was: 0,
            now: 1_000,
            rows: None
        }
    );
    assert_eq!(
        p.set_total(2_696_724),
        Change::Length {
            was: 1_000,
            now: 2_696_724,
            rows: None,
        },
        "the exact count arrives later and is longer"
    );
    assert_eq!(
        p.set_total(2_696_724),
        Change::Nothing,
        "and again is not news"
    );
}
