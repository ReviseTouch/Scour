//! The public surface against real files in a temp directory.

use std::path::PathBuf;

use scour_grep::{Budget, File, Ground, Medium, Needle, Options, Outcome, Pattern, Scanner, Skip};

mod support;
use support::*;

#[test]
fn outcomes_come_back_in_the_order_asked() {
    let dir = tempfile::tempdir().unwrap();
    let files: Vec<File> = (0..200)
        .map(|i| {
            let body = if i % 7 == 0 {
                format!("one\ntwo İĞNE {i}\n")
            } else {
                "nothing\n".into()
            };
            put(dir.path(), &format!("f{i}.txt"), body.as_bytes())
        })
        .collect();
    let got =
        scanner(text("iğne"), Options::default(), dir.path()).scan(&files, &Budget::unlimited());
    assert_eq!(got.len(), 200);
    for (i, outcome) in got.iter().enumerate() {
        match outcome {
            Outcome::Found { hits, more } if i % 7 == 0 => {
                assert_eq!((hits.len(), *more), (1, false));
                assert_eq!((hits[0].line, hits[0].offset), (Some(2), 8));
                assert_eq!(hits[0].text, format!("two İĞNE {i}"));
            }
            Outcome::Clean if i % 7 != 0 => {}
            other => panic!("file {i}: {other:?}"),
        }
    }
}

#[test]
fn what_is_not_text_is_read_only_when_asked() {
    let dir = tempfile::tempdir().unwrap();
    let mut exe = b"\x7fELF\0\0\0".to_vec();
    exe.extend(utf16le("the Needle is here"));
    let mut marked = vec![0xFF, 0xFE];
    marked.extend(utf16le("first\r\nsecond needle\r\n"));
    let files = [
        put(dir.path(), "a.bin", &exe),
        put(dir.path(), "b.txt", &marked),
        put(dir.path(), "c.bin", b"\0\0needle\0\0"),
    ];
    let plain =
        scanner(text("needle"), Options::default(), dir.path()).scan(&files, &Budget::unlimited());
    assert_eq!(plain[0], Outcome::Skipped(Skip::Binary));
    // A byte-order mark says the file is text, whatever its NULs.
    let Outcome::Found { hits, .. } = &plain[1] else {
        panic!("{:?}", plain[1])
    };
    assert_eq!(
        (hits[0].text.as_str(), hits[0].line),
        ("second needle", None)
    );
    assert_eq!(plain[2], Outcome::Skipped(Skip::Binary));

    let options = Options {
        binary: true,
        ..Options::default()
    };
    let all = scanner(text("needle"), options, dir.path()).scan(&files, &Budget::unlimited());
    let Outcome::Found { hits, .. } = &all[0] else {
        panic!("{:?}", all[0])
    };
    assert_eq!(hits[0].offset, 7 + 8);
    assert!(
        hits[0].text.contains("the Needle is here"),
        "{:?}",
        hits[0].text
    );
    assert!(matches!(all[2], Outcome::Found { .. }));
}

#[test]
fn a_find_past_the_first_megabyte_keeps_its_line_and_offset() {
    let dir = tempfile::tempdir().unwrap();
    let line = "0123456789abcdef0123456789abcde\n"; // 32 bytes
    let mut body = line.repeat(100_000); // 3.2 MB
    body.push_str("the needle\n");
    body.push_str(&line.repeat(40_000));
    body.push_str("needle again");
    let files = [put(dir.path(), "big.log", body.as_bytes())];
    let got =
        scanner(text("needle"), Options::default(), dir.path()).scan(&files, &Budget::unlimited());
    let Outcome::Found { hits, more } = &got[0] else {
        panic!("{:?}", got[0])
    };
    assert!(!more);
    assert_eq!((hits[0].line, hits[0].offset), (Some(100_001), 3_200_004));
    assert_eq!(hits[0].text, "the needle");
    assert_eq!(hits[1].line, Some(140_002));
    assert_eq!(hits[1].offset as usize, body.len() - "needle again".len());
}

