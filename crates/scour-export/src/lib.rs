//! The result set, as a spreadsheet.
//!
//! Comma-separated values, written a row at a time into a buffer the caller
//! owns. Nothing here accumulates and nothing here knows how many rows are
//! coming — an export of this index is two and a quarter million of them, and
//! the only shape that works at that size is one that never holds more than
//! the row in hand.
//!
//! ## The decisions, which are older than this crate
//!
//! All four were made in the browser bridge and are carried here unchanged;
//! they are the reason a file produced by `a7789d8` and one produced now are
//! byte for byte the same.
//!
//! * **RFC 4180 quoting**, because filenames contain commas, quotes and
//!   newlines — all three, on this machine.
//! * **A byte-order mark**, because the spreadsheet most people open this in
//!   guesses the encoding otherwise and guesses wrong on Turkish.
//! * **Dates as `2026-03-07 18:25:13`** rather than the window's "5 dk önce".
//!   One is for a person glancing at a screen, the other for something that
//!   will sort and filter it.
//! * **The columns are named by the caller, in the caller's order**, because
//!   they are what the reader chose. A column that is not on screen is one
//!   click from showing and then exported.
//!
//! ## Why this is not in the bridge any more
//!
//! It was, and the bridge is one frontend of four. The terminal had no export
//! at all and the window and the model server would each have needed their own
//! — which is three more chances for the quoting to be subtly different, and a
//! CSV whose quoting is subtly different is a file that opens and is wrong.

use scour_core::Hit;

/// The bytes that go in front of the first line.
///
/// UTF-8's byte-order mark. It signals nothing about byte order — UTF-8 has
/// none — and is here for one reason: without it, the spreadsheet most people
/// open a `.csv` in guesses the encoding, and on Turkish text it guesses a
/// legacy codepage and renders `ç` and `ğ` as mojibake. Written once, before
/// the header row.
pub const BOM: [u8; 3] = [0xEF, 0xBB, 0xBF];

/// What a row is written into.
///
/// The buffer is the caller's, reused between rows. A `String` per row was
/// measured at nothing much per row and two and a quarter million allocations
/// over an export, which is the sort of cost that only shows up at the size
/// this exists for.
#[derive(Debug, Clone)]
pub struct Sheet {
    columns: Vec<String>,
}

impl Sheet {
    /// The columns to write, in the order they will appear.
    ///
    /// An unknown name is kept rather than rejected. It writes an empty cell
    /// under its own heading, which is what a caller asking for a column this
    /// version does not have should see: the shape it asked for, with the part
    /// nobody could fill left blank. Refusing would mean a frontend that grew a
    /// column could not export until the service caught up.
    pub fn new<I, S>(columns: I) -> Sheet
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Sheet {
            columns: columns.into_iter().map(Into::into).collect(),
        }
    }

    /// The columns everything defaults to: what the window shows out of the box.
    pub fn default_columns() -> Vec<String> {
        ["name", "path", "size", "mtime", "kind"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect()
    }

    pub fn columns(&self) -> &[String] {
        &self.columns
    }

    /// The heading row, with the byte-order mark in front of it.
    pub fn header(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(&BOM);
        for (i, c) in self.columns.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            escape_into(c, out);
        }
        out.extend_from_slice(b"\r\n");
    }

    /// One row, appended.
    ///
    /// `cell` is a scratch buffer rather than a `String` returned per column:
    /// an export of this index writes two and a quarter million rows of five
    /// columns, and a returned `String` is eleven million allocations that do
    /// nothing a cleared buffer does not.
    pub fn row(&self, hit: &Hit, out: &mut Vec<u8>) {
        let mut cell = String::new();
        for (i, id) in self.columns.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            cell.clear();
            cell_of(id, hit, &mut cell);
            escape_into(&cell, out);
        }
        out.extend_from_slice(b"\r\n");
    }
}

/// RFC 4180: quote when the value holds a separator, a quote or a line break,
/// and double any quote inside.
///
/// Filenames contain all three. `a,b.txt` unquoted is two columns; a name with
/// a newline in it — which ext4 allows and something on this machine has done
/// — unquoted is two rows, and every row after it is shifted by one column
/// for the rest of the file.
fn escape_into(value: &str, out: &mut Vec<u8>) {
    if value.contains([',', '"', '\n', '\r']) {
        out.push(b'"');
        for b in value.bytes() {
            if b == b'"' {
                out.push(b'"');
            }
            out.push(b);
        }
        out.push(b'"');
    } else {
        out.extend_from_slice(value.as_bytes());
    }
}

/// The same rule, when a `String` is what the caller has.
pub fn escape(value: &str) -> String {
    let mut out = Vec::with_capacity(value.len() + 2);
    escape_into(value, &mut out);
    String::from_utf8(out).unwrap_or_default()
}

/// What a column is called, and how to read it off a hit.
///
/// Keyed by the same ids the window uses for its columns, so that "the columns
/// on screen, in the order they are on screen" is a list of strings the page
/// already has and does not have to translate.
fn cell_of(id: &str, h: &Hit, out: &mut String) {
    match id {
        "name" => out.push_str(h.name()),
        "path" => out.push_str(h.parent()),
        "full" => out.push_str(&h.path),
        "ext" => out.push_str(&scour_core::ext_of(h.name())),
        "size" => push_num(h.meta.size, out),
        "disk" => push_num(h.meta.disk, out),
        "mtime" => stamp(h.meta.mtime, out),
        "ctime" => stamp(h.meta.ctime, out),
        "atime" => stamp(h.meta.atime, out),
        "kind" => out.push_str(h.kind.token()),
        "perm" => out.push_str(&scour_core::mode_string(h.meta.mode)),
        "user" => out.push_str(&owner(scour_core::Owner::User, h.meta.uid)),
        "group" => out.push_str(&owner(scour_core::Owner::Group, h.meta.gid)),
        "items" => push_num(h.meta.items, out),
        // Not a column this version knows. An empty cell under the heading the
        // caller asked for — see `Sheet::new`.
        _ => {}
    }
}

