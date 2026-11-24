/// A `:` starting a filter is not marked as misread: it is left out.
#[test]
fn a_filter_being_started_is_not_a_warning() {
    for q in ["kind:doc :", "kind:doc :ko"] {
        let spans = spans_at(q, 1_800_000_000);
        assert!(spans.iter().all(|s| !s.role.is_warning()), "{q}: {spans:?}");
    }
}

/// An empty `content:""` is not marked as misread: it is left out.
#[test]
fn an_empty_content_term_is_not_a_warning() {
    let spans = spans_at("rapor content:\"\"", 1_800_000_000);
    assert!(spans.iter().all(|s| !s.role.is_warning()), "{spans:?}");
    assert!(spans.iter().filter(|s| s.role == Role::Quote).count() == 2);
}
use super::*;

fn roles(q: &str) -> Vec<(Role, &str)> {
    spans_at(q, 1_800_000_000)
        .into_iter()
        .map(|s| (s.role, s.of(q)))
        .collect()
}

/// The invariant: put the spans back together and the query comes back.
fn covers(q: &str) {
    let spans = spans_at(q, 1_800_000_000);
    let mut at = 0;
    let mut rebuilt = String::new();
    for s in &spans {
        assert_eq!(s.start as usize, at, "gap or overlap in {q:?}: {spans:?}");
        rebuilt.push_str(s.of(q));
        at += s.len as usize;
    }
    assert_eq!(at, q.len(), "spans stop short of the end of {q:?}");
    assert_eq!(rebuilt, q);
}

#[test]
fn plain_words_and_the_space_between_them() {
    assert_eq!(
        roles("rapor pdf"),
        vec![
            (Role::Text, "rapor"),
            (Role::Space, " "),
            (Role::Text, "pdf")
        ]
    );
    covers("rapor pdf");
    covers("  leading and trailing   ");
}

#[test]
fn a_field_is_three_pieces() {
    assert_eq!(
        roles("ext:pdf"),
        vec![
            (Role::Field, "ext"),
            (Role::Colon, ":"),
            (Role::Value, "pdf")
        ]
    );
    covers("ext:pdf");
}

#[test]
fn a_value_the_field_cannot_read_is_marked() {
    // The case the module is for: the parser searches for these as text.
    assert_eq!(
        roles("kind:zurna"),
        vec![
            (Role::Field, "kind"),
            (Role::Colon, ":"),
            (Role::BadValue, "zurna")
        ]
    );
    assert_eq!(
        roles("size:abc"),
        vec![
            (Role::Field, "size"),
            (Role::Colon, ":"),
            (Role::BadValue, "abc")
        ]
    );
    assert_eq!(
        roles("dm:yarin"),
        vec![
            (Role::Field, "dm"),
            (Role::Colon, ":"),
            (Role::BadValue, "yarin")
        ]
    );
    covers("kind:zurna");
}

#[test]
fn a_field_that_does_not_exist_is_the_whole_term() {
    assert_eq!(roles("boyut:1mb"), vec![(Role::UnknownField, "boyut:1mb")]);
    covers("boyut:1mb");
}

#[test]
fn a_field_name_is_case_insensitive_and_coloured_as_one() {
    // Field names are folded, so this is the size field, not a mistake.
    assert_eq!(
        roles("sizE:>1mb"),
        vec![
            (Role::Field, "sizE"),
            (Role::Colon, ":"),
            (Role::Cmp, ">"),
            (Role::Value, "1mb")
        ]
    );
}

#[test]
fn windows_paths_and_urls_are_not_fields() {
    // One letter is not a field name, so this is ordinary text.
    assert_eq!(roles("C:/Users"), vec![(Role::Text, "C:/Users")]);
    assert_eq!(
        roles("http://example"),
        vec![(Role::UnknownField, "http://example")]
    );
}

#[test]
fn comparisons_and_separators_are_syntax() {
    assert_eq!(
        roles("size:>1mb"),
        vec![
            (Role::Field, "size"),
            (Role::Colon, ":"),
            (Role::Cmp, ">"),
            (Role::Value, "1mb")
        ]
    );
    assert_eq!(
        roles("ext:rs;toml"),
        vec![
            (Role::Field, "ext"),
            (Role::Colon, ":"),
            (Role::Value, "rs"),
            (Role::Sep, ";"),
            (Role::Value, "toml")
        ]
    );
    covers("ext:rs;toml;md");
}

