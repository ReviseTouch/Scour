//! Just enough gettext to read a `.po` file.
//!
//! Enough, and not more: `msgid`, `msgstr`, continuation lines, escapes, and
//! comments. No plural forms, no contexts, no obsolete entries. Adding them
//! when something needs them is a small change; carrying a full gettext
//! implementation for strings that are almost all "Folder" and "modified" is
//! not.
//!
//! An entry whose translation is empty is dropped rather than stored, so a
//! half-finished catalogue falls back to English per string instead of showing
//! blanks.

use std::collections::HashMap;

pub fn parse(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut id = String::new();
    let mut str_ = String::new();
    // Which of the two the continuation lines belong to.
    let mut in_msgstr = false;
    let mut have = false;

    let mut flush = |id: &mut String, s: &mut String, have: &mut bool| {
        if *have && !id.is_empty() && !s.is_empty() {
            out.insert(std::mem::take(id), std::mem::take(s));
        } else {
            id.clear();
            s.clear();
        }
        *have = false;
    };

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(rest) = line.strip_prefix("msgid ") {
            flush(&mut id, &mut str_, &mut have);
            id = unquote(rest);
            in_msgstr = false;
            have = true;
        } else if let Some(rest) = line.strip_prefix("msgstr ") {
            str_ = unquote(rest);
            in_msgstr = true;
        } else if line.starts_with('"') {
            let piece = unquote(line);
            if in_msgstr {
                str_.push_str(&piece);
            } else {
                id.push_str(&piece);
            }
        }
    }
    flush(&mut id, &mut str_, &mut have);
    out
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    let inner = s
        .strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .unwrap_or(s);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') => out.push('\\'),
            Some('"') => out.push('"'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_catalogue_parses() {
        let m = parse(
            r#"
# a comment
msgid ""
msgstr "Content-Type: text/plain\n"

msgid "Folder"
msgstr "Klasör"

msgid "modified"
msgstr "değiştirilme"
"#,
        );
        assert_eq!(m.get("Folder").map(String::as_str), Some("Klasör"));
        assert_eq!(m.get("modified").map(String::as_str), Some("değiştirilme"));
        assert!(!m.contains_key(""), "the header is not a message");
    }

    #[test]
    fn continuation_lines_are_joined() {
        let m = parse(
            r#"
msgid ""
"The index is empty. "
"Run a scan."
msgstr ""
"Dizin boş. "
"Bir tarama başlatın."
"#,
        );
        assert_eq!(
            m.get("The index is empty. Run a scan.").map(String::as_str),
            Some("Dizin boş. Bir tarama başlatın.")
        );
    }

    #[test]
    fn an_untranslated_entry_is_dropped_rather_than_stored_empty() {
        // Otherwise a half-finished catalogue shows blanks where English would
        // have been correct.
        let m = parse("msgid \"Folder\"\nmsgstr \"\"\n");
        assert!(m.is_empty());
    }

    #[test]
    fn escapes_survive() {
        // Written with explicit escapes rather than a raw string: a `.po` file
        // is full of quotes and backslashes, and a test whose own literal is
        // ambiguous proves nothing about the parser.
        let text = "msgid \"a\\nb\"\nmsgstr \"c\\td\\\"e\"\n";
        let m = parse(text);
        assert_eq!(m.get("a\nb").map(String::as_str), Some("c\td\"e"));
    }
}
