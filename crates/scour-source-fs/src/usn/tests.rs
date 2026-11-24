use super::*;

#[test]
fn a_journal_stands_in_for_a_walk_for_a_day_at_most() {
    let dir = std::env::temp_dir().join(format!("scour-usn-walk-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let stamp = dir.join("walked-1");
    assert!(walk_due(&stamp), "never walked is due");
    walked_now(&stamp);
    assert!(!walk_due(&stamp), "just walked is not");
    let old = std::time::SystemTime::now() - WALK_AT_LEAST - std::time::Duration::from_secs(60);
    std::fs::File::options()
        .write(true)
        .open(&stamp)
        .unwrap()
        .set_modified(old)
        .unwrap();
    assert!(walk_due(&stamp), "a day old is due");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A version-2 record as the volume writes it.
fn record(parent: u64, reason: u32, attrs: u32, name: &str) -> Vec<u8> {
    let units: Vec<u8> = name.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let len = (60 + units.len()).div_ceil(8) * 8;
    let mut r = vec![0u8; len];
    r[0..4].copy_from_slice(&(len as u32).to_le_bytes());
    r[4..6].copy_from_slice(&2u16.to_le_bytes());
    r[8..16].copy_from_slice(&99u64.to_le_bytes());
    r[16..24].copy_from_slice(&parent.to_le_bytes());
    r[40..44].copy_from_slice(&reason.to_le_bytes());
    r[52..56].copy_from_slice(&attrs.to_le_bytes());
    r[56..58].copy_from_slice(&(units.len() as u16).to_le_bytes());
    r[58..60].copy_from_slice(&60u16.to_le_bytes());
    r[60..60 + units.len()].copy_from_slice(&units);
    r
}

#[test]
fn records_are_read_as_the_volume_writes_them() {
    let mut buf = 1234i64.to_le_bytes().to_vec();
    buf.extend(record(5, FILE_CREATE, 0x20, "rapor.pdf"));
    buf.extend(record(
        7,
        RENAME_NEW_NAME,
        ATTRIBUTE_DIRECTORY,
        "Yeni klasör",
    ));
    let (next, recs) = parse(&buf).expect("a buffer");
    assert_eq!(next, 1234);
    assert_eq!(
        recs,
        vec![
            Record {
                parent: 5,
                reason: FILE_CREATE,
                dir: false,
                name: "rapor.pdf".into()
            },
            Record {
                parent: 7,
                reason: RENAME_NEW_NAME,
                dir: true,
                name: "Yeni klasör".into()
            },
        ]
    );
    // A truncated tail is not a record.
    let cut = &buf[..buf.len() - 4];
    assert_eq!(parse(cut).expect("a buffer").1.len(), 1);
}

#[test]
fn a_batch_is_one_look_per_path() {
    let r = |reason, dir, name: &str| Record {
        parent: 5,
        reason,
        dir,
        name: name.into(),
    };
    let recs = vec![
        r(FILE_CREATE, false, "a.txt"),
        r(0x2, false, "a.txt"), // data extend
        r(0x2, false, "b.txt"),
        r(FILE_DELETE, false, "b.txt"),
        r(FILE_DELETE, false, "c.txt"),
        r(FILE_CREATE, false, "c.txt"),
        r(RENAME_OLD_NAME, true, "eski"),
        r(RENAME_NEW_NAME, true, "yeni"),
    ];
    let mut forgot = 0;
    let out = settle(&recs, |_| Some("C:/".into()), || forgot += 1);
    assert_eq!(
        out,
        vec![
            ("C:/a.txt".into(), Act::Fresh),
            ("C:/b.txt".into(), Act::Gone),
            ("C:/c.txt".into(), Act::Fresh),
            ("C:/eski".into(), Act::Gone),
            ("C:/yeni".into(), Act::Fresh),
        ]
    );
    assert_eq!(
        forgot, 2,
        "a directory that moved makes the kept paths stale"
    );
    // A parent the volume no longer has: nothing to say about it.
    assert!(settle(&recs, |_| None, || {}).is_empty());
}

#[test]
fn a_position_is_kept_whole() {
    let dir = tempfile::tempdir().expect("tmpdir");
    let path = dir.path().join("journal-3");
    assert_eq!(read_kept(&path), None);
    write_kept(&path, 133_000_000_000, 98_765_432);
    assert_eq!(read_kept(&path), Some((133_000_000_000, 98_765_432)));
    std::fs::write(&path, "garbage").expect("write");
    assert_eq!(read_kept(&path), None);
}

#[test]
fn only_paths_under_a_root_count() {
    let roots = vec!["c:/users/h".to_owned()];
    assert!(under("C:/Users/H/belge.txt", &roots));
    assert!(under("C:/Users/H", &roots));
    assert!(!under("C:/Users/Hasan/belge.txt", &roots));
    assert!(under("D:/x", &["d:/".to_owned()]));
}
