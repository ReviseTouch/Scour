//! A face draws a content pattern by `scour_ui::pattern::pieces`; the search
//! reads it by `scour_grep::Expr`. The two must take it apart alike, or a
//! capsule would show a clause the search does not have. A capsule with only
//! a hole in it (`!`, `;`, `~`, `""` with no word yet) is no clause.

use scour_grep::{BadPattern, Expr, Needle};
use scour_ui::pattern::{PieceKind, pieces};

#[test]
fn the_faces_draw_a_pattern_as_the_search_reads_it() {
    for pattern in [
        "icra",
        "icra dairesi",
        r#""icra dairesi" karar"#,
        "a;b c",
        "a | b",
        "(a | b) c",
        "a ; b",
        "x !y",
        "x ! y",
        "!y !z w",
        r#""a b";c !"d e""#,
        "ş;ç ğ",
        "~rapor",
        "~rapor|dilekçe",
        "!~taslak",
        "x !~y",
        "(a | ~rapor) c",
        r#""~x""#,
        r"\~x",
        "~",
        "~|x",
        r#"~"iş kazası""#,
        "!",
        "x !",
        ";",
        "x;",
        "x ;",
        "; x",
        "!~",
        "x ~",
        r#""""#,
        r#"x !"""#,
        r#""ab"#,
        "a;~",
    ] {
        let (must, forbid) = match Expr::new(&Needle::Text(pattern.into()), false) {
            Ok(e) => (e.must.len(), e.forbid.is_some()),
            Err(BadPattern::Empty) => (0, false),
            Err(e) => panic!("{pattern}: {e:?}"),
        };
        let drawn: Vec<_> = pieces(pattern)
            .into_iter()
            .filter(|p| !p.is_empty())
            .collect();
        let musts = drawn.iter().filter(|p| p.kind != PieceKind::Not).count();
        let nots = drawn.iter().filter(|p| p.kind == PieceKind::Not).count();
        assert_eq!(musts, must, "{pattern}: {drawn:?}");
        assert_eq!(nots > 0, forbid, "{pattern}: {drawn:?}");
    }
}
