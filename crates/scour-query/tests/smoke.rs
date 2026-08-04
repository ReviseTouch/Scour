//! The query language, from outside.
//!
//! The most valuable test here is the last one: every example in [`SYNTAX`] is
//! parsed and checked. That document is served to language models as the
//! authoritative description of the language, so an example in it that no
//! longer works is worse than no documentation at all — it actively teaches
//! the wrong thing.

use scour_core::{Ast, Cmp, Kind, Match, TimeField};
use scour_query::{SYNTAX, describe, glob_matches, parse, parse_at};

/// One flattened list of every alternative, for terse assertions.
fn alts(q: &str) -> Vec<(bool, Match)> {
    parse_at(q, NOW)
        .groups
        .into_iter()
        .flat_map(|g| g.alts)
        .collect()
}

/// 2027-01-15T08:00:00Z. Fixed so relative windows are assertable.
const NOW: i64 = 1_800_000_000;

#[test]
fn the_everyday_query() {
    let q = parse_at("rapor ext:pdf dm:30d", NOW);
    assert_eq!(q.groups.len(), 3);
    assert_eq!(q.narrowing_terms(3), vec!["rapor"]);
    assert!(!q.needs_content());
    assert_eq!(
        alts("rapor ext:pdf dm:30d"),
        vec![
            (false, Match::NameContains("rapor".into())),
            (false, Match::Ext(vec!["pdf".into()])),
            (
                false,
                Match::Time(TimeField::Modified, Cmp::Ge, NOW - 30 * 86_400)
            ),
        ]
    );
}

#[test]
fn an_empty_query_is_empty_not_an_error() {
    for input in ["", "   ", "\t\n", "\"\"", "!"] {
        assert!(parse(input).is_empty(), "{input:?} should parse to nothing");
    }
}

#[test]
fn parsing_never_panics_on_partial_input() {
    // Everything-style search parses on every keystroke, so half-typed queries
    // are the common case, not the edge case.
    for input in [
        "size:",
        "size:>",
        "ext:",
        "dm:",
        "dm:>",
        "\"unclosed",
        "a|",
        "|b",
        "|",
        "!!x",
        "::",
        "kind:",
        "path:",
        "*",
        "?",
        "**",
        "a b c d e f g",
        "content:",
    ] {
        let _ = parse_at(input, NOW);
    }
}

#[test]
fn queries_survive_a_json_round_trip() {
    // The wire protocol and the MCP server both carry the parsed tree.
    let q = parse_at(
        "rapor|belge !tmp ext:pdf;docx size:>1mb kind:doc dc:2026-01-31",
        NOW,
    );
    let json = serde_json::to_string(&q).expect("serialise");
    let back: Ast = serde_json::from_str(&json).expect("deserialise");
    assert_eq!(q, back);
}

#[test]
fn glob_agrees_with_what_the_parser_produced() {
    let Some((false, Match::NameGlob(p))) = alts("*.rs").into_iter().next() else {
        panic!("a wildcard term should parse to a glob");
    };
    assert!(glob_matches(&p, "main.rs"));
    assert!(!glob_matches(&p, "main.rst"));
}

#[test]
fn content_terms_are_recognised_and_flagged() {
    let q = parse_at("content:fatura ext:pdf", NOW);
    assert!(
        q.needs_content(),
        "an index must be able to refuse this before searching"
    );
    assert_eq!(
        alts("content:fatura")[0].1,
        Match::ContentContains("fatura".into())
    );
}

#[test]
fn turkish_folding_is_applied_to_every_kind_of_term() {
    assert_eq!(
        alts("İSTANBUL")[0].1,
        Match::NameContains("istanbul".into())
    );
    assert_eq!(alts("ext:JPG")[0].1, Match::Ext(vec!["jpg".into()]));
    assert_eq!(
        alts("path:BELGELER")[0].1,
        Match::PathContains("belgeler".into())
    );
    assert_eq!(alts("kind:KLASÖR")[0].1, Match::Kind(vec![Kind::Dir]));
}

#[test]
fn describe_reads_back_what_was_asked() {
    assert_eq!(describe(&parse("")), "everything");
    assert_eq!(
        describe(&parse_at("rapor ext:pdf", NOW)),
        "name contains \"rapor\" and extension is .pdf"
    );
    assert_eq!(
        describe(&parse_at("a|!b", NOW)),
        "(name contains \"a\" or not name contains \"b\")"
    );
}

