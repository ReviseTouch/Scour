use super::*;
use std::path::{Path, PathBuf};

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("scour-sendto-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("mkdir");
    d
}

#[test]
fn a_taken_name_gets_the_first_free_number_and_keeps_its_extension() {
    let d = scratch("names");
    assert_eq!(free_name(&d, "a.txt"), d.join("a.txt"));
    std::fs::write(d.join("a.txt"), b"").expect("write");
    std::fs::write(d.join("a (2).txt"), b"").expect("write");
    assert_eq!(free_name(&d, "a.txt"), d.join("a (3).txt"));
    std::fs::write(d.join("b.tar.gz"), b"").expect("write");
    assert_eq!(free_name(&d, "b.tar.gz"), d.join("b (2).tar.gz"));
    std::fs::write(d.join(".hidden"), b"").expect("write");
    assert_eq!(free_name(&d, ".hidden"), d.join(".hidden (2)"));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn the_desktop_is_where_user_dirs_says() {
    let home = Path::new("/home/u");
    let text =
        "# comment\nXDG_DOWNLOAD_DIR=\"$HOME/İndirilenler\"\nXDG_DESKTOP_DIR=\"$HOME/Masaüstü\"\n";
    assert_eq!(
        desktop_in(home, text),
        Some(PathBuf::from("/home/u/Masaüstü"))
    );
    assert_eq!(
        desktop_in(home, "XDG_DESKTOP_DIR=\"/srv/desk\"\n"),
        Some(PathBuf::from("/srv/desk"))
    );
    assert_eq!(desktop_in(home, ""), None);
}

#[test]
fn a_drive_is_a_mount_under_the_users_media_and_nothing_else() {
    let info = "\
36 1 0:30 / / rw - btrfs /dev/nvme0n1p5 rw
90 36 8:17 / /run/media/hasan/USB\\040BELLEK rw - vfat /dev/sdb1 rw
91 36 8:33 / /media/hasan/Yedek rw - ext4 /dev/sdc1 rw
92 36 8:49 / /run/media/other/Theirs rw - vfat /dev/sdd1 rw
93 90 8:50 / /run/media/hasan/USB\\040BELLEK/inner rw - vfat /dev/sdd2 rw
94 36 0:60 / /mnt/depo rw - ntfs3 /dev/nvme1n1p2 rw";
    assert_eq!(
        drives_in(info, "hasan"),
        vec![
            (
                "USB BELLEK".to_owned(),
                PathBuf::from("/run/media/hasan/USB BELLEK")
            ),
            ("Yedek".to_owned(), PathBuf::from("/media/hasan/Yedek")),
        ]
    );
}

#[test]
fn an_archive_is_named_after_what_it_holds() {
    let d = scratch("stems");
    std::fs::write(d.join("report.pdf"), b"").expect("write");
    std::fs::create_dir_all(d.join("photos.2024")).expect("mkdir");
    std::fs::write(d.join("b.txt"), b"").expect("write");
    assert_eq!(archive_stem(&[&d.join("report.pdf")]), "report");
    assert_eq!(archive_stem(&[&d.join("photos.2024")]), "photos.2024");
    let folder = d.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(
        archive_stem(&[&d.join("report.pdf"), &d.join("b.txt")]),
        folder
    );
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_place_not_offered_is_refused_before_anything_starts() {
    let d = scratch("refuse");
    std::fs::write(d.join("f.txt"), b"").expect("write");
    let f = d.join("f.txt");
    assert!(matches!(
        send("drive:/etc", &[f.as_path()]),
        Err(Error::Unknown(_))
    ));
    assert!(matches!(
        send("nowhere", &[f.as_path()]),
        Err(Error::Unknown(_))
    ));
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn octal_escapes_become_the_bytes_they_name() {
    assert_eq!(unescape("/a\\040b\\011c"), "/a b\tc");
    assert_eq!(unescape("/plain"), "/plain");
    assert_eq!(unescape("/end\\04"), "/end\\04");
}