#[test]
fn a_find_across_a_seam_is_found_once() {
    let dir = tempfile::tempdir().unwrap();
    // No newline anywhere: the seam is 64 KiB before each megabyte's end.
    for at in [
        (1 << 20) - (64 << 10) - 3,
        (1 << 20) - 3,
        (2 << 20) - (128 << 10) - 2,
    ] {
        let mut body = vec![b'x'; 3 << 20];
        body[at..at + 6].copy_from_slice(b"needle");
        let files = [put(dir.path(), "blob", &body)];
        let got = scanner(text("needle"), Options::default(), dir.path())
            .scan(&files, &Budget::unlimited());
        let Outcome::Found { hits, more } = &got[0] else {
            panic!("{at}: {:?}", got[0])
        };
        assert_eq!(
            (hits.len(), *more, hits[0].offset),
            (1, false, at as u64),
            "{at}"
        );
    }
}

#[test]
fn limits_are_kept_and_said() {
    let dir = tempfile::tempdir().unwrap();
    let many = "needle\n".repeat(10);
    let files = [
        put(dir.path(), "many.txt", many.as_bytes()),
        put(dir.path(), "big.txt", &vec![b'a'; 4096]),
        put(dir.path(), "empty.txt", b""),
        File {
            path: dir.path().join("gone.txt"),
            size: None,
            class: None,
        },
        File {
            path: dir.path().to_path_buf(),
            size: None,
            class: None,
        },
    ];
    let options = Options {
        max_size: 1024,
        hits_per_file: 2,
        ..Options::default()
    };
    let got = scanner(text("needle"), options, dir.path()).scan(&files, &Budget::unlimited());
    let Outcome::Found { hits, more } = &got[0] else {
        panic!("{:?}", got[0])
    };
    assert_eq!((hits.len(), *more), (2, true));
    assert_eq!(got[1], Outcome::Skipped(Skip::TooBig));
    assert_eq!(got[2], Outcome::Clean);
    assert_eq!(got[3], Outcome::Unreadable(std::io::ErrorKind::NotFound));
    assert!(
        matches!(
            got[4],
            Outcome::Skipped(Skip::NotAFile) | Outcome::Unreadable(_)
        ),
        "{:?}",
        got[4]
    );

    // A size the caller did not know is asked of the file itself.
    let unknown = [File {
        size: None,
        ..files[1].clone()
    }];
    let options = Options {
        max_size: 1024,
        ..Options::default()
    };
    let got = scanner(text("a"), options, dir.path()).scan(&unknown, &Budget::unlimited());
    assert_eq!(got[0], Outcome::Skipped(Skip::TooBig));
}

#[test]
fn a_spent_budget_leaves_the_rest_unread() {
    let dir = tempfile::tempdir().unwrap();
    let files: Vec<File> = (0..32)
        .map(|i| put(dir.path(), &format!("{i:02}"), &vec![b'a'; 64 << 10]))
        .collect();
    // One lane of one reader, so the order read is the order given: a disk
    // said to spin, with too few files to be asked again what it is.
    let ground = Ground::of([(dir.path().to_path_buf(), None, Medium::Spinning)]);
    let scanner = Scanner::new(
        Pattern::new(&text("zzz"), false).unwrap(),
        Options::default(),
        ground,
    );
    let budget = Budget::new(256 << 10, None);
    let got = scanner.scan(&files, &budget);
    let read = got.iter().filter(|o| **o == Outcome::Clean).count();
    assert_eq!(read, 4);
    assert!(got[4..].iter().all(|o| *o == Outcome::NotReached));
    assert_eq!(budget.spent(), 256 << 10);

    let stopped = Budget::unlimited();
    stopped.stop();
    assert!(
        scanner
            .scan(&files, &stopped)
            .iter()
            .all(|o| *o == Outcome::NotReached)
    );
}

