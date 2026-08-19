//! Numbers, sizes and dates, said the same way in every window.
//!
//! **These were written three times.** The browser page has its own in
//! JavaScript, the window had `grouped` and `compact_bytes` and reached for
//! `humansize` for a third shape, and a terminal would have made a fourth set
//! — at which point "2,1 GB" in one window and "2.1 GiB" in another would be
//! the same file. So they live here, beside the palette and the columns, for
//! the same reason those do: a thing every face has to agree on is not a thing
//! any one face owns.
//!
//! No dependencies, like the rest of this crate.

/// A number a person can read: `5356281` becomes `5.356.281`.
///
/// **The separator is the language's, not the platform's.** A window may be
/// asked for English on a Turkish desktop, and the number belongs to the text
/// around it rather than to the machine under it.
pub fn grouped(n: u64, separator: char) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(separator);
        }
        out.push(c);
    }
    out
}

/// The separator a language groups thousands with.
///
/// Two answers, because two languages: a dot for Turkish, a comma for
/// everything else this speaks. It takes the tag rather than a catalogue so
/// that a crate with no dependencies can answer it.
pub fn group_mark(language: &str) -> char {
    if language.starts_with("tr") { '.' } else { ',' }
}

/// The decimal mark that goes with [`group_mark`].
pub fn decimal_mark(language: &str) -> char {
    if language.starts_with("tr") { ',' } else { '.' }
}

/// Bytes at a glance, for a meter or a summary: `636,3 MB`.
///
/// Two units and one decimal, because this is read in passing — the exact
/// byte count belongs in a column, not in a sentence about how big an index
/// is.
pub fn compact_bytes(n: u64, decimal: char) -> String {
    let mb = n as f64 / 1_048_576.0;
    let said = if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else {
        format!("{mb:.1} MB")
    };
    if decimal == '.' {
        said
    } else {
        said.replace('.', &decimal.to_string())
    }
}

/// Bytes in a column, where they are compared with the row above: `1,44 MiB`.
///
/// **Binary units, and the unit is named.** A list sorted by size is read down
/// the column, and `1.4 MB` beside `1,440 KB` makes somebody do arithmetic to
/// see which is bigger. Powers of two, three significant figures, and the same
/// width whatever the number.
pub fn size(n: u64, decimal: char) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    if n < 1024 {
        return format!("{n} B");
    }
    let mut left = n as f64;
    let mut unit = 0;
    while left >= 1024.0 && unit + 1 < UNITS.len() {
        left /= 1024.0;
        unit += 1;
    }
    let said = if left >= 100.0 {
        format!("{left:.0} {}", UNITS[unit])
    } else if left >= 10.0 {
        format!("{left:.1} {}", UNITS[unit])
    } else {
        format!("{left:.2} {}", UNITS[unit])
    };
    if decimal == '.' {
        said
    } else {
        said.replace('.', &decimal.to_string())
    }
}

/// Seconds in a day, which several of these count in.
pub const DAY: i64 = 86_400;

/// `YYYY-MM-DD HH:MM`, in UTC.
///
/// The same choice the command line makes and for the same reason: local time
/// needs the zone database, and a listing is read for ordering far more often
/// than for the exact minute.
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

/// Which of the six age bands a moment falls in: 0 is today, 5 is over a year.
///
/// The stripe down the left of every row, and the bands the report counts in.
/// One function so that a row drawn in a terminal and the same row in a window
/// are never a different colour.
pub fn band(now: i64, mtime: i64) -> usize {
    match now - mtime {
        a if a < DAY => 0,
        a if a < 7 * DAY => 1,
        a if a < 30 * DAY => 2,
        a if a < 180 * DAY => 3,
        a if a < 365 * DAY => 4,
        _ => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_number_is_grouped_in_the_language_being_spoken() {
        assert_eq!(grouped(5_356_281, '.'), "5.356.281");
        assert_eq!(grouped(5_356_281, ','), "5,356,281");
        assert_eq!(grouped(0, '.'), "0");
        assert_eq!(grouped(999, '.'), "999");
        assert_eq!(grouped(1_000, '.'), "1.000");
        assert_eq!(group_mark("tr"), '.');
        assert_eq!(group_mark("tr-TR"), '.');
        assert_eq!(group_mark("en"), ',');
        assert_eq!(decimal_mark("tr"), ',');
    }

    #[test]
    fn a_size_keeps_three_figures_and_names_its_unit() {
        assert_eq!(size(0, '.'), "0 B");
        assert_eq!(size(1023, '.'), "1023 B");
        assert_eq!(size(1024, '.'), "1.00 KiB");
        assert_eq!(size(1_512_000, '.'), "1.44 MiB");
        assert_eq!(size(1_512_000, ','), "1,44 MiB");
        // Ten and a hundred are where the decimals drop, so that the column
        // stays the same width all the way down.
        assert_eq!(size(10 * 1024 * 1024 + 512 * 1024, '.'), "10.5 MiB");
        assert_eq!(size(150 * 1024 * 1024, '.'), "150 MiB");
    }

    #[test]
    fn the_meter_says_two_units_and_one_decimal() {
        assert_eq!(compact_bytes(667_264_614, ','), "636,4 MB");
        assert_eq!(compact_bytes(667_264_614, '.'), "636.4 MB");
        assert_eq!(compact_bytes(2_147_483_648, ','), "2,0 GB");
    }

    #[test]
    fn a_stamp_is_utc_and_an_unset_time_says_nothing() {
        assert_eq!(stamp(0), "");
        assert_eq!(stamp(-1), "");
        assert_eq!(stamp(1_755_000_000), "2025-08-12 12:00");
    }

    #[test]
    fn the_bands_are_the_six_the_stripe_draws() {
        let now = 1_000 * DAY;
        assert_eq!(band(now, now), 0, "this minute");
        assert_eq!(band(now, now - DAY), 1);
        assert_eq!(band(now, now - 8 * DAY), 2);
        assert_eq!(band(now, now - 40 * DAY), 3);
        assert_eq!(band(now, now - 200 * DAY), 4);
        assert_eq!(band(now, now - 400 * DAY), 5);
        // A file dated in the future is not older than a year.
        assert_eq!(band(now, now + DAY), 0);
    }
}
