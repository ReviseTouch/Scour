//! Wildcard matching, anchored end to end.
//!
//! `*.rs` matches `main.rs` and not `main.rst`; a pattern with no wildcard is an
//! exact-name test, not a substring one. Callers pass case-folded input — this
//! compares what it is given, so every index answers the question the same way.

/// Does `text` match `pattern`, where `*` is any run and `?` is one character?
///
/// Iterative with backtracking: a user pattern like `*a*a*a*a*b` against a long
/// run of `a` blows a recursive matcher's stack.
pub fn glob_matches(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    // Where to resume if the current `*` turns out to have consumed too little.
    let mut star: Option<usize> = None;
    let mut resume = 0usize;

    while ti < t.len() {
        match p.get(pi) {
            Some('*') => {
                star = Some(pi);
                resume = ti;
                pi += 1;
            }
            Some('?') => {
                pi += 1;
                ti += 1;
            }
            Some(&c) if c == t[ti] => {
                pi += 1;
                ti += 1;
            }
            _ => match star {
                // Give the last `*` one more character and try again.
                Some(s) => {
                    pi = s + 1;
                    resume += 1;
                    ti = resume;
                }
                None => return false,
            },
        }
    }
    // Trailing `*`s may still match the empty rest.
    while p.get(pi) == Some(&'*') {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn patterns_are_anchored() {
        assert!(glob_matches("*.rs", "main.rs"));
        assert!(
            !glob_matches("*.rs", "main.rst"),
            "a pattern matches the whole name"
        );
        assert!(!glob_matches("*.rs", "rs.main"));
        assert!(glob_matches("main*", "main.rs"));
        assert!(glob_matches("*main*", "src.main.rs"));
    }

    #[test]
    fn single_character_wildcard() {
        assert!(glob_matches("a?c", "abc"));
        assert!(!glob_matches("a?c", "ac"));
        assert!(!glob_matches("a?c", "abbc"));
    }

    #[test]
    fn no_wildcard_is_an_exact_test() {
        assert!(glob_matches("main.rs", "main.rs"));
        assert!(!glob_matches("main", "main.rs"));
    }

    #[test]
    fn stars_collapse_and_match_nothing() {
        assert!(glob_matches("*", ""));
        assert!(glob_matches("***", "anything"));
        assert!(glob_matches("a**b", "ab"));
        assert!(glob_matches("", ""));
        assert!(!glob_matches("", "a"));
    }

    #[test]
    fn backtracking_terminates_on_a_pathological_pattern() {
        // A recursive matcher would recurse ~2^n here.
        let text = "a".repeat(64);
        assert!(!glob_matches("*a*a*a*a*a*a*b", &text));
        assert!(glob_matches("*a*a*a*a*a*a*a", &text));
    }

    #[test]
    fn matching_is_by_character_not_by_byte() {
        // 'ş' is two bytes; `?` must consume the letter, not half of it.
        assert!(glob_matches("ç?lış*", "çalışkan"));
        assert!(glob_matches("*ğ*", "değil"));
    }
}
