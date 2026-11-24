//! The cache against a real directory.

use scour_textcache::{Kept, Key, Store};

fn key(path: &str, size: u64, mtime: i64) -> Key {
    Key {
        path: path.into(),
        size,
        mtime,
    }
}

#[test]
fn a_text_is_found_again_until_its_file_changes() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), 0).unwrap();
    let k = key("/d/a.pdf", 100, 1_000);
    store.put(&k, &Kept::Words("İcra dairesi".into())).unwrap();
    assert_eq!(store.get(&k, 1), Some(Kept::Words("İcra dairesi".into())));
    // FAT keeps time in two-second steps.
    assert!(store.get(&key("/d/a.pdf", 100, 1_001), 1).is_some());
    assert_eq!(store.get(&key("/d/a.pdf", 101, 1_000), 1), None);
    assert_eq!(store.get(&key("/d/a.pdf", 100, 1_005), 1), None);
    store.flush().unwrap();
    drop(store);
    let again = Store::open(dir.path(), 0).unwrap();
    assert_eq!(again.get(&k, 1), Some(Kept::Words("İcra dairesi".into())));
}

#[test]
fn a_failure_is_tried_again_only_under_new_readers() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), 0).unwrap();
    let k = key("/d/broken.doc", 5, 9);
    store.put(&k, &Kept::Failed { version: 3 }).unwrap();
    assert_eq!(store.get(&k, 3), Some(Kept::Failed { version: 3 }));
    assert_eq!(store.get(&k, 4), None);
    store.put(&key("/d/scan.pdf", 5, 9), &Kept::NoText).unwrap();
    assert_eq!(store.get(&key("/d/scan.pdf", 5, 9), 4), Some(Kept::NoText));
}

#[test]
fn changed_documents_leave_dead_text_that_compaction_removes() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), 0).unwrap();
    let body = "metin ".repeat(200_000);
    for round in 0..3 {
        for i in 0..4 {
            let k = key(&format!("/d/{i}.pdf"), 10, round);
            store
                .put(&k, &Kept::Words(format!("{round} {body}")))
                .unwrap();
        }
    }
    store.flush().unwrap();
    let before = store.stats();
    assert!(before.dead > before.live, "{before:?}");
    assert!(store.compact().unwrap());
    let after = store.stats();
    assert_eq!((after.dead, after.documents), (0, 4));
    assert!(after.disk < before.disk);
    for i in 0..4 {
        let got = store.get(&key(&format!("/d/{i}.pdf"), 10, 2), 1);
        assert!(matches!(got, Some(Kept::Words(t)) if t.starts_with("2 ")));
    }
}

#[test]
fn a_full_cache_gives_up_its_oldest_blocks() {
    let dir = tempfile::tempdir().unwrap();
    // Text that does not compress: random letters.
    let mut seed = 7u64;
    let mut noise = |n: usize| -> String {
        (0..n)
            .map(|_| {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                (b'a' + (seed >> 59) as u8 % 26) as char
            })
            .collect()
    };
    let store = Store::open(dir.path(), 3 << 20).unwrap();
    for i in 0..8 {
        store
            .put(&key(&format!("/d/{i}"), 1, 1), &Kept::Words(noise(1 << 20)))
            .unwrap();
    }
    store.flush().unwrap();
    assert!(store.stats().disk <= 3 << 20);
    assert!(store.get(&key("/d/0", 1, 1), 0).is_none());
    assert!(store.get(&key("/d/7", 1, 1), 0).is_some());
}

#[test]
fn clearing_forgets_everything() {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path(), 0).unwrap();
    store
        .put(&key("/x", 1, 1), &Kept::Words("x".into()))
        .unwrap();
    store.flush().unwrap();
    store.clear().unwrap();
    assert_eq!(store.get(&key("/x", 1, 1), 0), None);
    assert_eq!(store.stats().disk, 0);
}

#[cfg(unix)]
#[test]
fn only_its_owner_can_read_it() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("cache");
    let store = Store::open(&root, 0).unwrap();
    store
        .put(&key("/x", 1, 1), &Kept::Words("gizli".into()))
        .unwrap();
    store.flush().unwrap();
    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&root), 0o700);
    for entry in std::fs::read_dir(&root).unwrap().flatten() {
        assert_eq!(mode(&entry.path()), 0o600, "{:?}", entry.path());
    }
}
