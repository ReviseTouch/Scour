//! Turning a reply into what the window draws.
//!
//! Everything here is formatting, and it is all on this side of the language
//! boundary on purpose. A `.slint` file that formats a size has to know about
//! binary units; one that computes a highlight has to fold Turkish text. Both
//! would be a second implementation of something that
//! already exists in `scour-core`, and the two would drift.

use humansize::{BINARY, format_size};
use scour_core::{Hit, Kind, text::Folder};

use crate::Row;

const DAY: i64 = 86_400;

/// Which of the six age bands a timestamp falls in.
///
/// The same bands the disk-usage report uses, and for the same reason: how old
/// a thing is answers "is this what I was just working on" faster than a date
/// does, and it does it in six pixels.
pub fn band(now: i64, mtime: i64) -> i32 {
    match now - mtime {
        a if a < DAY => 0,
        a if a < 7 * DAY => 1,
        a if a < 30 * DAY => 2,
        a if a < 180 * DAY => 3,
        a if a < 365 * DAY => 4,
        _ => 5,
    }
}

/// A name cut into what precedes the match, the match, and what follows.
///
/// Split here rather than in the interface, and not only because Slint has no
/// substring: the search runs on **folded** text, and Turkish folding changes
/// byte lengths — `İ` is two bytes and folds to one — so an offset found in
/// folded text cannot be applied to the original spelling. `fold_indexed` is
/// the function that maps it back, it lives in `scour-core`, and its doc
/// comment says this is what it is for.
pub fn split_at_match<'a>(name: &'a str, terms: &[String]) -> (&'a str, &'a str, &'a str) {
    let folder = scour_core::text::DefaultFolder;
    let (folded, back) = folder.fold_indexed(name);
    let mut best: Option<(usize, usize)> = None;
    for term in terms {
        let needle = folder.fold(term);
        if needle.is_empty() {
            continue;
        }
        if let Some(at) = folded.find(&needle) {
            // The earliest match, and the longest among those: a query of two
            // terms should light up the one the eye lands on first.
            let end = at + needle.len();
            let cand = (at, end);
            if best.is_none_or(|(b_at, b_end)| (at, end - at) < (b_at, b_end - b_at)) {
                best = Some(cand);
            }
        }
    }
    let Some((from, to)) = best else {
        return (name, "", "");
    };
    // Folded byte offset → the same place in the original spelling. Clamped
    // and pushed to a character boundary, because a slice that lands mid
    // character is a panic and a name is arbitrary bytes from a disk.
    let mut a = (back.get(from).copied().unwrap_or(0) as usize).min(name.len());
    let mut b = (back.get(to).copied().unwrap_or(name.len() as u32) as usize).min(name.len());
    while a > 0 && !name.is_char_boundary(a) {
        a -= 1;
    }
    while b < name.len() && !name.is_char_boundary(b) {
        b += 1;
    }
    if a > b {
        return (name, "", "");
    }
    (&name[..a], &name[a..b], &name[b..])
}

/// One hit, formatted.
/// The colour a kind's icon is drawn in, or the window's quiet ink when the
/// kind has none — a plain file is not a category worth a hue.
fn tint_of(token: &str) -> slint::Brush {
    match scour_ui::kind_colour(token) {
        Some(c) => {
            let (a, r, g, b) = c.argb();
            slint::Brush::SolidColor(slint::Color::from_argb_u8(a, r, g, b))
        }
        None => {
            let (a, r, g, b) = scour_ui::DARK.ink_3.argb();
            slint::Brush::SolidColor(slint::Color::from_argb_u8(a, r, g, b))
        }
    }
}

pub fn row_of(h: &Hit, terms: &[String], now: i64, kind: &str, fresh: bool) -> Row {
    let (pre, hit, post) = split_at_match(h.name(), terms);
    Row {
        pre: pre.into(),
        hit: hit.into(),
        post: post.into(),
        folder: h.parent().into(),
        kind: kind.into(),
        fresh,
        ktoken: h.kind.token().into(),
        tint: tint_of(h.kind.token()),
        size: if h.is_dir {
            slint::SharedString::new()
        } else {
            format_size(h.meta.size.max(0) as u64, BINARY).into()
        },
        stamp: stamp(h.meta.mtime).into(),
        is_dir: h.is_dir,
        age: band(now, h.meta.mtime),
    }
}

/// `YYYY-MM-DD HH:MM`, in UTC.
///
/// The same choice the CLI makes and for the same reason: local time needs the
/// zone database, and a listing is read for ordering far more often than for
/// the exact minute. A window will want a real clock eventually; this is not
/// the thing to stop and get right before it opens.
pub fn stamp(secs: i64) -> String {
    if secs <= 0 {
        return String::new();
    }
    let days = secs.div_euclid(DAY);
    let rest = secs.rem_euclid(DAY);
    let (y, m, d) = civil(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rest / 3600,
        rest % 3600 / 60
    )
}

/// Days since the epoch to a calendar date. Howard Hinnant's `civil_from_days`.
fn civil(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The kinds the rail offers, in the order it shows them.
pub fn offered_kinds() -> &'static [Kind] {
    &Kind::OFFERED
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_split_lands_on_the_original_spelling_not_the_folded_one() {
        // `Değişiklik` is ten characters and thirteen bytes. An offset taken
        // from the folded text and used unchanged would cut somewhere else —
        // and could cut a character in half, which is a panic.
        let (pre, hit, post) = split_at_match("Değişiklik Raporu.txt", &["raporu".into()]);
        assert_eq!(pre, "Değişiklik ");
        assert_eq!(hit, "Raporu");
        assert_eq!(post, ".txt");
    }

    #[test]
    fn folding_finds_what_a_turkish_keyboard_typed() {
        // The case the fold exists for: `İ` is two bytes and folds to one.
        let (pre, hit, post) = split_at_match("İSTANBUL.pdf", &["istanbul".into()]);
        assert_eq!((pre, hit, post), ("", "İSTANBUL", ".pdf"));
    }

    #[test]
    fn a_term_that_is_not_there_lights_nothing_up() {
        assert_eq!(
            split_at_match("main.rs", &["zzz".into()]),
            ("main.rs", "", "")
        );
        assert_eq!(split_at_match("main.rs", &[]), ("main.rs", "", ""));
    }

    #[test]
    fn the_earliest_match_wins_when_several_terms_hit() {
        let (pre, hit, _) = split_at_match("rapor-belge.pdf", &["belge".into(), "rapor".into()]);
        assert_eq!((pre, hit), ("", "rapor"));
    }

    #[test]
    fn a_name_that_is_not_text_does_not_panic() {
        // Names come off a disk and are arbitrary bytes; this one is what a
        // lossy conversion leaves behind.
        let odd = "caf\u{fffd}\u{301}.txt";
        let (pre, hit, post) = split_at_match(odd, &["caf".into()]);
        assert_eq!(format!("{pre}{hit}{post}"), odd);
    }

    #[test]
    fn stamps_are_the_dates_they_claim_to_be() {
        assert_eq!(stamp(1_769_817_600), "2026-01-31 00:00");
        assert_eq!(stamp(0), "");
    }

    #[test]
    fn the_bands_run_from_today_to_older() {
        let now = 1_800_000_000;
        assert_eq!(band(now, now), 0);
        assert_eq!(band(now, now - 3 * DAY), 1);
        assert_eq!(band(now, now - 400 * DAY), 5);
        // A timestamp in the future is today, not older than everything.
        assert_eq!(band(now, now + DAY), 0);
    }
}
