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

#[test]
fn a_phrase_broken_across_lines_is_still_the_phrase() {
    let text_of = |s: &str| Needle::Text(s.into());
    let text = b"Ankara 3. Asliye\n  Hukuk Mahkemesi'ne\nbitti";
    let got = one(
        text_of("asliye hukuk mahkemesi"),
        Options::default(),
        "a.txt",
        text,
    );
    assert!(matches!(got, Outcome::Found { .. }), "{got:?}");
    let apart = b"asliye\n\nbaska\nhukuk";
    let got = one(text_of("asliye hukuk"), Options::default(), "b.txt", apart);
    assert_eq!(got, Outcome::Clean);
}

#[test]
fn a_phrase_spaced_with_no_break_spaces_is_the_phrase() {
    let text = "icra\u{a0}dairesi ve hukuk\u{2009}\u{a0}mahkemesi".as_bytes();
    for phrase in ["icra dairesi", "hukuk mahkemesi"] {
        let got = one(
            Needle::Text(phrase.into()),
            Options::default(),
            "a.txt",
            text,
        );
        assert!(matches!(got, Outcome::Found { .. }), "{phrase}: {got:?}");
    }
}

/// What a scan does to the files it reads, from the kernel's side.
#[cfg(target_os = "linux")]
mod side_effects {
    use super::*;
    use std::time::{Duration, SystemTime};

    /// A file whose access time is ten days old: older than its change, so
    /// even `relatime` updates it on the next read.
    fn aged(dir: &std::path::Path, name: &str) -> File {
        let file = put(dir, name, b"one needle here");
        let old = SystemTime::now() - Duration::from_secs(10 * 86_400);
        let f = std::fs::File::options()
            .write(true)
            .open(&file.path)
            .unwrap();
        f.set_times(std::fs::FileTimes::new().set_accessed(old))
            .unwrap();
        file
    }

    fn accessed(file: &File) -> SystemTime {
        std::fs::metadata(&file.path).unwrap().accessed().unwrap()
    }

    #[test]
    fn reading_leaves_access_times_alone() {
        // tmpfs keeps access times; the disks here are mounted `noatime`.
        let dir = tempfile::tempdir_in("/dev/shm")
            .or_else(|_| tempfile::tempdir())
            .unwrap();
        let control = aged(dir.path(), "control.txt");
        let before = accessed(&control);
        std::fs::read(&control.path).unwrap();
        if accessed(&control) == before {
            eprintln!("this filesystem records no reads; nothing to observe");
            return;
        }
        let file = aged(dir.path(), "a.txt");
        let before = accessed(&file);
        let got = scanner(text("needle"), Options::default(), dir.path())
            .scan(std::slice::from_ref(&file), &Budget::unlimited());
        assert!(matches!(got[0], Outcome::Found { .. }));
        assert_eq!(accessed(&file), before);
    }

    #[test]
    fn nothing_is_opened_for_writing() {
        let dir = tempfile::tempdir().unwrap();
        let files: Vec<File> = (0..20)
            .map(|i| put(dir.path(), &format!("f{i}.txt"), b"needle"))
            .collect();
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK) };
        let path = std::ffi::CString::new(dir.path().as_os_str().as_encoded_bytes()).unwrap();
        let mask = libc::IN_CLOSE_WRITE | libc::IN_CLOSE_NOWRITE | libc::IN_MODIFY;
        assert!(unsafe { libc::inotify_add_watch(fd, path.as_ptr(), mask) } >= 0);
        scanner(text("needle"), Options::default(), dir.path()).scan(&files, &Budget::unlimited());
        let (mut wrote, mut read) = (0, 0);
        let mut buf = [0u8; 8192];
        loop {
            let n = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
            let mut at = 0usize;
            while at < n as usize {
                let ev: libc::inotify_event =
                    unsafe { std::ptr::read_unaligned(buf.as_ptr().add(at).cast()) };
                if ev.mask & (libc::IN_CLOSE_WRITE | libc::IN_MODIFY) != 0 {
                    wrote += 1;
                }
                if ev.mask & libc::IN_CLOSE_NOWRITE != 0 {
                    read += 1;
                }
                at += size_of::<libc::inotify_event>() + ev.len as usize;
            }
        }
        unsafe { libc::close(fd) };
        assert_eq!(wrote, 0);
        assert!(read >= 20, "{read} closed after reading");
    }
}
