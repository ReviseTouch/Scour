//! Each kind of file read the way its kind is read: by what the caller says
//! it is, by its first bytes, by its encoding.

use scour_grep::{Budget, Class, File, Needle, Options, Outcome, Skip};

mod support;
use support::*;

fn one(needle: Needle, options: Options, name: &str, bytes: &[u8]) -> Outcome {
    let dir = tempfile::tempdir().unwrap();
    let file = put(dir.path(), name, bytes);
    scanner(needle, options, dir.path())
        .scan(&[file], &Budget::unlimited())
        .remove(0)
}

fn found_text(outcome: &Outcome) -> &str {
    match outcome {
        Outcome::Found { hits, .. } => &hits[0].text,
        other => panic!("not found: {other:?}"),
    }
}

#[test]
fn a_picture_is_not_opened_for_words() {
    let dir = tempfile::tempdir().unwrap();
    let mut file = put(dir.path(), "a.txt", b"needle");
    file.class = Some(Class::HeaderOnly);
    let got = scanner(text("needle"), Options::default(), dir.path())
        .scan(std::slice::from_ref(&file), &Budget::unlimited());
    assert_eq!(got, [Outcome::Skipped(Skip::HeaderOnly)]);
    // Bytes are looked for everywhere, a picture included.
    let got = scanner(
        Needle::hex("6E 65").unwrap(),
        Options::default(),
        dir.path(),
    )
    .scan(&[file], &Budget::unlimited());
    assert!(matches!(got[0], Outcome::Found { .. }), "{got:?}");
}

#[test]
fn first_bytes_overrule_the_name() {
    let png = b"\x89PNG\r\n\x1A\nneedle";
    assert_eq!(
        one(text("needle"), Options::default(), "a.txt", png),
        Outcome::Skipped(Skip::HeaderOnly)
    );
    let pdf = b"%PDF-1.7\nneedle";
    assert_eq!(
        one(text("needle"), Options::default(), "a.txt", pdf),
        Outcome::Skipped(Skip::NoReader)
    );
    let binary = Options {
        binary: true,
        ..Options::default()
    };
    assert!(matches!(
        one(text("needle"), binary, "a.pdf", pdf),
        Outcome::Found { .. }
    ));
}

#[test]
fn hex_with_gaps_and_at_the_start() {
    let elf = b"\x7FELF\x02\x01\x01\0\0\0needle";
    let gap = Needle::hex("7F ?? 4C 46").unwrap();
    assert!(matches!(
        one(gap, Options::default(), "a", elf),
        Outcome::Found { .. }
    ));
    let start = Needle::hex("^45 4C 46").unwrap();
    assert_eq!(one(start, Options::default(), "a", elf), Outcome::Clean);
    let start = Needle::hex("^7F 45").unwrap();
    assert!(matches!(
        one(start, Options::default(), "a", elf),
        Outcome::Found { hits, .. } if hits[0].offset == 0
    ));
    // Only at the first byte, even when it recurs past the first buffer.
    let mut far = vec![b'x'; 3 << 20];
    far[2 << 20..(2 << 20) + 4].copy_from_slice(b"\x7FELF");
    assert_eq!(
        one(
            Needle::hex("^7F 45").unwrap(),
            Options::default(),
            "b",
            &far
        ),
        Outcome::Clean
    );
    assert_eq!(Needle::hex("?? ??"), None);
}

#[test]
fn a_turkish_file_in_1254_is_found_and_shown() {
    let legacy = b"ilk\nbu \xDEEH\xDDR g\xFCzel\nson";
    let got = one(text("şehir"), Options::default(), "eski.txt", legacy);
    assert_eq!(found_text(&got), "bu ŞEHİR güzel");
}

#[test]
fn a_decomposed_letter_is_the_same_letter() {
    let nfd = "bu s\u{327}ehir".as_bytes();
    let got = one(text("şehir"), Options::default(), "mac.txt", nfd);
    assert!(matches!(got, Outcome::Found { .. }), "{got:?}");
}

#[test]
fn rtf_is_read_for_its_text() {
    let rtf = br"{\rtf1\ansi\ansicpg1254{\fonttbl{\f0 Arial;}}\f0 Asliye Hukuk Mahkemesi'ne\par ikinci \'fe\'fd\'f0 sat\'fdr}";
    let got = one(text("mahkemesi'ne"), Options::default(), "a.rtf", rtf);
    assert_eq!(found_text(&got), "Asliye Hukuk Mahkemesi'ne");
    let got = one(text("satır"), Options::default(), "a.rtf", rtf);
    assert_eq!(found_text(&got), "ikinci şığ satır");
    // Font names are not the document's words.
    assert_eq!(
        one(text("arial"), Options::default(), "a.rtf", rtf),
        Outcome::Clean
    );
}

#[test]
fn sheet_names_comments_and_charts_are_read() {
    let xlsx = make_zip(&[
        ("[Content_Types].xml", 0, b"<Types/>"),
        (
            "xl/workbook.xml",
            8,
            b"<workbook><sheets><sheet name=\"Mahkeme listesi\"/></sheets></workbook>",
        ),
        ("xl/sharedStrings.xml", 8, b"<sst><si><t>bir</t></si></sst>"),
        (
            "xl/comments1.xml",
            8,
            b"<comments><t>bilirkisi notu</t></comments>",
        ),
    ]);
    for word in ["listesi", "bilirkisi"] {
        let got = one(text(word), Options::default(), "a.xlsx", &xlsx);
        assert!(matches!(got, Outcome::Found { .. }), "{word}: {got:?}");
    }
    let pptx = make_zip(&[
        ("ppt/presentation.xml", 0, b"<p/>"),
        ("ppt/slides/slide1.xml", 8, b"<a:t>bos</a:t>"),
        ("ppt/charts/chart1.xml", 8, b"<c:v>icra dairesi</c:v>"),
    ]);
    let got = one(text("dairesi"), Options::default(), "a.pptx", &pptx);
    assert!(matches!(got, Outcome::Found { .. }), "{got:?}");
}

#[test]
fn a_file_named_by_the_caller_carries_no_class() {
    let file = File::at("/nowhere");
    assert_eq!((file.size, file.class), (None, None));
}

#[cfg(unix)]
#[test]
fn a_link_is_not_read_through() {
    let dir = tempfile::tempdir().unwrap();
    let target = put(dir.path(), "real.txt", b"needle");
    let link = dir.path().join("link.txt");
    std::os::unix::fs::symlink(&target.path, &link).unwrap();
    let loop_ = dir.path().join("loop");
    std::os::unix::fs::symlink(&loop_, &loop_).unwrap();
    let got = scanner(text("needle"), Options::default(), dir.path()).scan(
        &[File::at(link), File::at(loop_), target],
        &Budget::unlimited(),
    );
    assert_eq!(got[0], Outcome::Skipped(Skip::NotAFile));
    assert_eq!(got[1], Outcome::Skipped(Skip::NotAFile));
    assert!(matches!(got[2], Outcome::Found { .. }));
}
