//! Numbers, sizes and dates, said the same way in every window: "2,1 GB" in one
//! face and "2.1 GiB" in another must never be the same file.

/// A number a person can read: `5356281` becomes `5.356.281`. The separator is
/// the language's, not the platform's.
pub fn grouped(n: u64, separator: char) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(separator);
        }
        out.push(c);
    }
    out
}

/// The separator a language groups thousands with: a dot for Turkish, a comma
/// otherwise. Takes the tag rather than a catalogue, to stay dependency-free.
pub fn group_mark(language: &str) -> char {
    if language.starts_with("tr") { '.' } else { ',' }
}

/// The decimal mark that goes with [`group_mark`].
pub fn decimal_mark(language: &str) -> char {
    if language.starts_with("tr") { ',' } else { '.' }
}

/// Bytes at a glance, for a meter or a summary: `636,3 MB`. Two units and one
/// decimal, because this is read in passing.
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

/// Bytes in a column, compared with the row above: `1,44 MiB`. Binary units,
/// named, three significant figures, same width whatever the number.
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

/// `YYYY-MM-DD HH:MM` in the person's zone, from `localtime_r`, which knows
/// daylight saving at *that* instant. Off Unix there is no such call: UTC.
pub fn stamp(secs: i64) -> String {
    stamp_at(secs, local_offset(secs))
}

/// The same, with the zone offset in seconds: the pure half, machine-independent.
pub fn stamp_at(secs: i64, offset: i64) -> String {
    if secs <= 0 {
        return String::new();
    }
    let local = secs + offset;
    let days = local.div_euclid(DAY);
    let rest = local.rem_euclid(DAY);
    let (y, m, d) = civil(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}",
        rest / 3600,
        rest % 3600 / 60
    )
}

/// Seconds east of UTC at that instant, asked per instant: a service outlives
/// the twice-yearly change.
#[cfg(unix)]
pub fn local_offset(secs: i64) -> i64 {
    let t = secs as libc::time_t;
    // SAFETY: `tm` is plain data the call fills; `localtime_r` writes only
    // into it and reads only `t`.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if unsafe { libc::localtime_r(&t, &mut tm) }.is_null() {
        return 0;
    }
    tm.tm_gmtoff as i64
}

#[cfg(not(unix))]
pub fn local_offset(_secs: i64) -> i64 {
    0
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
/// One function, so a row in a terminal and the same row in a window match.
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
        // Ten and a hundred are where the decimals drop, keeping the width.
        assert_eq!(size(10 * 1024 * 1024 + 512 * 1024, '.'), "10.5 MiB");
        assert_eq!(size(150 * 1024 * 1024, '.'), "150 MiB");
    }

    #[test]
    fn the_meter_says_two_units_and_one_decimal() {
        assert_eq!(compact_bytes(667_264_614, ','), "636,4 MB");
        assert_eq!(compact_bytes(667_264_614, '.'), "636.4 MB");
        assert_eq!(compact_bytes(2_147_483_648, ','), "2,0 GB");
    }

    /// The pure half, pinned.
    #[test]
    fn a_stamp_at_an_offset_is_exact_and_an_unset_time_says_nothing() {
        assert_eq!(stamp_at(0, 0), "");
        assert_eq!(stamp_at(-1, 3 * 3600), "");
        assert_eq!(stamp_at(1_755_000_000, 0), "2025-08-12 12:00");
        // Istanbul: +03:00 all year.
        assert_eq!(stamp_at(1_755_000_000, 3 * 3600), "2025-08-12 15:00");
        // An offset that crosses midnight moves the date too.
        assert_eq!(stamp_at(1_755_043_200, -3 * 3600), "2025-08-12 21:00");
        assert_eq!(stamp_at(1_755_043_200, 0), "2025-08-13 00:00");
    }

    /// `stamp` is `stamp_at` with the system's offset, never UTC on a machine
    /// that is not in UTC.
    #[test]
    fn a_stamp_is_in_the_zone_the_machine_is_in() {
        let at = 1_755_000_000;
        let off = local_offset(at);
        assert!(off.abs() <= 14 * 3600, "an offset no zone has: {off}");
        assert_eq!(stamp(at), stamp_at(at, off));
        // Only assertable where TZ is not UTC, so it says so rather than
        // failing on a CI box.
        if off != 0 {
            assert_ne!(stamp(at), stamp_at(at, 0), "still UTC despite offset {off}");
        }
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