#[test]
fn negation_glob_and_phrase() {
    assert_eq!(roles("!tmp"), vec![(Role::Not, "!"), (Role::Text, "tmp")]);
    assert_eq!(roles("*.rs"), vec![(Role::Glob, "*.rs")]);
    assert_eq!(
        roles("\"iki kelime\""),
        vec![
            (Role::Quote, "\""),
            (Role::Phrase, "iki kelime"),
            (Role::Quote, "\"")
        ]
    );
    covers("\"iki kelime\"");
    // A quote that is still open must still cover the input.
    covers("\"yarim");
}

#[test]
fn a_lone_bang_or_pipe_is_an_operator_but_an_exclaiming_word_is_not() {
    assert_eq!(
        roles("! main"),
        vec![(Role::Not, "!"), (Role::Space, " "), (Role::Text, "main")]
    );
    assert_eq!(roles("hello!"), vec![(Role::Text, "hello!")]);
    assert_eq!(
        roles("a|b"),
        vec![(Role::Text, "a"), (Role::Or, "|"), (Role::Text, "b")]
    );
    covers("a | b");
}

/// The `!` is not the exclusion — the term after it is.
#[test]
fn an_excluded_term_is_excluded_all_the_way_through() {
    fn excluded(q: &str) -> Vec<&str> {
        spans_at(q, 1_800_000_000)
            .into_iter()
            .filter(|s| s.not)
            .map(|s| s.of(q))
            .collect()
    }
    assert_eq!(excluded("!tmp"), vec!["!", "tmp"]);
    assert_eq!(excluded("rapor !tmp"), vec!["!", "tmp"]);
    // A field term: the name, the colon and the value are all inside it.
    assert_eq!(excluded("!ext:pdf"), vec!["!", "ext", ":", "pdf"]);
    // A list carries it past the token it started in.
    assert_eq!(
        excluded("!ext:rs; toml"),
        vec!["!", "ext", ":", "rs", ";", " ", "toml"]
    );
    // One alternative of a group, not the group.
    assert_eq!(excluded("a|!b"), vec!["!", "b"]);
    // A `!` on its own binds to the term after it: `! main` is `!main`.
    assert_eq!(excluded("! main"), vec!["!", " ", "main"]);
    assert_eq!(
        excluded("rapor ! ext:pdf"),
        vec!["!", " ", "ext", ":", "pdf"]
    );
    // A `!` with nothing after it is still only itself.
    assert_eq!(excluded("rapor !"), vec!["!"]);
    // And nothing at all when nothing is excluded.
    assert!(excluded("ext:rs rapor").is_empty());
    assert!(excluded("hello!").is_empty());
}

/// Marking the extent must not move a byte of it.
#[test]
fn exclusion_does_not_disturb_the_roles_or_the_cover() {
    assert_eq!(
        roles("!ext:pdf"),
        vec![
            (Role::Not, "!"),
            (Role::Field, "ext"),
            (Role::Colon, ":"),
            (Role::Value, "pdf")
        ]
    );
    covers("!ext:rs; toml");
    covers("a|!b !kind:zurna");
}

/// `;` between words separates two terms, and is drawn the way a space is.
#[test]
fn a_semicolon_between_words_is_a_separator() {
    assert_eq!(
        roles("HASAN;DENEME"),
        vec![
            (Role::Text, "HASAN"),
            (Role::Space, ";"),
            (Role::Text, "DENEME")
        ]
    );
    covers("HASAN;DENEME");
    assert_eq!(
        roles("a ;b"),
        vec![
            (Role::Text, "a"),
            (Role::Space, " "),
            (Role::Space, ";"),
            (Role::Text, "b")
        ]
    );
    covers("a ;b");
    covers("a ; b");
    // And `|` is still the operator it always was.
    assert_eq!(
        roles("a|b"),
        vec![(Role::Text, "a"), (Role::Or, "|"), (Role::Text, "b")]
    );
}

