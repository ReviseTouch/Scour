//! A place group turned round: `under:a|under:b` ("in any of them") and
//! `!under:a !under:b` ("in none of them") are one set written two ways, and
//! the capsule's `!` goes from one to the other in place.

use super::Term;
use super::group::{is_place, runs};

/// The blocks of standalone `!under:` terms side by side, in order: each
/// term a run of its own, with nothing but spaces between them.
pub(super) fn exclusion_blocks(text: &str) -> Vec<Vec<Term>> {
    let mut out: Vec<Vec<Term>> = Vec::new();
    let mut open = false;
    for run in runs(text) {
        let one = run.len() == 1 && run[0].not && run[0].field == Some("under");
        match out.last_mut() {
            Some(block) if one && open => block.push(run[0].clone()),
            _ if one => out.push(vec![run[0].clone()]),
            _ => {}
        }
        open = one;
    }
    out
}

/// "In none of them": the run of `under:` terms at `at` written as one
/// `!under:` per member, side by side, and the caret after them. Empty
/// members go. Anything else at `at` is left as it is.
pub fn negate_places(text: &str, at: usize) -> (String, usize) {
    let Some(run) = runs(text)
        .into_iter()
        .find(|r| r.iter().all(is_place) && spans(r, at))
    else {
        return (text.to_string(), at);
    };
    let each: Vec<String> = run
        .iter()
        .filter(|t| !t.value.is_empty())
        .map(|t| format!("!{}", &text[t.start..t.end]))
        .collect();
    rewrite(text, &run, &each.join(" "))
}

/// The reverse of [`negate_places`]: the block of `!under:` terms at `at`
/// written as one `|` group, and the caret after it.
pub fn unnegate_places(text: &str, at: usize) -> (String, usize) {
    let Some(block) = exclusion_blocks(text).into_iter().find(|b| spans(b, at)) else {
        return (text.to_string(), at);
    };
    let each: Vec<&str> = block.iter().map(|t| &text[t.start + 1..t.end]).collect();
    rewrite(text, &block, &each.join("|"))
}

fn spans(terms: &[Term], at: usize) -> bool {
    terms[0].start <= at && at <= terms[terms.len() - 1].end
}

fn rewrite(text: &str, terms: &[Term], with: &str) -> (String, usize) {
    let (a, b) = (terms[0].start, terms[terms.len() - 1].end);
    (
        format!("{}{with}{}", &text[..a], &text[b..]),
        a + with.len(),
    )
}

#[cfg(test)]
#[path = "negate_tests.rs"]
mod tests;