/// Every fenced example and every table row in [`SYNTAX`] has to parse to
/// something other than "a literal string", because that is the parser's
/// fallback for input it did not understand.
#[test]
fn every_documented_example_still_works() {
    let examples = [
        "rapor ext:pdf dm:30d",
        "*.log size:>100mb",
        "folder: node_modules",
        "path:src ext:rs !test",
        "kind:image dm:today",
        "\"annual report\" ext:docx;pdf",
    ];
    for e in examples {
        assert!(SYNTAX.contains(e), "example {e:?} is missing from SYNTAX");
        let q = parse_at(e, NOW);
        assert!(!q.is_empty(), "documented example {e:?} parsed to nothing");
    }

    // The field table, checked term by term against what it promises.
    let claims: &[(&str, Match)] = &[
        ("ext:rs", Match::Ext(vec!["rs".into()])),
        (
            "ext:rs;toml;md",
            Match::Ext(vec!["rs".into(), "toml".into(), "md".into()]),
        ),
        ("path:src/api", Match::PathContains("src/api".into())),
        ("file:", Match::IsDir(false)),
        ("folder:", Match::IsDir(true)),
        ("size:>1mb", Match::Size(Cmp::Gt, 1_048_576)),
        ("kind:code", Match::Kind(vec![Kind::Code])),
        (
            "dm:7d",
            Match::Time(TimeField::Modified, Cmp::Ge, NOW - 7 * 86_400),
        ),
        (
            "dc:2026-01-31",
            Match::Time(TimeField::Created, Cmp::Eq, 1_769_817_600),
        ),
        (
            "da:>2026-01-01",
            Match::Time(TimeField::Accessed, Cmp::Gt, 1_767_225_600),
        ),
        ("content:invoice", Match::ContentContains("invoice".into())),
    ];
    for (input, expected) in claims {
        assert!(SYNTAX.contains(input), "{input:?} is documented nowhere");
        assert_eq!(
            &alts(input)[0].1,
            expected,
            "SYNTAX claims something {input:?} does not do"
        );
    }

    // Every size unit the document lists.
    for (unit, mult) in [
        ("b", 1_i64),
        ("kb", 1024),
        ("mb", 1024 * 1024),
        ("gb", 1024_i64.pow(3)),
        ("tb", 1024_i64.pow(4)),
    ] {
        assert_eq!(
            alts(&format!("size:>2{unit}"))[0].1,
            Match::Size(Cmp::Gt, 2 * mult)
        );
    }

    // Every relative window it lists.
    for (word, days) in [
        ("today", 1_i64),
        ("yesterday", 2),
        ("week", 7),
        ("month", 30),
        ("year", 365),
    ] {
        assert_eq!(
            alts(&format!("dm:{word}"))[0].1,
            Match::Time(TimeField::Modified, Cmp::Ge, NOW - days * 86_400),
            "SYNTAX documents dm:{word}"
        );
    }
    for (spec, secs) in [
        ("24h", 24 * 3600_i64),
        ("7d", 7 * 86_400),
        ("2w", 14 * 86_400),
        ("6m", 180 * 86_400),
        ("1y", 365 * 86_400),
    ] {
        assert_eq!(
            alts(&format!("dm:{spec}"))[0].1,
            Match::Time(TimeField::Modified, Cmp::Ge, NOW - secs),
            "SYNTAX documents dm:{spec}"
        );
    }

    // Every kind name it lists.
    for name in [
        "file", "folder", "code", "image", "archive", "doc", "exec", "media",
    ] {
        assert!(
            matches!(alts(&format!("kind:{name}"))[0].1, Match::Kind(_)),
            "SYNTAX lists kind:{name} but the parser does not know it"
        );
    }

    // And the two claims about how forgiving it is.
    assert_eq!(
        alts("size:abc")[0].1,
        Match::NameContains("size:abc".into())
    );
    assert_eq!(
        alts("C:/Users")[0].1,
        Match::NameContains("c:/users".into())
    );
    assert_eq!(
        alts("http://example")[0].1,
        Match::NameContains("http://example".into())
    );
}

#[test]
fn a_comparison_on_a_relative_window_is_not_dropped() {
    // `dm:<7d` used to mean `dm:7d`: the operator was computed and discarded,
    // so a query for "not touched in a week" returned exactly the files that
    // *had* been. A confident answer to the opposite question, and nothing
    // reported it.
    let now = 1_785_000_000;
    let week = 7 * 86_400;
    let of = |q: &str| match &parse_at(q, now).groups[..] {
        [g] => match &g.alts[..] {
            [(false, Match::Time(f, cmp, at))] => (*f, *cmp, *at),
            other => panic!("{q}: {other:?}"),
        },
        other => panic!("{q}: {other:?}"),
    };
    assert_eq!(of("dm:7d"), (TimeField::Modified, Cmp::Ge, now - week));
    assert_eq!(of("dm:>7d"), (TimeField::Modified, Cmp::Gt, now - week));
    assert_eq!(of("dm:<7d"), (TimeField::Modified, Cmp::Lt, now - week));
    assert_eq!(of("da:<24h"), (TimeField::Accessed, Cmp::Lt, now - 86_400));
}

#[test]
fn operators_survive_the_spaces_around_them() {
    // Whitespace splitting runs first, so `a | b` arrived as three tokens and
    // the lone pipe parsed to nothing — the query quietly became `a AND b`.
    // `! main` did the same and searched *for* main.
    let now = 1_785_000_000;
    let shape = |q: &str| {
        parse_at(q, now)
            .groups
            .iter()
            .map(|g| g.alts.len())
            .collect::<Vec<_>>()
    };
    for q in ["a|b", "a | b", "a |b", "a| b"] {
        assert_eq!(shape(q), vec![2], "{q:?} should be one group of two");
    }
    for q in ["!main", "! main"] {
        let ast = parse_at(q, now);
        assert_eq!(ast.groups.len(), 1, "{q:?}");
        assert!(ast.groups[0].alts[0].0, "{q:?} should be negated");
    }
    // And a bang that is part of a word stays part of the word.
    let ast = parse_at("hello! doc", now);
    assert_eq!(ast.groups.len(), 2);
    assert!(
        !ast.groups[0].alts[0].0,
        "`hello!` is a name, not a negation"
    );
    assert!(!ast.groups[1].alts[0].0);
}
