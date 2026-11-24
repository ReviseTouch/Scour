/// A `:` starting a filter, and the letters choosing it, are left out: the
/// rest of the query answers until a field is chosen.
#[test]
fn a_filter_being_started_is_left_out() {
    for q in ["kind:doc :", "kind:doc :d", ": kind:doc", "kind:doc !:kon"] {
        assert_eq!(parse_at(q, 0).groups, parse_at("kind:doc", 0).groups, "{q}");
    }
    assert!(parse_at(":", 0).groups.is_empty());
    // Quoted, it is the character; with a field after it, not a launcher.
    assert!(!parse_at("\":\"", 0).groups.is_empty());
    assert!(!parse_at(":a:b", 0).groups.is_empty());
}

/// A filter whose value is not chosen yet is left out, whichever field it
/// is: the list goes on answering for the rest of the query under the lens.
#[test]
fn a_filter_without_a_value_is_left_out() {
    for q in [
        "kind:doc dm:",
        "kind:doc ext:",
        "kind:doc size:",
        "kind:doc under:",
        "kind:doc kind:",
        "kind:doc !dm:",
        "kind:doc dm:\"\"",
    ] {
        assert_eq!(parse_at(q, 0).groups, parse_at("kind:doc", 0).groups, "{q}");
    }
    assert!(parse_at("dm:", 0).groups.is_empty());
    // The fields that take nothing are complete as written.
    assert!(matches!(
        parse_at("folder:", 0).groups[0].alts[0].1,
        Match::IsDir(true)
    ));
    // A value that cannot be read is still a search for its text.
    assert!(!parse_at("dm:zzz", 0).groups.is_empty());
}

/// An empty `content:` is a filter not written yet: the rest of the query
/// answers as if it were not there, not as a search for its text.
#[test]
fn an_empty_content_term_is_left_out() {
    for q in [
        "rapor content:\"\"",
        "rapor content:",
        "content:\" \" rapor",
        "rapor içerik:\"\"",
    ] {
        assert_eq!(parse_at(q, 0).groups, parse_at("rapor", 0).groups, "{q}");
    }
    assert!(parse_at("content:\"\"", 0).groups.is_empty());
    // Written, it is a filter.
    assert!(matches!(
        parse_at("content:x", 0).groups[0].alts[0].1,
        Match::ContentContains(_)
    ));
}
use super::*;
use scour_core::Kind;

/// Every alternative in the query, flattened.
fn m(q: &str) -> Vec<(bool, Match)> {
    parse_at(q, 0)
        .groups
        .into_iter()
        .flat_map(|g| g.alts)
        .collect()
}

/// The same, with a home for `~` to expand to.
fn mh(q: &str) -> Vec<(bool, Match)> {
    parse_in(q, 0, Some("/home/u"))
        .groups
        .into_iter()
        .flat_map(|g| g.alts)
        .collect()
}

#[test]
fn tilde_is_the_home_directory_when_one_is_known() {
    assert_eq!(
        mh("under:~/x"),
        vec![(false, Match::Under("/home/u/x".into()))]
    );
    assert_eq!(mh("under:~"), vec![(false, Match::Under("/home/u".into()))]);
    assert_eq!(
        mh("under:~/"),
        vec![(false, Match::Under("/home/u".into()))]
    );
    assert_eq!(
        mh("parent:~/Downloads"),
        vec![(false, Match::ParentIs("/home/u/Downloads".into()))]
    );
    assert_eq!(
        mh(r"under:~\x"),
        vec![(false, Match::Under("/home/u/x".into()))]
    );
    // Only a lone `~` or `~/`: a folder named `~x` is a folder named `~x`.
    assert_eq!(mh("under:~x"), vec![(false, Match::Under("~x".into()))]);
    // Without a home there is nothing to expand to.
    assert_eq!(m("under:~/x"), vec![(false, Match::Under("~/x".into()))]);
}

