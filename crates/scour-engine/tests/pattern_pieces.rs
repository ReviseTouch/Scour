//! A face draws a content pattern by `scour_ui::pattern::pieces`; the search
//! reads it by `scour_grep::Expr`. The two must take it apart alike, or a
//! capsule would show a clause the search does not have.

use scour_grep::{Expr, Needle};
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
    ] {
        let expr = Expr::new(&Needle::Text(pattern.into()), false).expect(pattern);
        let drawn = pieces(pattern);
        let musts = drawn.iter().filter(|p| p.kind != PieceKind::Not).count();
        let nots = drawn.iter().filter(|p| p.kind == PieceKind::Not).count();
        assert_eq!(musts, expr.must.len(), "{pattern}: {drawn:?}");
        assert_eq!(nots > 0, expr.forbid.is_some(), "{pattern}: {drawn:?}");
    }
}