/// And a field's own list keeps its semicolons.
#[test]
fn a_semicolon_inside_a_list_is_still_a_separator_not_an_operator() {
    assert_eq!(
        roles("ext:rs;toml"),
        vec![
            (Role::Field, "ext"),
            (Role::Colon, ":"),
            (Role::Value, "rs"),
            (Role::Sep, ";"),
            (Role::Value, "toml")
        ]
    );
    assert_eq!(
        roles("!ext:rs;toml"),
        vec![
            (Role::Not, "!"),
            (Role::Field, "ext"),
            (Role::Colon, ":"),
            (Role::Value, "rs"),
            (Role::Sep, ";"),
            (Role::Value, "toml")
        ]
    );
    covers("!ext:rs;toml");
    // A quoted run is literal all the way through.
    assert_eq!(
        roles("\"a;b\""),
        vec![
            (Role::Quote, "\""),
            (Role::Phrase, "a;b"),
            (Role::Quote, "\"")
        ]
    );
}

/// The colouring and the parser read the same query: the same AND-ed terms,
/// alternatives and exclusions. No rewritten spellings in the corpus —
/// `expand` turns one token into several terms, which the line cannot show.
#[test]
fn the_colouring_and_the_parser_read_the_same_query() {
    const NOW: i64 = 1_800_000_000;
    for q in [
        "",
        "rapor",
        "rapor pdf",
        "  leading and trailing   ",
        "!tmp",
        "! main",
        "rapor !tmp",
        "rapor!tmp",
        "a!b!c",
        "hello!",
        "a!",
        "a|b",
        "a|!b",
        "!a|b",
        "a;b",
        "HASAN;DENEME",
        "hasan;genel",
        "a ;b",
        "a; b",
        "a ; b",
        "a;b;c",
        "a;!b",
        "a|b;c",
        "a;b|c",
        "a|;c",
        "; a",
        "! ; a",
        "!ama ;deneme",
        "HASAN;DENEME !ama ;deneme !dfd",
        "ext:pdf",
        "!ext:pdf",
        "ext:rs;toml",
        "!ext:rs;toml",
        "ext:rs; toml",
        "kind:image rapor",
        "kind:zurna",
        "!kind:zurna",
        "sizE:>1mb",
        "\"iki kelime\"",
        "rapor \"iki kelime\"",
        "\"a!b\"",
        "\"a;b\"",
        "*.rs !cache",
        "değiştirme !öğe",
        "path:/home/a!b",
        "under:/home rapor !tmp",
        "*.docx kind:doc;",
        "*.docx kind:doc ;",
        "kind:doc;image ;",
        "size:>1mb;",
        "size:>1mb ; dm:7d ;",
        "folder: ; *.rs ;",
        "\"annual report\";ext:pdf;",
        "ext:rs;!tmp",
        "ext:rs ; !tmp",
        "kind:doc;*.docx",
        "kind:doc ; *.docx",
        ";;;",
    ] {
        agree(q, NOW);
        covers(q);
    }
}

/// The same agreement, over every query a handful of pieces can build.
#[test]
fn the_colouring_and_the_parser_agree_on_everything_these_pieces_can_spell() {
    const NOW: i64 = 1_800_000_000;
    const PIECES: &[&str] = &[
        "a",
        "!b",
        ";c",
        "d;e",
        "f!g",
        "h!",
        "!",
        ";",
        "|",
        "i|j",
        "!k|l",
        "ext:rs",
        "!ext:rs;toml",
        "\"x y\"",
        "*.m",
        "kind:zurna",
    ];
    let mut checked = 0;
    for one in PIECES {
        for two in PIECES {
            for three in PIECES {
                let q = format!("{one} {two} {three}");
                // One shape no colouring can draw truthfully: a lone `!`
                // before a term opening with `|` binds past the operator
                // (`a ! |c` is `a` or `not c`), so it is written on the
                // wrong side of the `or`.
                let toks: Vec<&str> = q.split_whitespace().collect();
                let looks_past = toks
                    .windows(2)
                    .any(|w| w[0] == "!" && w[1].starts_with('|'));
                if looks_past {
                    continue;
                }
                agree(&q, NOW);
                checked += 1;
            }
        }
    }
    assert!(checked > 3500, "the sweep did not run");
}

