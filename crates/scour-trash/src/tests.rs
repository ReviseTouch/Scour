#[test]
fn a_trash_directory_somebody_else_made_or_linked_is_not_this_accounts() {
    let tmp = std::env::temp_dir().join(format!("scour-owndir-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("elsewhere")).expect("mkdir");
    let uid = unsafe { libc::getuid() };

    // Made here: this account's, and for nobody else to read.
    assert!(super::own_dir(&tmp.join("fresh"), uid));
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(tmp.join("fresh"))
        .expect("meta")
        .permissions()
        .mode();
    assert_eq!(mode & 0o077, 0, "{mode:o}");
    // A link put where the directory would be leads wherever its maker likes.
    std::os::unix::fs::symlink(tmp.join("elsewhere"), tmp.join("linked")).expect("link");
    assert!(!super::own_dir(&tmp.join("linked"), uid));
    // And a directory that is there but another account's is theirs.
    assert!(!super::own_dir(&tmp.join("fresh"), uid.wrapping_add(1)));
    let _ = std::fs::remove_dir_all(&tmp);
}

#[test]
fn erasing_takes_a_file_a_tree_and_a_link_but_not_what_the_link_points_at() {
    let tmp = std::env::temp_dir().join(format!("scour-erase-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(tmp.join("tree/deeper")).expect("mkdir");
    std::fs::write(tmp.join("tree/deeper/f.txt"), b"x").expect("write");
    std::fs::write(tmp.join("one.txt"), b"x").expect("write");
    std::fs::create_dir_all(tmp.join("kept")).expect("mkdir");
    std::fs::write(tmp.join("kept/inside.txt"), b"x").expect("write");
    std::os::unix::fs::symlink(tmp.join("kept"), tmp.join("link")).expect("link");

    super::erase(&tmp.join("one.txt")).expect("file");
    super::erase(&tmp.join("tree")).expect("tree");
    super::erase(&tmp.join("link")).expect("link");
    assert!(!tmp.join("one.txt").exists());
    assert!(!tmp.join("tree").exists());
    assert!(std::fs::symlink_metadata(tmp.join("link")).is_err());
    assert!(
        tmp.join("kept/inside.txt").exists(),
        "the link's target went too"
    );

    assert!(matches!(
        super::erase(&tmp.join("never-was")),
        Err(super::Error::Missing(_))
    ));
    assert!(matches!(
        super::erase(std::path::Path::new("/")),
        Err(super::Error::Unnamed(_))
    ));
    let _ = std::fs::remove_dir_all(&tmp);
}
use super::*;
use std::path::{Path, PathBuf};

/// A trash of our own, so the test never touches the desktop's.
/// A directory of its own per call. The clock alone was not enough: two
/// tests running side by side took one name, and one removed the other's.
fn sandbox() -> PathBuf {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let at = std::env::temp_dir().join(format!(
        "scour-trash-test-{}-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .subsec_nanos()
    ));
    std::fs::create_dir_all(&at).unwrap();
    at
}

#[test]
fn a_file_moves_and_leaves_a_note_saying_where_it_came_from() {
    let box_ = sandbox();
    let data = box_.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let file = box_.join("notes.txt");
    std::fs::write(&file, b"hello").unwrap();

    let landed = into(&file, Some(data.join("Trash"))).expect("trashed");
    assert!(!file.exists(), "the original is gone");
    assert_eq!(std::fs::read(&landed).unwrap(), b"hello", "bytes intact");

    let info = data.join("Trash/info").join(format!(
        "{}.trashinfo",
        landed.file_name().unwrap().to_string_lossy()
    ));
    let note = std::fs::read_to_string(&info).unwrap();
    assert!(note.starts_with("[Trash Info]\n"), "{note}");
    assert!(
        note.contains(&format!("Path={}", encode(&file.to_string_lossy()))),
        "{note}"
    );
    assert!(note.contains("DeletionDate=20"), "{note}");

    std::fs::remove_dir_all(&box_).ok();
}

/// `files/` can hold a name whose `.trashinfo` is gone — another program
/// lost it, or a crash between the two. Claiming only the info name and
/// renaming onto it replaced those bytes for good.
#[test]
fn a_file_left_in_the_trash_without_its_note_is_not_written_over() {
    let box_ = sandbox();
    let trash = box_.join("data/Trash");
    std::fs::create_dir_all(trash.join("files")).unwrap();
    std::fs::create_dir_all(trash.join("info")).unwrap();
    std::fs::write(trash.join("files/orphan.txt"), b"OLD").unwrap();
    let file = box_.join("orphan.txt");
    std::fs::write(&file, b"NEW").unwrap();

    let landed = into(&file, Some(trash.clone())).expect("trashed");
    assert_eq!(
        std::fs::read(trash.join("files/orphan.txt")).unwrap(),
        b"OLD"
    );
    assert_eq!(landed, trash.join("files/orphan.1.txt"));
    assert_eq!(std::fs::read(&landed).unwrap(), b"NEW");
    assert!(trash.join("info/orphan.1.txt.trashinfo").exists());
    assert!(
        !trash.join("info/orphan.txt.trashinfo").exists(),
        "the name it gave up is not left claimed"
    );
    std::fs::remove_dir_all(&box_).ok();
}

#[test]
fn a_move_onto_something_is_refused_not_made() {
    let box_ = sandbox();
    std::fs::write(box_.join("a"), b"a").unwrap();
    std::fs::write(box_.join("b"), b"b").unwrap();
    let e = move_new(&box_.join("a"), &box_.join("b")).expect_err("b is there");
    assert_eq!(e.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(std::fs::read(box_.join("b")).unwrap(), b"b");
    move_new(&box_.join("a"), &box_.join("c")).expect("c is free");
    assert_eq!(std::fs::read(box_.join("c")).unwrap(), b"a");
    std::fs::remove_dir_all(&box_).ok();
}

#[test]
fn a_second_file_of_the_same_name_gets_a_number_rather_than_the_first_ones_place() {
    let box_ = sandbox();
    let data = box_.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let mut landed = Vec::new();
    for (i, sub) in ["a", "b", "c"].iter().enumerate() {
        let dir = box_.join(sub);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("notes.tar.gz");
        std::fs::write(&file, format!("{i}")).unwrap();
        landed.push(into(&file, Some(data.join("Trash"))).expect("trashed"));
    }

    let names: Vec<String> = landed
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names[0], "notes.tar.gz");
    // The extension survives the number, so the file still opens.
    assert_eq!(names[1], "notes.tar.1.gz");
    assert_eq!(names[2], "notes.tar.2.gz");
    // All three are still distinguishable by their contents.
    for (i, p) in landed.iter().enumerate() {
        assert_eq!(std::fs::read_to_string(p).unwrap(), i.to_string());
    }

    std::fs::remove_dir_all(&box_).ok();
}

#[test]
fn a_directory_goes_whole() {
    let box_ = sandbox();
    let data = box_.join("data");
    std::fs::create_dir_all(&data).unwrap();
    let tree = box_.join("project");
    std::fs::create_dir_all(tree.join("src")).unwrap();
    std::fs::write(tree.join("src/main.rs"), b"fn main() {}").unwrap();

    let landed = into(&tree, Some(data.join("Trash"))).expect("trashed");
    assert!(!tree.exists());
    assert_eq!(
        std::fs::read_to_string(landed.join("src/main.rs")).unwrap(),
        "fn main() {}"
    );

    std::fs::remove_dir_all(&box_).ok();
}

#[test]
fn what_is_not_there_is_not_trashed_and_says_so() {
    let box_ = sandbox();
    let missing = box_.join("never-existed");
    assert!(matches!(into(&missing, None), Err(Error::Missing(_))));
    assert!(matches!(into(Path::new("/"), None), Err(Error::Unnamed(_))));
    std::fs::remove_dir_all(&box_).ok();
}

#[test]
fn the_path_in_the_note_survives_a_round_trip() {
    // A space, an accent and a percent sign all appear in real file names.
    assert_eq!(
        encode("/home/a b/çay%1.txt"),
        "/home/a%20b/%C3%A7ay%251.txt"
    );
    assert_eq!(encode("/plain/path.txt"), "/plain/path.txt");
}

#[test]
fn an_extension_is_the_last_dot_and_a_hidden_file_has_none() {
    assert_eq!(
        split_extension("notes.tar.gz"),
        ("notes.tar".into(), "gz".into())
    );
    assert_eq!(split_extension("notes"), ("notes".into(), "".into()));
    assert_eq!(split_extension(".bashrc"), (".bashrc".into(), "".into()));
}
