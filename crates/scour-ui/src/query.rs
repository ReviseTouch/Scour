//! Putting a filter and a typed query together.
//!
//! Every face has a rail beside the list: press "Documents" and the result
//! narrows. What that *does* is add a term to the query — `kind:doc`,
//! `under:"/home/hasan"`, `size:>10mb`, `dm:7d` — and the rules about how are
//! small, shared, and were written twice already.
//!
//! **One filter at a time, appended.** Pressing a second replaces the first,
//! and pressing the active one clears it. Two rails' worth of terms could be
//! combined instead, but then a person has to be able to see and remove each
//! of them — and the place where they are all visible is the query box, which
//! is where somebody who wants two terms can type the second.

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

/// Pressing a filter: the one that is now in force.
///
/// **Pressing the active one clears it.** A filter somebody cannot see how to
/// remove is worse than no filter at all.
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

/// The term a place stands for.
///
/// Quoted, because a path with a space in it is two words to a parser — and
/// `/home/hasan/Belgelerim ve Diğerleri` is an ordinary folder name.
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

#[cfg(test)]
mod tests {
    use super::*;

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