/// The line and the engine read `q` the same way, or say how they differ.
/// Structure comes off the spans as a person reads them: spaces separate
/// terms, an `or` joins alternatives, a `!` begins one unless it opens one.
fn agree(q: &str, now: i64) {
    let ast = crate::parse::parse_at(q, now);
    let spans = spans_at(q, now);
    let gap = |s: &Span| s.role == Role::Space && !s.not;
    let mut groups: Vec<Vec<Span>> = Vec::new();
    let mut cur: Vec<Span> = Vec::new();
    let mut prev: Option<Role> = None;
    for s in &spans {
        let starts_term = s.role == Role::Not
            && !cur.is_empty()
            && prev != Some(Role::Or)
            && prev != Some(Role::Sep)
            && prev != Some(Role::Space);
        if (gap(s) || starts_term) && !cur.is_empty() {
            groups.push(std::mem::take(&mut cur));
        }
        if !gap(s) {
            cur.push(*s);
        }
        prev = Some(s.role);
    }
    if !cur.is_empty() {
        groups.push(cur);
    }
    // A term made only of operators has nothing to search for: the parser
    // drops it, and the line draws it because someone is mid-word.
    groups.retain(|g| {
        !g.iter()
            .all(|s| matches!(s.role, Role::Not | Role::Or | Role::Space | Role::Sep))
    });
    // A `!` at the end of a term excludes nothing: `a ; !` is one
    // alternative, not two with an empty second.
    for g in &mut groups {
        while g
            .last()
            .is_some_and(|s| matches!(s.role, Role::Not | Role::Space | Role::Sep))
        {
            g.pop();
        }
    }
    assert_eq!(
        groups.len(),
        ast.groups.len(),
        "{q:?}: the line shows {} terms and the engine reads {}",
        groups.len(),
        ast.groups.len()
    );
    for (i, (drawn, read)) in groups.iter().zip(&ast.groups).enumerate() {
        // An `or` with nothing after it, or right after another, joins
        // nothing: `a ; ;c` reads as `a` or `c`.
        let mut alts = 1;
        let mut last: Option<Role> = None;
        for s in drawn {
            if matches!(s.role, Role::Space | Role::Sep) {
                continue;
            }
            if s.role == Role::Or && last.is_some_and(|r| r != Role::Or) {
                alts += 1;
            }
            last = Some(s.role);
        }
        let dangling = last == Some(Role::Or);
        alts -= usize::from(dangling);
        assert_eq!(
            alts,
            read.alts.len(),
            "{q:?}: term {i} is drawn with {alts} alternatives and read with {}",
            read.alts.len()
        );
        // Cut the drawn run at its `or`s; each piece must carry the mark.
        if dangling {
            continue;
        }
        let mut drawn_neg = Vec::new();
        let mut any = false;
        let mut last: Option<Role> = None;
        for s in drawn {
            if matches!(s.role, Role::Space | Role::Sep) {
                any |= s.not;
                continue;
            }
            if s.role == Role::Or {
                // Only an `or` with something in front starts a new one.
                if last.is_some_and(|r| r != Role::Or) {
                    drawn_neg.push(any);
                    any = false;
                }
            } else {
                any |= s.not;
            }
            last = Some(s.role);
        }
        drawn_neg.push(any);
        let read_neg: Vec<bool> = read.alts.iter().map(|(n, _)| *n).collect();
        assert_eq!(
            drawn_neg, read_neg,
            "{q:?}: term {i} — the line marks {drawn_neg:?} excluded, \
                 the engine excludes {read_neg:?}"
        );
    }
    // Byte for byte, whatever else happened.
    let mut at = 0;
    for s in &spans {
        assert_eq!(s.start as usize, at, "gap or overlap in {q:?}");
        at += s.len as usize;
    }
    assert_eq!(at, q.len(), "spans stop short of the end of {q:?}");
}

#[test]
fn an_unfinished_field_is_not_yet_a_mistake() {
    // Someone is mid-word: a warning here fires on the way to every field.
    assert_eq!(
        roles("ext:"),
        vec![(Role::Field, "ext"), (Role::Colon, ":")]
    );
    assert_eq!(
        roles("folder:"),
        vec![(Role::Field, "folder"), (Role::Colon, ":")]
    );
}

