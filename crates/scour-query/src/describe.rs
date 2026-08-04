//! An [`Ast`] read back as a sentence.
//!
//! Two callers want this and for the same reason. A language model that issues
//! a query should be able to check what it actually asked for, because the
//! parser is forgiving by design and a typo becomes a plain text search rather
//! than an error — `boyut:1mb` looks for the *string* "boyut:1mb", silently and
//! reasonably, because there is no `boyut` field. (`sizE:>1mb` is not an example
//! of this, though it was used as one here for a while: field names are folded,
//! so that one is the size field.) And a person looking at a saved search wants
//! to read it without learning the syntax.
//!
//! The English here is a message id, like everywhere else: a frontend with a
//! catalogue translates it, one without is still correct.
//!
//! [`Ast`]: scour_core::Ast

use scour_core::{Ast, Cmp, Match, TimeField};

/// Describe a parsed query in English.
pub fn describe(ast: &Ast) -> String {
    if ast.is_empty() {
        return "everything".into();
    }
    let groups: Vec<String> = ast
        .groups
        .iter()
        .map(|g| {
            let alts: Vec<String> = g
                .alts
                .iter()
                .map(|(neg, m)| {
                    let s = describe_match(m);
                    if *neg { format!("not {s}") } else { s }
                })
                .collect();
            match alts.len() {
                1 => alts.into_iter().next().unwrap_or_default(),
                _ => format!("({})", alts.join(" or ")),
            }
        })
        .collect();
    groups.join(" and ")
}

fn describe_match(m: &Match) -> String {
    match m {
        Match::NameContains(t) => format!("name contains \"{t}\""),
        Match::NameGlob(p) => format!("name matches \"{p}\""),
        Match::PathContains(t) => format!("path contains \"{t}\""),
        Match::Under(d) => format!("is under {d}"),
        Match::ParentIs(d) => format!("is directly in {d}"),
        Match::Ext(e) if e.len() == 1 => format!("extension is .{}", e[0]),
        Match::Ext(e) => format!("extension is one of .{}", e.join(", .")),
        Match::IsDir(true) => "is a folder".into(),
        Match::IsDir(false) => "is a file".into(),
        Match::Size(cmp, bytes) => format!("size {} {}", cmp.symbol(), human_size(*bytes)),
        Match::Kind(k) if k.len() == 1 => format!("type is {}", k[0].msgid().to_lowercase()),
        Match::Kind(k) => format!(
            "type is one of {}",
            k.iter()
                .map(|k| k.msgid().to_lowercase())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Match::Time(f, Cmp::Eq, secs) => format!("{} on {}", time_word(*f), iso_day(*secs)),
        Match::Time(f, cmp, secs) => {
            format!("{} {} {}", time_word(*f), cmp.symbol(), iso_day(*secs))
        }
        Match::ContentContains(t) => format!("contents contain \"{t}\""),
    }
}

fn time_word(f: TimeField) -> &'static str {
    match f {
        TimeField::Modified => "modified",
        TimeField::Created => "created",
        TimeField::Accessed => "accessed",
    }
}

/// Binary units, because that is what the parser accepts and what file
/// managers on these platforms report.
fn human_size(bytes: i64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v.abs() >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.3} {}", UNITS[u]).replace(".000", "")
    }
}

/// Epoch seconds back to `YYYY-MM-DD` in UTC — the inverse of the parser's
/// civil-date conversion, so a described query can be pasted back in.
fn iso_day(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_at;

    fn d(q: &str) -> String {
        describe(&parse_at(q, 1_800_000_000))
    }

    #[test]
    fn an_empty_query_is_everything() {
        assert_eq!(d(""), "everything");
    }

    #[test]
    fn structure_reads_as_and_or_not() {
        assert_eq!(
            d("rapor ext:pdf"),
            "name contains \"rapor\" and extension is .pdf"
        );
        assert_eq!(d("a|b"), "(name contains \"a\" or name contains \"b\")");
        assert_eq!(d("!tmp"), "not name contains \"tmp\"");
    }

    #[test]
    fn sizes_and_kinds_read_naturally() {
        assert_eq!(d("size:>1mb"), "size > 1 MB");
        assert_eq!(d("size:=0"), "size = 0 B");
        assert_eq!(d("kind:klasör"), "type is folder");
        assert_eq!(d("folder:"), "is a folder");
    }

    #[test]
    fn a_described_date_can_be_pasted_back_in() {
        // The round trip is the point: a model reading this back must get a
        // string the parser accepts and resolves to the same instant.
        assert_eq!(d("dc:2026-01-31"), "created on 2026-01-31");
        assert_eq!(parse_at("dc:2026-01-31", 0), parse_at("dc:=2026-01-31", 0));
    }

    #[test]
    fn a_relative_window_is_described_as_the_day_it_resolved_to() {
        // 1_800_000_000 is 2027-01-15; seven days back lands on 2027-01-08.
        assert_eq!(d("dm:7d"), "modified >= 2027-01-08");
    }

    #[test]
    fn civil_conversion_round_trips() {
        for days in [0_i64, 1, -1, 19_000, 20_500, -25_000] {
            let (y, m, dd) = civil_from_days(days);
            assert_eq!(iso_day(days * 86_400), format!("{y:04}-{m:02}-{dd:02}"));
        }
        assert_eq!(iso_day(0), "1970-01-01");
        assert_eq!(iso_day(1_769_817_600), "2026-01-31");
    }
}