/// A `!` in front of a list does not take the list apart: `!ext:rs;toml`
/// is one exclusion, not a negation OR-ed with a bare word.
#[test]
fn a_negated_list_stays_one_list() {
    assert_eq!(
        m("!ext:rs;toml"),
        vec![(true, Match::Ext(vec!["rs".into(), "toml".into()]))]
    );
    // One group, not two OR-ed alternatives.
    assert_eq!(parse_at("!ext:rs;toml", 0).groups.len(), 1);
    assert_eq!(parse_at("!ext:rs;toml", 0).groups[0].alts.len(), 1);
    // And across the space, which `join_lists` glues back together.
    assert_eq!(
        m("!ext:rs; toml"),
        vec![(true, Match::Ext(vec!["rs".into(), "toml".into()]))]
    );
    // The same for every other list field.
    assert_eq!(
        m("!kind:image;code"),
        vec![(true, Match::Kind(vec![Kind::Image, Kind::Code]))]
    );
    // What it must not break: the un-negated form, and `;` between words.
    assert_eq!(
        m("ext:rs;toml"),
        vec![(false, Match::Ext(vec!["rs".into(), "toml".into()]))]
    );
    assert_eq!(parse_at("a;b", 0).groups.len(), 2);
}

#[test]
fn a_term_can_be_forgiven_its_typing_mistakes_or_be_letters_in_order() {
    assert_eq!(
        m("fuzzy:Raporr"),
        vec![(false, Match::NameNear("raporr".into(), 1))]
    );
    assert_eq!(
        m("benzer:muhasebeciler"),
        vec![(false, Match::NameNear("muhasebeciler".into(), 2))]
    );
    // Four letters are forgiven nothing: one wrong is another word.
    assert_eq!(
        m("fuzzy:main"),
        vec![(false, Match::NameContains("main".into()))]
    );
    assert_eq!(
        m("letters:ScrGui"),
        vec![(false, Match::NameLetters("scrgui".into()))]
    );
    assert_eq!(
        m("!harfler:abc"),
        vec![(true, Match::NameLetters("abc".into()))]
    );
    assert_eq!(m("letters:"), vec![]);
}

#[test]
fn the_forgiven_text_reads_as_the_forgiven_query() {
    for q in [
        "raporr",
        "istanbul raporr 2024",
        "raporr ext:pdf !taslak",
        "raporr|belgee kind:doc",
        "(raporr | belgee) ankara",
        "\"yillik raporr\" *.pdf",
        "boyut:1mb muhasebe",
        "size:abcdef",
        "Projeler/Scour deneme",
        "fuzzy:raporr tutanak",
        "a;b;cdefgh",
        "!raporr ŞİRKETLER",
        "letters:scrgui dilekce",
    ] {
        let text = forgiving(q).unwrap_or_else(|| panic!("{q:?} has a word to forgive"));
        assert_eq!(
            Some(parse_at(&text, 0)),
            parse_at(q, 0).forgiving(),
            "{q:?} became {text:?}"
        );
    }
    for q in [
        "",
        "main",
        "ext:pdf size:>1mb",
        "!raporr",
        "fuzzy:raporr",
        "*.rs a/b",
    ] {
        assert_eq!(forgiving(q), None, "{q:?}");
        assert_eq!(parse_at(q, 0).forgiving(), None, "{q:?}");
    }
    assert_eq!(
        forgiving("raporr ext:pdf").as_deref(),
        Some("fuzzy:raporr ext:pdf")
    );
}

#[test]
fn whitespace_is_and_pipe_is_or_bang_is_not() {
    assert_eq!(
        m("rapor"),
        vec![(false, Match::NameContains("rapor".into()))]
    );
    assert_eq!(
        parse_at("a bc", 0).groups.len(),
        2,
        "whitespace makes two groups"
    );
    let q = parse_at("abc|def", 0);
    assert_eq!(q.groups.len(), 1);
    assert_eq!(
        q.groups[0].alts.len(),
        2,
        "a pipe makes one group with two alternatives"
    );
    assert_eq!(m("!tmp"), vec![(true, Match::NameContains("tmp".into()))]);
}

#[test]
fn wildcards_and_quotes() {
    assert_eq!(m("*.rs"), vec![(false, Match::NameGlob("*.rs".into()))]);
    assert_eq!(
        m("\"a*b\""),
        vec![(false, Match::NameContains("a*b".into()))]
    );
    assert_eq!(
        parse_at("\"iki kelime\"", 0).groups.len(),
        1,
        "quotes keep the space"
    );
}

