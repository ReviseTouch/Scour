//! Reading an NTFS volume raw answers exactly what reading its files does:
//! the same outcomes, file for file, on one corpus read both ways. Needs the
//! volume opened, so an administrator; anywhere else it says so and passes.

#![cfg(windows)]

use std::time::Duration;

use scour_grep::{Budget, File, Options, Outcome, Scanner};

mod support;
use support::*;

fn corpus(dir: &std::path::Path) -> Vec<File> {
    let mut files = Vec::new();
    for i in 0..400 {
        let body: Vec<u8> = match i % 8 {
            // Small enough to live in the MFT record.
            0 => format!("a needle {i}\n").into_bytes(),
            1 => "nothing here\n".repeat(400).into_bytes(),
            2 => {
                let mut b = "filler line\n".repeat(120_000);
                b.push_str("the needle past the first megabyte\n");
                b.into_bytes()
            }
            3 => {
                let mut b = vec![0u8, 1, 2, 3, 0, 0xFF];
                b.extend(utf16le("needle in UTF-16"));
                b.resize(9000, 0);
                b
            }
            4 => make_zip(&[("word/document.xml", 8, b"<w:t>a needle inside</w:t>")]),
            5 => "{\\rtf1 a needle in RTF}".into(),
            6 => Vec::new(),
            _ => format!("{}needle at the end", "x".repeat(70_000)).into_bytes(),
        };
        // Every other eight without the needle: answers that stand raw.
        let body = if (i / 8) % 2 == 1 {
            without_needle(body)
        } else {
            body
        };
        files.push(put(dir, &format!("f{i:03}.txt"), &body));
    }
    for f in &files {
        std::fs::File::options()
            .write(true)
            .open(&f.path)
            .and_then(|h| h.sync_all())
            .unwrap();
    }
    files
}

fn without_needle(mut body: Vec<u8>) -> Vec<u8> {
    for (from, to) in [
        (b"needle".to_vec(), b"noodle".to_vec()),
        (utf16le("needle"), utf16le("noodle")),
    ] {
        let mut at = 0;
        while let Some(k) = body[at..].windows(from.len()).position(|w| w == from) {
            body[at + k..at + k + to.len()].copy_from_slice(&to);
            at += k + to.len();
        }
    }
    body
}

fn scan(scanner: &Scanner, files: &[File], raw: &str) -> Vec<Outcome> {
    // SAFETY: this test is the only one in its binary.
    unsafe {
        std::env::set_var("SCOUR_GREP_RAW", raw);
        std::env::set_var("SCOUR_GREP_RAW_RECENT_SECS", "0");
        std::env::set_var("SCOUR_GREP_RAW_LOG", "1");
    }
    scanner.scan(files, &Budget::unlimited())
}

#[test]
fn raw_reading_answers_what_reading_the_files_does() {
    let temp = tempfile::tempdir().unwrap();
    // The long name of every component, as the MFT spells it: `%TEMP%` is
    // often given short (`C:\Users\HSALIH~1`), and a short name is read the
    // normal way.
    let long = std::fs::canonicalize(temp.path()).unwrap();
    let long = long
        .to_str()
        .unwrap()
        .trim_start_matches(r"\\?\")
        .to_owned();
    let dir = std::path::Path::new(&long);
    let letter = long.chars().next().unwrap();
    if std::fs::File::open(format!(r"\\.\{letter}:")).is_err() {
        eprintln!("the volume {letter}: does not open: not an administrator, nothing to compare");
        return;
    }
    let mut files = corpus(dir);
    // One file nobody may read, administrators included: refused both ways.
    let shut = put(dir, "shut.txt", b"a needle behind a lock");
    let deny = std::process::Command::new("icacls")
        .arg(&shut.path)
        .args(["/deny", "*S-1-1-0:(R)"])
        .output()
        .unwrap();
    assert!(deny.status.success(), "icacls: {deny:?}");
    files.push(shut.clone());
    // The lazy writer puts the new directory's record on the disk.
    std::thread::sleep(Duration::from_secs(8));

    let options = Options {
        binary: true,
        ..Options::default()
    };
    let normal = scan(&scanner(text("needle"), options.clone(), dir), &files, "0");
    let raw_scanner = scanner(text("needle"), options, dir);
    let raw = scan(&raw_scanner, &files, "1");
    for (i, (a, b)) in normal.iter().zip(&raw).enumerate() {
        assert_eq!(a, b, "{}", files[i].path.display());
    }
    assert!(
        matches!(
            normal.last(),
            Some(Outcome::Unreadable(std::io::ErrorKind::PermissionDenied))
        ),
        "{:?}",
        normal.last()
    );
    // Files found to hold the needle are read again the normal way, so the
    // raw answers that stood are the others: most of them.
    let clean = (normal.iter().zip(&files))
        .filter(|(o, f)| !matches!(o, Outcome::Found { .. }) && f.size != Some(0))
        .count();
    assert!(clean > 100, "clean {clean}");
    assert!(
        raw_scanner.read_raw() * 2 > clean,
        "read raw {} of {clean}",
        raw_scanner.read_raw()
    );

    let _ = std::process::Command::new("icacls")
        .arg(&shut.path)
        .arg("/reset")
        .output();
}