#[test]
fn bytes_and_expressions() {
    let dir = tempfile::tempdir().unwrap();
    let files = [
        put(dir.path(), "a.bin", b"\0\0\xDE\xAD\xBE\xEF\0"),
        put(dir.path(), "b.rs", b"fn main() {}\n  fn helper() {}\n"),
    ];
    let options = Options {
        binary: true,
        ..Options::default()
    };
    let hex = scanner(Needle::hex("dead beef").unwrap(), options, dir.path());
    let got = hex.scan(&files, &Budget::unlimited());
    let Outcome::Found { hits, .. } = &got[0] else {
        panic!("{:?}", got[0])
    };
    assert_eq!(hits[0].offset, 2);
    assert_eq!(got[1], Outcome::Clean);

    let regex = scanner(
        Needle::Regex(r"^\s+fn \w+".into()),
        Options::default(),
        dir.path(),
    );
    let got = regex.scan(&files[1..], &Budget::unlimited());
    let Outcome::Found { hits, .. } = &got[0] else {
        panic!("{:?}", got[0])
    };
    assert_eq!((hits.len(), hits[0].line), (1, Some(2)));
}

#[cfg(unix)]
#[test]
fn a_pipe_is_not_waited_on() {
    let dir = tempfile::tempdir().unwrap();
    let pipe: PathBuf = dir.path().join("pipe");
    let made = std::process::Command::new("mkfifo").arg(&pipe).status();
    if !made.is_ok_and(|s| s.success()) {
        return;
    }
    let files = [File {
        path: pipe,
        size: None,
        class: None,
    }];
    let got = scanner(text("x"), Options::default(), dir.path()).scan(&files, &Budget::unlimited());
    assert_eq!(got[0], Outcome::Skipped(Skip::NotAFile));
}

#[test]
fn an_identity_that_cannot_be_taken_reads_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let files = [put(dir.path(), "a.txt", b"needle")];
    let refused = scanner(text("needle"), Options::default(), dir.path())
        .entering(Some(std::sync::Arc::new(|| Err(()))));
    assert_eq!(
        refused.scan(&files, &Budget::unlimited()),
        [Outcome::Unreadable(std::io::ErrorKind::PermissionDenied)]
    );
}

#[test]
fn a_line_with_two_finds_is_shown_once() {
    let dir = tempfile::tempdir().unwrap();
    let files = [put(
        dir.path(),
        "a.rs",
        b"needle_init and needle_mark\nnothing\nneedle\n",
    )];
    let got =
        scanner(text("needle"), Options::default(), dir.path()).scan(&files, &Budget::unlimited());
    let Outcome::Found { hits, more } = &got[0] else {
        panic!("{:?}", got[0])
    };
    let lines: Vec<_> = hits.iter().map(|hit| hit.line).collect();
    assert_eq!((lines, *more), (vec![Some(1), Some(3)], false));
}

