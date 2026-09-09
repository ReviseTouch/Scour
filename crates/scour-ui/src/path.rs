//! Cutting a path up the way every face cuts it: a leaf, the folder beside it,
//! a trail of steps. Slash-separated throughout, as the index stores it.

/// The last component: what a file or folder is called. A trailing slash is not
/// a component, and a path of nothing but slashes is its own name.
pub fn leaf(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        return path;
    }
    match trimmed.rsplit_once('/') {
        Some((_, leaf)) if !leaf.is_empty() => leaf,
        _ => trimmed,
    }
}

/// Everything above the leaf: the folder a row sits in. `/` at the root, and
/// empty for a bare name with no path in it, which is a different answer.
pub fn folder(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rsplit_once('/') {
        Some(("", _)) => "/",
        Some((up, _)) => up,
        None => "",
    }
}

/// The steps of a path, each with the path that reaches it. `("Everything", "")`
/// comes first; the caller supplies that label, as it is a catalogue word.
///
/// ```
/// # use scour_ui::path::steps;
/// let trail = steps("/home/hasan/Belgeler", "Everything");
/// assert_eq!(trail[0], ("Everything".to_string(), String::new()));
/// assert_eq!(trail[2], ("hasan".to_string(), "/home/hasan".to_string()));
/// ```
pub fn steps(path: &str, everything: &str) -> Vec<(String, String)> {
    let mut out = vec![(everything.to_string(), String::new())];
    let mut walked = String::new();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        walked.push('/');
        walked.push_str(part);
        out.push((part.to_string(), walked.clone()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_leaf_is_the_last_thing_that_is_not_a_slash() {
        assert_eq!(leaf("/home/hasan/x.txt"), "x.txt");
        assert_eq!(
            leaf("/home/hasan/"),
            "hasan",
            "a trailing slash is not a name"
        );
        assert_eq!(leaf("x.txt"), "x.txt");
        assert_eq!(leaf("/"), "/", "the root is called that");
        assert_eq!(leaf(""), "");
    }

    #[test]
    fn a_folder_stops_short_of_the_leaf() {
        assert_eq!(folder("/home/hasan/x.txt"), "/home/hasan");
        assert_eq!(folder("/x.txt"), "/", "at the root, the folder is the root");
        assert_eq!(
            folder("x.txt"),
            "",
            "a bare name is in no folder this can name"
        );
        assert_eq!(folder("/home/hasan/"), "/home");
    }

    #[test]
    fn the_trail_starts_at_everything_and_carries_where_each_step_goes() {
        let trail = steps("/home/hasan/Belgeler", "Hepsi");
        assert_eq!(
            trail,
            vec![
                ("Hepsi".into(), "".into()),
                ("home".into(), "/home".into()),
                ("hasan".into(), "/home/hasan".into()),
                ("Belgeler".into(), "/home/hasan/Belgeler".into()),
            ]
        );
        assert_eq!(steps("", "Hepsi").len(), 1, "the root is one step");
        assert_eq!(
            steps("//home//", "Hepsi").len(),
            2,
            "empty parts are not steps"
        );
    }
}