fn push_num(v: i64, out: &mut String) {
    use std::fmt::Write;
    let _ = write!(out, "{v}");
}

/// Seconds since the epoch, written the way a spreadsheet reads a date.
///
/// `2026-03-07 18:25:13` rather than the window's "5 dk önce": one is for a
/// person glancing at a screen and the other is for a column that will be
/// sorted and filtered by something else. Zero means the filesystem never
/// said, and an empty cell says that better than 1970 does.
fn stamp(t: i64, out: &mut String) {
    use std::fmt::Write;
    if t <= 0 {
        return;
    }
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    let (y, m, d) = civil(days);
    let _ = write!(
        out,
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
        secs / 3600,
        (secs % 3600) / 60,
        secs % 60
    );
}

/// Days since 1970-01-01 to a calendar date.
///
/// Howard Hinnant's `civil_from_days`, which is exact for every date this
/// index can hold and needs no dependency. A date crate would be the fifth
/// thing in the workspace's dependency tree for fourteen lines of arithmetic.
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// The name behind a numeric id, from this machine.
///
/// [`scour_core::owner_name`]'s answer. The copy that used to be here was
/// identical to the browser bridge's, comment included.
fn owner(which: scour_core::Owner, id: i64) -> String {
    scour_core::owner_name(which, id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use scour_core::{EntryId, Kind, Meta, SourceId};

    fn hit(path: &str) -> Hit {
        Hit {
            id: EntryId::path_hash(SourceId(0), path),
            path: path.to_owned(),
            is_dir: false,
            kind: Kind::File,
            meta: Meta {
                size: 12,
                mtime: 1_772_907_913,
                ..Meta::UNKNOWN
            },
            under: None,
        }
    }

    fn text(sheet: &Sheet, rows: &[Hit]) -> String {
        let mut out = Vec::new();
        sheet.header(&mut out);
        for h in rows {
            sheet.row(h, &mut out);
        }
        String::from_utf8(out).expect("utf-8")
    }

    #[test]
    fn a_file_starts_with_the_mark_that_stops_the_encoding_being_guessed() {
        let mut out = Vec::new();
        Sheet::new(["name"]).header(&mut out);
        assert_eq!(&out[..3], &BOM, "the byte-order mark comes first");
    }

    /// The three characters a filename is allowed to contain and a CSV is not.
    ///
    /// Each of them on its own, because the failures are different: a comma
    /// splits the row into an extra column, a quote ends the field early, and a
    /// newline splits it into an extra *row* — and that last one shifts every
    /// line after it, so a file with one such name is wrong from there to the
    /// end rather than wrong in one place.
    #[test]
    fn the_three_characters_a_filename_may_hold_are_quoted() {
        let sheet = Sheet::new(["name"]);
        let mut out = Vec::new();
        for (name, want) in [
            ("a,b.txt", "\"a,b.txt\""),
            ("a\"b.txt", "\"a\"\"b.txt\""),
            ("a\nb.txt", "\"a\nb.txt\""),
            ("a\rb.txt", "\"a\rb.txt\""),
            ("plain.txt", "plain.txt"),
        ] {
            out.clear();
            sheet.row(&hit(&format!("/x/{name}")), &mut out);
            assert_eq!(
                String::from_utf8(out.clone()).expect("utf-8"),
                format!("{want}\r\n"),
                "{name}"
            );
        }
    }

    #[test]
    fn a_date_is_written_for_something_that_will_sort_it() {
        let mut out = String::new();
        stamp(1_772_907_913, &mut out);
        assert_eq!(out, "2026-03-07 18:25:13");
    }

    /// Zero is not 1970. The filesystem never said, and a blank says so.
    #[test]
    fn a_date_nobody_recorded_is_blank_rather_than_the_epoch() {
        let mut out = String::new();
        stamp(0, &mut out);
        assert!(out.is_empty());
        stamp(-1, &mut out);
        assert!(out.is_empty());
    }

    #[test]
    fn the_columns_are_the_callers_in_the_callers_order() {
        let sheet = Sheet::new(["kind", "size", "name"]);
        let text = text(&sheet, &[hit("/x/a.txt")]);
        let mut lines = text.lines();
        assert_eq!(
            lines.next().expect("header").trim_start_matches('\u{feff}'),
            "kind,size,name"
        );
        assert_eq!(lines.next().expect("row"), "file,12,a.txt");
    }

    /// A name this version does not know writes an empty cell under its own
    /// heading rather than dropping the column — otherwise a frontend that
    /// grew a column would silently produce rows one field short of its header.
    #[test]
    fn an_unknown_column_keeps_its_place() {
        let sheet = Sheet::new(["name", "phase-of-moon", "size"]);
        let text = text(&sheet, &[hit("/x/a.txt")]);
        let row = text.lines().nth(1).expect("row");
        assert_eq!(row, "a.txt,,12");
        assert_eq!(row.split(',').count(), sheet.columns().len());
    }

    #[test]
    fn no_columns_still_produces_a_line_per_row() {
        let sheet = Sheet::new(Vec::<String>::new());
        let text = text(&sheet, &[hit("/x/a.txt"), hit("/x/b.txt")]);
        assert_eq!(text.lines().count(), 3, "a header and two rows");
    }
}