#[test]
fn udf_docx_xlsx_and_nested_zip_documents_are_searched_in_memory() {
    let dir = tempfile::tempdir().unwrap();

    let udf_xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<template format_id=\"1.8\"><content><![CDATA[ADANA 13. ASLİYE TİCARET MAHKEMESİ\nDAVA KONUSU: İş Kazası Tazminatı]]></content><properties><font name=\"Calibri\"/></properties></template>";
    let udf_bytes = make_zip(&[("content.xml", 8, udf_xml.as_bytes())]);

    let docx_xml = "<w:document><w:body><w:p><w:r><w:t>İş </w:t></w:r><w:r><w:t>Kazası </w:t></w:r><w:r><w:t>Raporu</w:t></w:r></w:p></w:body></w:document>";
    let styles_xml = "<w:styles><w:docDefaults><w:rFonts w:ascii=\"CalibriBoilerplate\"/></w:docDefaults></w:styles>";
    let docx_bytes = make_zip(&[
        ("word/styles.xml", 8, styles_xml.as_bytes()),
        ("word/document.xml", 8, docx_xml.as_bytes()),
    ]);

    // A normal `.zip` holding both a Stored `.docx`, a Deflated `.udf`, a plain `.txt`, and a binary `.png`.
    let bundle_bytes = make_zip(&[
        ("img/photo.png", 0, b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR"),
        ("ekler/rapor.docx", 0, &docx_bytes),
        ("ekler/dilekce.udf", 8, &udf_bytes),
        ("notlar.txt", 8, "Özet: iş kazası dosyası\n".as_bytes()),
    ]);

    let bin_only_zip = make_zip(&[("logo.png", 0, b"\x89PNG\r\n\x1a\n\0\0\0")]);

    let files = [
        put(dir.path(), "dava.udf", &udf_bytes),
        put(dir.path(), "sozlesme.docx", &docx_bytes),
        put(dir.path(), "arsiv.zip", &bundle_bytes),
        put(dir.path(), "resimler.zip", &bin_only_zip),
    ];

    let got = scanner(text("iş kazası"), Options::default(), dir.path())
        .scan(&files, &Budget::unlimited());

    // 1. UDF matched inside CDATA with Turkish case folding, on line 2!
    let Outcome::Found { hits: udf_hits, .. } = &got[0] else {
        panic!("udf: {:?}", got[0]);
    };
    assert_eq!(udf_hits[0].line, Some(2));
    assert!(udf_hits[0].text.contains("İş Kazası Tazminatı"));

    // 2. DOCX matched across split `<w:r><w:t>` runs!
    let Outcome::Found {
        hits: docx_hits, ..
    } = &got[1]
    else {
        panic!("docx: {:?}", got[1]);
    };
    assert_eq!(docx_hits[0].text, "İş Kazası Raporu");

    // 3. Normal `.zip` found matches inside nested `.docx`, nested `.udf`, and `.txt`, with exact `at` span!
    let Outcome::Found { hits: zip_hits, .. } = &got[2] else {
        panic!("zip: {:?}", got[2]);
    };
    assert_eq!(zip_hits.len(), 3);
    assert!(zip_hits[0].text.starts_with("[ekler/rapor.docx] "));
    let (s, l) = (zip_hits[0].at.0 as usize, zip_hits[0].at.1 as usize);
    assert_eq!(&zip_hits[0].text[s..s + l], "İş Kazası");
    assert!(zip_hits[1].text.starts_with("[ekler/dilekce.udf] "));
    assert!(zip_hits[2].text.starts_with("[notlar.txt] "));

    // 4. Binary-only `.zip` reports `Skip::Binary`.
    assert_eq!(got[3], Outcome::Skipped(Skip::Binary));

    // 5. Boilerplate `word/styles.xml` is never searched as document content.
    let boiler = scanner(text("CalibriBoilerplate"), Options::default(), dir.path())
        .scan(&files[1..2], &Budget::unlimited());
    assert_eq!(boiler[0], Outcome::Clean);
}

#[test]
fn large_zip_skips_heavy_media_and_reads_only_document_xml() {
    let dir = tempfile::tempdir().unwrap();
    let media = vec![0xABu8; 1_500_000]; // > 1 MiB so `eof == false` on first read
    let doc = "<w:document><w:body><w:p><w:r><w:t>Büyük belgede gizli cümle</w:t></w:r></w:p></w:body></w:document>";
    let big_docx = make_zip(&[
        ("word/media/huge.png", 0, &media),
        ("word/document.xml", 8, doc.as_bytes()),
    ]);
    let files = [put(dir.path(), "huge.docx", &big_docx)];
    let got = scanner(text("gizli cümle"), Options::default(), dir.path())
        .scan(&files, &Budget::unlimited());
    let Outcome::Found { hits, .. } = &got[0] else {
        panic!("huge docx: {:?}", got[0]);
    };
    assert_eq!(hits[0].text, "Büyük belgede gizli cümle");
}
