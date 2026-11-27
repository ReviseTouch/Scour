use super::*;
use std::path::PathBuf;

fn tmp(name: &str, bytes: &[u8]) -> PathBuf {
    let p = std::env::temp_dir().join(format!("scour-preview-{name}"));
    std::fs::write(&p, bytes).expect("write");
    p
}

/// A named pipe is looked at and refused, never opened: opening one for
/// reading waits for a writer, which held a service thread and the
/// window's slow lane for as long as the window lived.
#[cfg(unix)]
#[test]
fn a_named_pipe_is_refused_without_waiting_for_a_writer() {
    let p = std::env::temp_dir().join(format!("scour-preview-pipe-{}", std::process::id()));
    let _ = std::fs::remove_file(&p);
    let made = std::process::Command::new("mkfifo").arg(&p).status();
    if !made.is_ok_and(|s| s.success()) {
        eprintln!("no mkfifo here; skipped");
        return;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    let at = p.clone();
    std::thread::spawn(move || {
        let _ = tx.send(look_at(&at, false));
    });
    let look = rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("looking at a named pipe waited for a writer");
    let _ = std::fs::remove_file(&p);
    assert_eq!(look.shape, "none");
    assert!(text_head(&p).is_err());
}

#[test]
fn a_picture_is_named_by_its_extension() {
    let p = tmp("a.png", b"\x89PNG\r\n\x1a\n");
    assert_eq!(shape_of(&p, false), Shape::Whole("image/png"));
    let _ = std::fs::remove_file(p);
}

#[test]
fn a_readable_file_is_text_whatever_it_is_called() {
    for name in ["notes.bak", "gitignore", "thing.weird", "x.log"] {
        let p = tmp(name, "bir iki üç\ndört\n".as_bytes());
        assert_eq!(shape_of(&p, false), Shape::Text, "{name}");
        let _ = std::fs::remove_file(p);
    }
}

#[test]
fn something_with_a_nul_in_it_is_not_text() {
    let p = tmp("weights.bin", b"\x93NUMPY\x01\x00\x00binary\x00stuff");
    assert_eq!(shape_of(&p, false), Shape::Nothing);
    let _ = std::fs::remove_file(p);
}

#[test]
fn a_file_in_another_encoding_is_not_offered_as_utf8() {
    // "Türkçe" in ISO-8859-9: no NUL, so only the UTF-8 check can reject it.
    let p = tmp("latin5.txt", b"T\xfcrk\xe7e metin");
    assert_eq!(shape_of(&p, false), Shape::Nothing);
    let _ = std::fs::remove_file(p);
}

#[test]
fn an_empty_file_is_text_rather_than_a_refusal() {
    let p = tmp("empty.txt", b"");
    assert_eq!(shape_of(&p, false), Shape::Text);
    let _ = std::fs::remove_file(p);
}

#[test]
fn a_folder_is_never_previewed() {
    let p = std::env::temp_dir();
    assert_eq!(shape_of(&p, true), Shape::Nothing);
}

#[test]
fn a_multibyte_character_split_by_the_sniff_boundary_is_still_text() {
    let mut bytes = vec![b'a'; SNIFF - 1];
    bytes.extend_from_slice("ş".as_bytes()); // two bytes, one of them past the edge
    bytes.extend_from_slice(&[b'b'; 64]);
    let p = tmp("edge.txt", &bytes);
    assert_eq!(shape_of(&p, false), Shape::Text);
    let _ = std::fs::remove_file(p);
}

/// Derived from the content type, so a format added to `DRAWN` cannot land
/// in the wrong bucket.
#[test]
fn every_drawn_format_says_which_element_it_is() {
    for (ext, shape) in DRAWN {
        let (what, kind) = shape.shown();
        assert!(
            ["image", "audio", "video", "pdf"].contains(&what),
            "{ext} → {what}"
        );
        assert!(kind.contains('/'), "{ext} → {kind:?}");
    }
    assert_eq!(Shape::Text.shown(), ("text", "text/plain"));
    assert_eq!(Shape::Nothing.shown().0, "none");
}

/// Never `text/html`: a page served under its own type, from the origin
/// whose URL carries the token, is stored cross-site scripting.
#[test]
fn a_web_page_is_offered_as_text_and_not_as_a_page() {
    for name in ["index.html", "page.htm", "thing.xhtml", "x.js"] {
        let p = tmp(name, b"<script>alert(1)</script>\n");
        assert_eq!(shape_of(&p, false), Shape::Text, "{name}");
        let _ = std::fs::remove_file(p);
    }
    assert!(!DRAWN.iter().any(|(_, s)| s.shown().1.contains("html")));
}

#[test]
fn a_span_is_the_bytes_that_were_asked_for() {
    let p = tmp("span.bin", b"0123456789");
    let mut out = Vec::new();
    assert_eq!(write_span(&p, 3, 4, &mut out).expect("span"), 4);
    assert_eq!(out, b"3456");

    // A file shorter than the promise stops at its end rather than padding.
    let mut out = Vec::new();
    assert_eq!(write_span(&p, 8, 99, &mut out).expect("span"), 2);
    assert_eq!(out, b"89");
    let _ = std::fs::remove_file(p);
}

#[test]
fn the_head_of_a_long_file_says_it_is_only_the_head() {
    let big = tmp("big.txt", &vec![b'x'; (TEXT_CAP + 10) as usize]);
    let (text, whole, len) = text_head(&big).expect("head");
    assert_eq!(text.len() as u64, TEXT_CAP);
    assert!(!whole);
    assert_eq!(len, TEXT_CAP + 10);
    let _ = std::fs::remove_file(big);

    let small = tmp("small.txt", b"iki satir\n");
    let (text, whole, len) = text_head(&small).expect("head");
    assert_eq!(text, "iki satir\n");
    assert!(whole);
    assert_eq!(len, 10);
    let _ = std::fs::remove_file(small);
}

#[test]
fn a_chosen_quicklook_wins_and_an_empty_one_is_no_quicklook() {
    assert_eq!(
        quicklook(Some("gwenview --fullscreen")),
        Some(vec!["gwenview".into(), "--fullscreen".into()])
    );
    assert_eq!(quicklook(Some("   ")), None);
}

#[test]
fn ranges_are_read_the_way_a_browser_writes_them() {
    assert_eq!(parse_range("bytes=0-99", 1000), Some((0, 99)));
    assert_eq!(parse_range("bytes=500-", 1000), Some((500, 999)));
    // The suffix form, which is how a PDF viewer opens a file.
    assert_eq!(parse_range("bytes=-100", 1000), Some((900, 999)));
    // Clamped, not refused: a browser asks for more than there is before it
    // knows the length.
    assert_eq!(parse_range("bytes=0-99999", 1000), Some((0, 999)));
    // What is not understood means "all of it", not "nothing".
    assert_eq!(parse_range("bytes=0-10,20-30", 1000), None);
    assert_eq!(parse_range("items=0-10", 1000), None);
    assert_eq!(parse_range("bytes=abc-", 1000), None);
    // A start past the end is a request that cannot be met.
    assert_eq!(parse_range("bytes=2000-", 1000), None);
    assert_eq!(parse_range("bytes=0-0", 0), None);
}

/// Where no pages are drawn, an Office document is its words, never its bytes:
/// RTF is plain ASCII, and read as a text file it would show its markup.
#[cfg(not(any(windows, target_os = "macos")))]
#[test]
fn an_office_document_is_its_words_not_its_markup() {
    let p = tmp(
        "kira.rtf",
        br"{\rtf1\ansi{\fonttbl{\f0 Times;}}\f0 Kira bedeli \b \'f6denmemi\'fetir\b0\par}",
    );
    assert_eq!(shape_of(&p, false), Shape::Nothing);
    let look = look_at(&p, false);
    let _ = std::fs::remove_file(&p);
    assert_eq!(look.shape, "text");
    assert!(look.head.contains("Kira bedeli"), "{:?}", look.head);
    assert!(
        !look.head.contains("rtf1") && !look.head.contains("fonttbl"),
        "{:?}",
        look.head
    );
}
