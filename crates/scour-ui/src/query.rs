//! Putting a filter and a typed query together: pressing a rail bar appends a
//! term (`kind:doc`, `under:"/home/hasan"`, `size:>10mb`, `dm:7d`). One filter
//! at a time — a second replaces the first, the active one clears it, and a
//! second term is typed into the query box where it can be seen and removed.

/// The query the service is asked, given what was typed and what is pressed.
///
/// ```
/// # use scour_ui::query::compose;
/// assert_eq!(compose("rapor", Some("kind:doc")), "rapor kind:doc");
/// assert_eq!(compose("  ", Some("kind:doc")), "kind:doc");
/// assert_eq!(compose("rapor", None), "rapor");
/// ```
pub fn compose(typed: &str, filter: Option<&str>) -> String {
    let typed = typed.trim();
    match filter {
        Some(term) if typed.is_empty() => term.to_string(),
        Some(term) => format!("{typed} {term}"),
        None => typed.to_string(),
    }
}

/// Pressing a filter: the one that is now in force. Pressing the active one
/// clears it, since a filter nobody can see how to remove is worse than none.
pub fn pressed(current: Option<&str>, term: &str) -> Option<String> {
    if current == Some(term) {
        None
    } else {
        Some(term.to_string())
    }
}

/// The term a kind bar stands for.
pub fn of_kind(token: &str) -> String {
    format!("kind:{token}")
}

/// The term a place stands for. Quoted: a path with a space in it is two words
/// to the parser.
pub fn of_place(path: &str) -> String {
    format!("under:\"{path}\"")
}

/// The term a bar of the time strip stands for: everything changed in the last
/// `days` days.
pub fn of_age(days: u32) -> String {
    format!("dm:{days}d")
}

/// The three size bands the rail offers, as the page names them.
pub const SIZES: [(&str, &str); 3] = [
    ("> 10 MB", "size:>10mb"),
    ("> 1 MB", "size:>1mb"),
    ("= 0", "size:0"),
];

/// The terms of a query as typed: split on spaces outside double quotes, so
/// `under:"/home/u/My Files"` is one term.
pub fn terms(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut start, mut quoted) = (None, false);
    for (i, c) in text.char_indices() {
        match c {
            '"' => {
                quoted = !quoted;
                start.get_or_insert(i);
            }
            c if c.is_whitespace() && !quoted => {
                if let Some(s) = start.take() {
                    out.push(&text[s..i]);
                }
            }
            _ => {
                start.get_or_insert(i);
            }
        }
    }
    if let Some(s) = start {
        out.push(&text[s..]);
    }
    out
}

/// The field a term names, if it names one: letters before a colon, so a
/// time like `10:30` is a word.
fn field(term: &str) -> Option<String> {
    let bare = term.strip_prefix('!').unwrap_or(term);
    let (f, _) = bare.split_once(':')?;
    (!f.is_empty() && f.chars().all(char::is_alphabetic)).then(|| f.to_lowercase())
}

/// The terms that name a field — `kind:doc`, `!ext:log` — each shown as a
/// chip under the query, whose `×` takes it out with [`toggle`].
pub fn filters(text: &str) -> Vec<&str> {
    terms(text)
        .into_iter()
        .filter(|t| field(t).is_some())
        .collect()
}

/// Whether this filter is in the query, written as a term of its own.
pub fn has_term(text: &str, term: &str) -> bool {
    terms(text).iter().any(|t| t.eq_ignore_ascii_case(term))
}

/// Pressing a filter, as the page does: the term goes into the text, where it
/// can be seen and deleted. One per field — another value replaces it, the
/// same one takes it out.
///
/// ```
/// # use scour_ui::query::toggle;
/// assert_eq!(toggle("rapor", "kind:doc"), "rapor kind:doc");
/// assert_eq!(toggle("rapor kind:doc", "kind:image"), "rapor kind:image");
/// assert_eq!(toggle("rapor kind:doc", "kind:doc"), "rapor");
/// ```
pub fn toggle(text: &str, term: &str) -> String {
    let mut parts: Vec<&str> = terms(text);
    let want = field(term);
    match parts
        .iter()
        .position(|t| want.is_some() && field(t) == want)
    {
        Some(at) if parts[at].eq_ignore_ascii_case(term) => {
            parts.remove(at);
        }
        Some(at) => parts[at] = term,
        None => parts.push(term),
    }
    parts.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_quoted_place_is_one_term_and_toggles_as_one() {
        let q = r#"rapor under:"/home/u/My Files" size:>1mb"#;
        assert_eq!(
            terms(q),
            ["rapor", r#"under:"/home/u/My Files""#, "size:>1mb"]
        );
        assert!(has_term(q, r#"under:"/home/u/My Files""#));
        assert!(!has_term(q, "kind:doc"));
        assert_eq!(toggle(q, r#"under:"/home/u/My Files""#), "rapor size:>1mb");
        assert_eq!(
            toggle(q, r#"under:"/srv""#),
            r#"rapor under:"/srv" size:>1mb"#
        );
        assert_eq!(toggle("", "dm:7d"), "dm:7d");
        assert_eq!(filters(q), [r#"under:"/home/u/My Files""#, "size:>1mb"]);
        assert_eq!(filters("rapor !ext:log 10:30"), ["!ext:log"]);
    }

    #[test]
    fn a_filter_is_appended_to_what_was_typed() {
        assert_eq!(compose("rapor", Some("kind:doc")), "rapor kind:doc");
        assert_eq!(compose("", Some("kind:doc")), "kind:doc");
        assert_eq!(compose("  rapor  ", None), "rapor");
        assert_eq!(compose("", None), "");
    }

    #[test]
    fn pressing_what_is_already_pressed_clears_it() {
        assert_eq!(pressed(None, "kind:doc").as_deref(), Some("kind:doc"));
        assert_eq!(pressed(Some("kind:doc"), "kind:doc"), None);
        assert_eq!(
            pressed(Some("kind:doc"), "kind:image").as_deref(),
            Some("kind:image")
        );
    }

    #[test]
    fn a_path_with_a_space_in_it_is_still_one_path() {
        assert_eq!(
            of_place("/home/x/Belgelerim ve Diğerleri"),
            "under:\"/home/x/Belgelerim ve Diğerleri\""
        );
        assert_eq!(of_kind("code"), "kind:code");
        assert_eq!(of_age(7), "dm:7d");
    }
}
