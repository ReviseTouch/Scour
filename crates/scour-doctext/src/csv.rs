//! A comma- or tab-separated file as a sheet: RFC 4180's quoting, the
//! separator the first lines agree on (`,` `;` tab or `|`), numbers to the
//! right and written as the file writes them, and a first row of words over
//! rows of numbers held at the top as the heading.

use scour_flow::{Align, Block, Flow, GridCell, GridRow};

/// The separators tried, in this order: a tab or a semicolon splitting every
/// line alike is meant, where a comma may be a decimal one (`1,5;2,5`).
const SEPARATORS: [u8; 4] = *b"\t;,|";
/// Lines looked at to choose one.
const SAMPLE_LINES: usize = 20;

/// The file in `bytes`; `extension` says `tsv` when the tab is meant.
pub fn flow(bytes: &[u8], extension: &str) -> Result<Flow, String> {
    let text = String::from_utf8_lossy(bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let separator = if extension.eq_ignore_ascii_case("tsv") {
        b'\t'
    } else {
        separator_of(text)
    };
    let mut rows = Vec::new();
    let mut cut = false;
    for record in Records::new(text, separator) {
        if rows.len() > scour_flow::MOST_SHEET_ROWS {
            cut = true;
            break;
        }
        rows.push(GridRow {
            cells: record.into_iter().map(cell).collect(),
        });
    }
    heading(&mut rows);
    let mut sheet = crate::flow::finish_sheet(String::new(), rows, Vec::new());
    sheet.cut |= cut;
    Ok(Flow {
        blocks: vec![Block::Sheet(sheet)],
        cut: false,
    }
    .bounded())
}

/// The separator that splits every one of the first lines the same number
/// of times, the first in [`SEPARATORS`]' order; failing that, the one most
/// lines agree on.
fn separator_of(text: &str) -> u8 {
    let lines: Vec<&str> = text
        .lines()
        .filter(|l| !l.is_empty())
        .take(SAMPLE_LINES)
        .collect();
    let mut best = (0usize, b',');
    for sep in SEPARATORS {
        let counts: Vec<usize> = lines
            .iter()
            .map(|l| Records::new(l, sep).next().map_or(0, |r| r.len() - 1))
            .collect();
        let Some(&first) = counts.first() else {
            continue;
        };
        if first == 0 {
            continue;
        }
        let even = counts.iter().filter(|&&c| c == first).count();
        if even == counts.len() {
            return sep;
        }
        if even > best.0 {
            best = (even, sep);
        }
    }
    best.1
}

/// A number, as a sheet would align it: digits with one decimal point, a
/// sign, a percent, grouping commas or spaces.
fn numeric(text: &str) -> bool {
    let t = text.trim();
    let t = t.strip_suffix('%').unwrap_or(t);
    let t = t.strip_prefix(['-', '+']).unwrap_or(t);
    !t.is_empty()
        && t.chars().any(|c| c.is_ascii_digit())
        && t.chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | ',' | ' ' | '\u{a0}' | '_'))
}

fn cell(text: String) -> GridCell {
    let number = numeric(&text);
    GridCell {
        text: text.chars().take(scour_flow::MOST_CELL_TEXT).collect(),
        align: number.then_some(Align::Right),
        ..GridCell::default()
    }
}

/// A first row of words over a row with numbers is the sheet's heading.
fn heading(rows: &mut [GridRow]) {
    let [first, second, ..] = rows else {
        return;
    };
    let words = first.cells.iter().all(|c| c.align.is_none());
    let numbers = second.cells.iter().any(|c| c.align.is_some());
    let named = first
        .cells
        .iter()
        .filter(|c| !c.text.trim().is_empty())
        .count()
        * 2
        >= first.cells.len();
    if words && (numbers || named) {
        for c in &mut first.cells {
            c.bold = true;
            c.shade = Some([0xf6, 0xf8, 0xfa]);
        }
    }
}

/// The records of a separated text, quoted fields across lines included.
struct Records<'a> {
    rest: &'a str,
    separator: u8,
}

impl<'a> Records<'a> {
    fn new(text: &'a str, separator: u8) -> Self {
        Records {
            rest: text,
            separator,
        }
    }

