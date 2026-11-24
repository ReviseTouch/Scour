//! How well a name answers the words typed, for ordering by relevance.

use super::*;

/// How well a name answers the terms that were typed — the one sort that is a
/// property of the *query*. The weights are ordered, not tuned: each rung is
/// worth more than everything below it can add up to.
///
/// * the name without its extension is the term — `main.rs` for `main`
/// * the whole name is the term
/// * the name starts with it — `main_window.rs`
/// * it starts a word inside the name — `my-main.rs`, but not `domain.rs`
/// * anything else that matched at all
///
/// A stem match outranks an exact name, which filled the page with
/// `.git/refs/heads/main` and `android/src/main`. Then shorter names, then
/// `steps` — [`crate::dirs::steps_of`] — without which 88 of the first two
/// hundred results were caches against 48 of the user's own work. Both are
/// bounded to rows the name already ties: 255 for a name and sixty steps at
/// [`STEP`], against a rung gap of 900.
pub(super) fn relevance(name: &[u8], terms: &[Vec<u8>], steps: u8) -> i64 {
    if terms.is_empty() {
        return 0;
    }
    let stem_end = name
        .iter()
        .rposition(|&b| b == b'.')
        .filter(|&i| i > 0)
        .unwrap_or(name.len());
    let mut best = 0i64;
    for t in terms {
        if t.is_empty() {
            continue;
        }
        let Some(at) = find(name, t) else { continue };
        let score = if stem_end < name.len() && name[..stem_end] == t[..] {
            // `main` in `main.rs`: someone named a file after this.
            4000
        } else if name == &t[..] {
            3000
        } else if at == 0 {
            2000
        } else if !name[at - 1].is_ascii_alphanumeric() {
            1000
        } else {
            100
        };
        best = best.max(score);
    }
    // 255 characters of name and 480 of distance work against it, and neither
    // can overturn a rung: the narrowest gap between rungs is 900.
    best - (name.len().min(255) as i64) - i64::from(steps) * STEP
}

#[cfg(test)]
mod relevance_tests {
    use super::relevance;

    fn score(name: &str, term: &str) -> i64 {
        at(name, term, 0)
    }

    /// The same, for a name that many steps from home.
    pub(super) fn at(name: &str, term: &str, steps: u8) -> i64 {
        relevance(name.as_bytes(), &[term.as_bytes().to_vec()], steps)
    }

    #[test]
    fn a_file_named_after_the_term_beats_a_folder_that_merely_is_it() {
        // Ranking the exact name highest filled the page with
        // `.git/refs/heads/main` and a hundred `android/src/main` directories.
        assert!(score("main.rs", "main") > score("main", "main"));
        assert!(score("main", "main") > score("main_window.rs", "main"));
    }

    #[test]
    fn a_word_boundary_beats_the_middle_of_a_word() {
        assert!(score("my-main.rs", "main") > score("domain.rs", "main"));
    }

    #[test]
    fn the_shorter_of_two_equal_matches_wins() {
        assert!(score("main.rs", "main") > score("mainly-about-something.rs", "main"));
    }

    #[test]
    fn length_never_overturns_a_rung() {
        // 900 is the narrowest gap between rungs and a name is capped at 255,
        // so no amount of length can promote a weaker match.
        let long_stem = format!("{}.rs", "main");
        let short_prefix = "mainx";
        assert!(score(&long_stem, "main") > score(short_prefix, "main"));
    }

    #[test]
    fn a_query_with_no_terms_scores_everything_alike() {
        assert_eq!(relevance(b"anything.txt", &[], 40), 0);
    }

    #[test]
    fn the_nearer_of_two_equal_names_wins() {
        // `~/Projeler/x/src/main.rs` against the same name buried in a cache.
        assert!(at("main.rs", "main", 5) > at("main.rs", "main", 12));
    }

    #[test]
    fn distance_never_overturns_a_rung() {
        // The bound at the narrowest gap there is: a word boundary is 1000 and
        // the middle of a word is 100. A match on the boundary, as far away as
        // the table records and with the whole length penalty, still wins.
        let far_and_long = format!("my-main{}.rs", "x".repeat(250));
        assert!(at("my-main.rs", "main", 60) > at("domain.rs", "main", 0));
        assert!(at(&far_and_long, "main", 60) > at("domain.rs", "main", 0));
    }
}