#[test]
fn fields() {
    assert_eq!(
        m("ext:rs;toml"),
        vec![(false, Match::Ext(vec!["rs".into(), "toml".into()]))]
    );
    assert_eq!(m("ext:.RS"), vec![(false, Match::Ext(vec!["rs".into()]))]);
    assert_eq!(m("folder:"), vec![(false, Match::IsDir(true))]);
    assert_eq!(m("file:"), vec![(false, Match::IsDir(false))]);
    assert_eq!(
        m("path:src"),
        vec![(false, Match::PathContains("src".into()))]
    );
    assert_eq!(
        m("size:>1mb"),
        vec![(false, Match::Size(Cmp::Gt, 1_048_576))]
    );
    assert_eq!(
        m("size:<=500kb"),
        vec![(false, Match::Size(Cmp::Le, 512_000))]
    );
    assert_eq!(
        m("size:2tb"),
        vec![(false, Match::Size(Cmp::Ge, 2 * 1024_i64.pow(4)))]
    );
    assert_eq!(m("kind:kod"), vec![(false, Match::Kind(vec![Kind::Code]))]);
    assert_eq!(
        m("kind:KLASÖR"),
        vec![(false, Match::Kind(vec![Kind::Dir]))]
    );
}

#[test]
fn scope_fields_keep_the_path_as_written() {
    assert_eq!(
        m("under:/home/U/Projeler"),
        vec![(false, Match::Under("/home/U/Projeler".into()))]
    );
    assert_eq!(
        m("in:/home/u/x/"),
        vec![(false, Match::Under("/home/u/x".into()))]
    );
    assert_eq!(
        m("parent:/etc"),
        vec![(false, Match::ParentIs("/etc".into()))]
    );
    assert_eq!(m("under:/"), vec![(false, Match::Under("/".into()))]);
    // Empty is not a scope, and not text either: it is left out.
    assert_eq!(m("under:"), vec![]);
}

#[test]
fn field_names_may_contain_non_ascii_letters() {
    // The only two field spellings that are not ASCII.
    assert_eq!(m("tür:kod"), vec![(false, Match::Kind(vec![Kind::Code]))]);
    assert_eq!(m("TÜR:kod"), vec![(false, Match::Kind(vec![Kind::Code]))]);
    assert_eq!(
        m("içerik:x"),
        vec![(false, Match::ContentContains("x".into()))]
    );
    // `C:` is still not a field, and is a path term because of the slash.
    assert_eq!(
        m("C:/Users"),
        vec![(false, Match::PathContains("c:/users".into()))]
    );
}

/// A term with a separator in it asks about the path: no name holds a slash.
#[test]
fn a_term_with_a_slash_in_it_is_about_the_path() {
    assert_eq!(
        m("Projeler/Scour"),
        vec![(false, Match::PathContains("projeler/scour".into()))]
    );
    // Without one it is a name, as before.
    assert_eq!(
        m("Scour"),
        vec![(false, Match::NameContains("scour".into()))]
    );
    // Quoted, it is still a path term: quotes only turn off wildcards.
    assert_eq!(
        m("\"Projeler/Scour\""),
        vec![(false, Match::PathContains("projeler/scour".into()))]
    );
}

#[test]
fn a_field_that_cannot_be_read_becomes_plain_text() {
    // The rule that keeps a half-typed query usable, and keeps Windows
    // paths and URLs from being read as fields.
    assert_eq!(
        m("http://x"),
        vec![(false, Match::PathContains("http://x".into()))]
    );
    assert_eq!(
        m("kind:zurna"),
        vec![(false, Match::NameContains("kind:zurna".into()))]
    );
    assert_eq!(
        m("size:abc"),
        vec![(false, Match::NameContains("size:abc".into()))]
    );
    assert_eq!(
        m("C:/Users"),
        vec![(false, Match::PathContains("c:/users".into()))]
    );
}

#[test]
fn content_is_parsed_even_though_nothing_can_answer_it_yet() {
    assert_eq!(
        m("content:gizli"),
        vec![(false, Match::ContentContains("gizli".into()))]
    );
    assert_eq!(
        m("içerik:gizli"),
        vec![(false, Match::ContentContains("gizli".into()))]
    );
    // An empty value is a filter not written yet, and left out — not the
    // literal text, which found nothing while the bar waited for a word.
    assert_eq!(m("content:"), vec![]);
}

#[test]
fn terms_are_case_folded_the_turkish_way() {
    assert_eq!(
        m("İSTANBUL"),
        vec![(false, Match::NameContains("istanbul".into()))]
    );
    assert_eq!(m("ISTANBUL"), m("ısтanbul".replace('т', "t").as_str()));
}

#[test]
fn narrowing_terms_survive_the_round_trip() {
    let q = parse_at("rapor !tmp *.rs ab ext:pdf", 0);
    assert_eq!(q.narrowing_terms(3), vec!["rapor"]);
}