#[test]
fn turkish_field_names_and_values_are_recognised() {
    assert_eq!(
        roles("tür:kod"),
        vec![
            (Role::Field, "tür"),
            (Role::Colon, ":"),
            (Role::Value, "kod")
        ]
    );
    covers("tür:kod içerik:gizli");
    assert_eq!(
        roles("TÜR:İMAGE"),
        vec![
            (Role::Field, "TÜR"),
            (Role::Colon, ":"),
            (Role::Value, "İMAGE")
        ],
        "folding is the parser's, so the spans stay on the original bytes"
    );
}

#[test]
fn every_span_lands_on_a_character_boundary() {
    for q in [
        "İSTANBUL",
        "tür:görsel",
        "\"çağlayan iş\"",
        "ıq ext:pdf",
        "içerik:ğ",
    ] {
        covers(q);
        for s in spans_at(q, 0) {
            assert!(
                q.is_char_boundary(s.start as usize)
                    && q.is_char_boundary((s.start + s.len) as usize),
                "{q:?} span {s:?} splits a character"
            );
        }
    }
}

#[test]
fn completions_offer_fields_by_prefix() {
    // Offered, not offered alone: `exe:` shares the first two letters.
    let c = complete("ex", 2);
    assert!(c.iter().any(|x| x.insert == "ext:"), "{c:?}");
    assert!(c.iter().all(|x| x.insert.starts_with("ex")), "{c:?}");
    assert!(
        complete("", 0).len() >= 10,
        "everything, when nothing typed"
    );
    // An alias matches too, and inserts the canonical spelling.
    let c = complete("tür", 4);
    assert_eq!(c[0].insert, "kind:");
}

#[test]
fn completions_offer_values_once_the_field_is_typed() {
    let c = complete("kind:i", 6);
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].insert, "kind:image");
    // The spelling the user chose is kept.
    let c = complete("tür:i", 6);
    assert_eq!(c[0].insert, "tür:image");
}

#[test]
fn completions_are_for_the_word_under_the_cursor() {
    let c = complete("rapor ex", 8);
    assert!(
        c.iter().any(|x| x.insert == "ext:") && c.iter().all(|x| x.insert.starts_with("ex")),
        "the earlier word is not what is being typed: {c:?}"
    );
    assert!(
        complete("rapor ex", 5).iter().all(|c| c.insert != "ext:"),
        "a cursor inside the first word does not complete the second"
    );
}

#[test]
fn a_caret_inside_a_character_does_not_take_the_connection_down() {
    // A browser counts `İ` as one code unit where this counts two bytes.
    // Every offset up to past the end must answer rather than panic.
    for input in ["İ", "kİ", "ad:İ", "rapor İş", "TRABZON.MÜZEKKERE"] {
        for cursor in 0..=input.len() + 4 {
            let _ = complete(input, cursor);
        }
    }

    // It lands on the character the caret is inside: `ad:İ` is 5 bytes,
    // so a browser's 4 is mid-`İ` and still means the word so far.
    assert_eq!(complete("ad:İ", 4), complete("ad:İ", 3));
}

#[test]
fn every_offered_completion_parses_as_what_it_claims() {
    // Any offered field, filled in as its own example does, parses as
    // that field rather than as plain text.
    for f in FIELDS {
        let query = if f.example.is_empty() {
            format!("{}:", f.name)
        } else {
            f.example.to_owned()
        };
        let ast = crate::parse_at(&query, 1_800_000_000);
        let is_text = ast
            .groups
            .iter()
            .flat_map(|g| &g.alts)
            .any(|(_, m)| matches!(m, scour_core::Match::NameContains(t) if t.contains(':')));
        assert!(!is_text, "{query} was read as plain text");
        // And the highlighter agrees it is a field, with no warning on it.
        assert!(
            spans_at(&query, 1_800_000_000)
                .iter()
                .all(|s| !s.role.is_warning()),
            "{query} highlights as a mistake"
        );
    }
    for c in complete("kind:", 5) {
        let ast = crate::parse_at(&c.insert, 0);
        assert!(
            ast.groups
                .iter()
                .flat_map(|g| &g.alts)
                .any(|(_, m)| matches!(m, scour_core::Match::Kind(_))),
            "{} was not read as a kind",
            c.insert
        );
    }
}
