//! What `:` offers in the content field: a piece to start where the caret
//! is, or the case to flip. Like the query's launcher, the letters after `:`
//! narrow it, and when none is left the `:` was only a `:`.

/// What choosing an item does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemAction {
    /// Writes `text` in place of `:` and its letters; the caret lands
    /// `caret` characters into it.
    Write { text: &'static str, caret: usize },
    /// Flips whether capitals must match; the field loses the `:`.
    Case,
}

/// One line of the content field's launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Item {
    /// The word a test or a page's class names it by.
    pub token: &'static str,
    /// The msgids that name it and say what it does.
    pub label: &'static str,
    pub about: &'static str,
    /// The operator it writes, as the line shows it.
    pub sign: &'static str,
    /// `quote`, `or`, `not`, `approx`, `case`.
    pub icon: &'static str,
    pub action: ItemAction,
}

const ITEMS: [Item; 5] = [
    Item {
        token: "phrase",
        label: "Exact phrase",
        about: "these words, in this order",
        sign: "\"…\"",
        icon: "quote",
        action: ItemAction::Write {
            text: "\"\"",
            caret: 1,
        },
    },
    Item {
        token: "or",
        label: "Either",
        about: "this word or the next",
        sign: "|",
        icon: "or",
        action: ItemAction::Write {
            text: "|",
            caret: 1,
        },
    },
    Item {
        token: "not",
        label: "Without",
        about: "files that do not hold the word",
        sign: "!",
        icon: "not",
        action: ItemAction::Write {
            text: "!",
            caret: 1,
        },
    },
    Item {
        token: "similar",
        label: "Similar spelling",
        about: "one letter off still matches",
        sign: "~",
        icon: "approx",
        action: ItemAction::Write {
            text: "~",
            caret: 1,
        },
    },
    Item {
        token: "case",
        label: "Match case",
        about: "capitals as typed",
        sign: "Aa",
        icon: "case",
        action: ItemAction::Case,
    },
];

/// Folded as a typed name is matched: case aside, dotted and dotless i one.
fn fold(s: &str) -> String {
    s.chars()
        .map(|c| {
            if matches!(c, 'İ' | 'I' | 'ı') {
                'i'
            } else {
                c
            }
        })
        .collect::<String>()
        .to_lowercase()
}

/// The `:` the caret is asking with: where it stands and the letters typed
/// after it. Never inside quotes, and only letters between it and the caret.
pub fn asking(input: &str, caret: usize) -> Option<(usize, String)> {
    let chars: Vec<char> = input.chars().collect();
    let caret = caret.min(chars.len());
    let mut at = caret;
    while at > 0 && chars[at - 1].is_alphabetic() {
        at -= 1;
    }
    if at == 0 || chars[at - 1] != ':' {
        return None;
    }
    let colon = at - 1;
    let quotes = chars[..colon].iter().filter(|&&c| c == '"').count();
    if quotes % 2 == 1 {
        return None;
    }
    Some((colon, chars[at..caret].iter().collect()))
}

/// The items `typed` keeps, matched against the token and the name a person
/// reads (`name` translates a msgid). None kept: the `:` is text.
pub fn items(typed: &str, name: impl Fn(&str) -> String) -> Vec<Item> {
    let want = fold(typed);
    ITEMS
        .iter()
        .filter(|i| {
            want.is_empty() || i.token.starts_with(&want) || fold(&name(i.label)).starts_with(&want)
        })
        .copied()
        .collect()
}

/// `input` with `item` chosen for the `:` at `colon` and the letters up to
/// `caret`: the text and where the caret goes. `Either` glues to the word
/// before it, as `a|b` is written.
pub fn choose(input: &str, colon: usize, caret: usize, item: &Item) -> (String, usize) {
    let chars: Vec<char> = input.chars().collect();
    let caret = caret.min(chars.len()).max(colon + 1);
    let mut head: String = chars[..colon].iter().collect();
    let tail: String = chars[caret..].iter().collect();
    let (text, into) = match item.action {
        ItemAction::Write { text, caret } => (text, caret),
        ItemAction::Case => ("", 0),
    };
    if item.token == "or" {
        let kept = head.trim_end().len();
        head.truncate(kept);
    }
    let at = head.chars().count() + into;
    (format!("{head}{text}{tail}"), at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn same(s: &str) -> String {
        s.to_string()
    }

    #[test]
    fn a_colon_asks_only_outside_quotes_and_before_letters() {
        assert_eq!(asking("rapor :", 7), Some((6, String::new())));
        assert_eq!(asking("rapor :ben", 10), Some((6, "ben".into())));
        assert_eq!(asking(":", 1), Some((0, String::new())));
        assert_eq!(asking("\"a :", 4), None);
        assert_eq!(asking("10:3", 4), None);
        assert_eq!(asking("rapor", 5), None);
    }

    #[test]
    fn letters_narrow_it_and_nothing_left_means_text() {
        assert_eq!(items("", same).len(), 5);
        let only: Vec<&str> = items("sim", same).iter().map(|i| i.token).collect();
        assert_eq!(only, ["similar"]);
        let tr = |m: &str| match m {
            "Similar spelling" => "Benzer yazım".to_string(),
            "Without" => "Değil".to_string(),
            m => m.to_string(),
        };
        let only: Vec<&str> = items("BEN", tr).iter().map(|i| i.token).collect();
        assert_eq!(only, ["similar"]);
        assert!(items("xyz", same).is_empty());
    }

    #[test]
    fn choosing_writes_in_place_of_the_colon() {
        let by = |t: &str| *ITEMS.iter().find(|i| i.token == t).unwrap();
        assert_eq!(
            choose("rapor :", 6, 7, &by("phrase")),
            ("rapor \"\"".into(), 7)
        );
        assert_eq!(choose("rapor :ve", 6, 9, &by("or")), ("rapor|".into(), 6));
        assert_eq!(choose(":ben x", 0, 4, &by("similar")), ("~ x".into(), 1));
        assert_eq!(choose("a :c", 2, 4, &by("case")), ("a ".into(), 2));
    }
}
