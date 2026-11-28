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
fn a_file_in_another_encoding_is_read_in_it_not_offered_as_utf8() {
    // "Türkçe" in ISO-8859-9, whose letters are 1254's: shown as written,
    // never as UTF-8's replacement characters.
    let p = tmp("latin5.txt", b"T\xfcrk\xe7e metin");
    assert_eq!(shape_of(&p, false), Shape::Text);
    assert_eq!(text_head(&p).unwrap().0, "Türkçe metin");
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

/// Derived from the content type, so a format added to the table cannot land
/// in the wrong bucket.
#[test]
fn every_drawn_format_says_which_element_it_is() {
    for (ext, shape) in every_drawn() {
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
    assert!(!every_drawn().any(|(_, s)| s.shown().1.contains("html")));
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

#[test]
fn an_older_turkish_file_is_shown_in_its_code_page() {
    // "Şirket kâğıdı" in Windows-1254: not UTF-8, no NUL.
    let p = tmp(
        "eski.txt",
        b"\xDEirket k\xE2\xF0\xFDd\xFD\nikinci sat\xFDr\n",
    );
    assert_eq!(shape_of(&p, false), Shape::Text);
    let (head, whole, _) = text_head(&p).unwrap();
    assert_eq!(
        (head.as_str(), whole),
        ("Şirket kâğıdı\nikinci satır\n", true)
    );
    let near = text_near(&p, None, Some(2)).unwrap();
    assert!(near.text.starts_with("Şirket"), "{:?}", near.text);
    let _ = std::fs::remove_file(p);
}

#[test]
fn a_file_with_a_byte_order_mark_is_shown_as_utf16() {
    let words = "Dava dilekçesi\nikinci satır\n";
    let mut le = vec![0xFF, 0xFE];
    le.extend(words.encode_utf16().flat_map(u16::to_le_bytes));
    let mut be = vec![0xFE, 0xFF];
    be.extend(words.encode_utf16().flat_map(u16::to_be_bytes));
    for (name, bytes) in [("le.txt", le), ("be.txt", be)] {
        let p = tmp(name, &bytes);
        assert_eq!(shape_of(&p, false), Shape::Text, "{name}");
        assert_eq!(text_head(&p).unwrap().0, words, "{name}");
        // Around the second line's place: not mid-character, the mark not read.
        let at = (bytes.len() / 2) as u64 | 1;
        let near = text_near(&p, Some(at), None).unwrap();
        assert!(
            words.contains(near.text.trim_end_matches('\n')),
            "{name}: {:?}",
            near.text
        );
        assert_eq!(near.from % 2, 0, "{name}");
        let _ = std::fs::remove_file(p);
    }
}

#[test]
fn what_is_text_and_in_what_is_said_by_the_first_bytes() {
    assert_eq!(encoding_of(b"plain"), Some(Encoding::Utf8));
    assert_eq!(encoding_of("şirket".as_bytes()), Some(Encoding::Utf8));
    assert_eq!(encoding_of(b"\xFEirket"), Some(Encoding::Cp1254));
    assert_eq!(encoding_of(b"\xFF\xFEa\x00"), Some(Encoding::Utf16Le));
    assert_eq!(encoding_of(b"a\x00b"), None, "a NUL is a binary");
    // Not UTF-8 and full of control bytes: a binary with no NUL in its head.
    let noise: Vec<u8> = (0..200u8)
        .map(|i| (i.wrapping_mul(37) % 0x20) | (0x80 * (i % 2)))
        .collect();
    assert_eq!(encoding_of(&noise), None);
}

#[test]
fn what_previews_here_is_said_by_name() {
    let m = means(Some(&["/usr/bin/sushi".to_owned(), "--x".to_owned()]));
    assert_eq!(m.quick_look, "sushi");
    assert_eq!(means(None).quick_look, "");
    // Linux never turns Office into pages: it draws them.
    if cfg!(target_os = "linux") {
        assert_eq!(m.office_pages, "");
    }
    if cfg!(any(windows, target_os = "macos")) {
        assert!(!m.pdf_pages.is_empty());
    }
}

/// The architecture's table of what is shown of a file is the rule a person
/// reads: every extension the code draws is named in it.
#[test]
fn the_written_rule_names_every_extension_drawn() {
    let doc = include_str!("../../../docs/ARCHITECTURE.md");
    let heading = doc
        .find("**What is shown of a file**")
        .expect("the table's heading");
    let rows = &doc[heading..];
    let rows = &rows[rows.find("\n|").expect("the table")..];
    let table = &rows[..rows[1..].find("\n\n").map_or(rows.len(), |end| end + 1)];
    for (ext, _) in crate::shape::every_drawn() {
        let named = table
            .split(|c: char| !c.is_ascii_alphanumeric())
            .any(|word| word == ext);
        assert!(named, "{ext} is drawn but not in the written rule");
    }
}