    /// A field ended at `end`: kept while the record has room for it. Past
    /// the sheet's columns a field is passed over unread — a row of 1,602
    /// fields was 70 ms of strings nobody would see.
    fn keep(&self, fields: &mut Vec<String>, field: &mut String, start: usize, end: usize) {
        if fields.len() < MOST_FIELDS {
            field.push_str(&self.rest[start..end]);
            fields.push(std::mem::take(field));
        } else {
            field.clear();
        }
    }
}

/// Fields kept of a record: a column past the sheet's bounds is cut anyway.
const MOST_FIELDS: usize = scour_flow::MOST_SHEET_COLUMNS + 1;

impl Iterator for Records<'_> {
    type Item = Vec<String>;

    fn next(&mut self) -> Option<Vec<String>> {
        if self.rest.is_empty() {
            return None;
        }
        let bytes = self.rest.as_bytes();
        let sep = self.separator;
        let mut fields = Vec::new();
        let mut field = String::new();
        let mut i = 0;
        let mut quoted = false;
        let mut start = 0;
        loop {
            if i >= bytes.len() {
                self.keep(&mut fields, &mut field, start, i);
                self.rest = "";
                return Some(fields);
            }
            let b = bytes[i];
            if quoted {
                if b == b'"' {
                    if fields.len() < MOST_FIELDS {
                        field.push_str(&self.rest[start..i]);
                    }
                    if bytes.get(i + 1) == Some(&b'"') {
                        field.push('"');
                        i += 2;
                    } else {
                        quoted = false;
                        i += 1;
                    }
                    start = i;
                    continue;
                }
                i += 1;
                continue;
            }
            match b {
                b'"' if i == start && field.is_empty() => {
                    quoted = true;
                    i += 1;
                    start = i;
                }
                b'\n' | b'\r' => {
                    self.keep(&mut fields, &mut field, start, i);
                    let mut end = i + 1;
                    if b == b'\r' && bytes.get(end) == Some(&b'\n') {
                        end += 1;
                    }
                    self.rest = &self.rest[end..];
                    return Some(fields);
                }
                _ if b == sep => {
                    self.keep(&mut fields, &mut field, start, i);
                    i += 1;
                    start = i;
                }
                _ => i += 1,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grid(text: &str, ext: &str) -> scour_flow::Sheet {
        match flow(text.as_bytes(), ext)
            .unwrap()
            .blocks
            .into_iter()
            .next()
        {
            Some(Block::Sheet(s)) => s,
            other => panic!("a sheet: {other:?}"),
        }
    }

    fn texts(s: &scour_flow::Sheet) -> Vec<Vec<String>> {
        s.rows
            .iter()
            .map(|r| r.cells.iter().map(|c| c.text.clone()).collect())
            .collect()
    }

    #[test]
    fn quoting_as_rfc_4180_has_it() {
        let s = grid("a,\"b, c\",\"d \"\"e\"\"\"\r\n1,\"iki\nsatır\",3\n", "csv");
        assert_eq!(
            texts(&s),
            [vec!["a", "b, c", "d \"e\""], vec!["1", "iki\nsatır", "3"]]
        );
    }

    #[test]
    fn the_separator_is_the_one_the_lines_agree_on() {
        assert_eq!(separator_of("ad;soyad;yaş\nAli;Veli;3\n"), b';');
        assert_eq!(separator_of("a\tb\tc\n1\t2\t3\n"), b'\t');
        assert_eq!(separator_of("1,5;2,5\n3,5;4,5\n"), b';');
        assert_eq!(grid("x\ty\n1\t2\n", "tsv").rows[1].cells.len(), 2);
    }

    #[test]
    fn numbers_are_right_and_a_row_of_names_is_the_heading() {
        let s = grid("ad,tutar\nelma,\"1,250.50\"\narmut,-3%\nkiraz,12\n", "csv");
        assert!(s.frozen && s.rows[0].cells[0].bold);
        assert_eq!(s.rows[1].cells[1].align, Some(Align::Right));
        assert_eq!(s.rows[2].cells[1].align, Some(Align::Right));
        assert_eq!(s.rows[1].cells[0].align, None);
        assert_eq!(s.rows[1].cells[1].text, "1,250.50");
    }
}
