//! Cutting a path up the way every face cuts it.
//!
//! Three windows show the same path three ways — a leaf in the name column,
//! the folder beside it, a trail of steps across the top of the report — and
//! each of them had written its own rule for where to cut. They agree here
//! instead.
//!
//! Slash-separated throughout, because that is what the index stores.

/// The last component: what a file or folder is called.
///
/// A trailing slash is not a component, and a path that is nothing but slashes
/// is its own name — there is nothing else to call it.
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

/// Everything above the leaf: the folder a row sits in.
///
/// `/` for something at the root, and empty for a bare name with no path in
/// it at all — which is a different answer, and a column that showed `/` for
/// it would be claiming something untrue.
pub fn folder(path: &str) -> &str {
    let trimmed = path.trim_end_matches('/');
    match trimmed.rsplit_once('/') {
        Some(("", _)) => "/",
        Some((up, _)) => up,
        None => "",
    }
}

/// The steps of a path, each with the path that reaches it.
///
/// `("Everything", "")` first — the whole index is where a trail starts, and
/// pressing it is how somebody gets back out of a folder. The label of that
/// first step is the caller's, because it is a word from the catalogue and
/// this crate does not speak.
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
