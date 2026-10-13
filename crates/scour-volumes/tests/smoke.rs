//! The public API against mount tables written out by hand.

use scour_volumes::{Kind, Probe, Volume, parse_mountinfo, volumes};

/// A machine whose answers are fixed: USB devices are removable, every block
/// device has a UUID named after it, and every disk is a terabyte.
struct Fixed;

impl Probe for Fixed {
    fn names(&self, source: &str) -> (Option<String>, Option<String>) {
        let name = source.trim_start_matches("/dev/");
        let label = (name == "sdb1").then(|| "Yedek".to_string());
        (Some(format!("U-{name}")), label)
    }
    fn removable(&self, source: &str) -> bool {
        source == "/dev/sdb1"
    }
    fn size(&self, _at: &str) -> u64 {
        1 << 40
    }
}

/// This machine's shape: btrfs subvolumes for `/` and `/home`, an NTFS disk,
/// an EFI partition, a USB stick, two shares, a container, a snap and a bind.
const TABLE: &str = "\
22 1 0:30 /@ / rw,noatime - btrfs /dev/nvme1n1p5 rw,ssd
23 22 0:30 /@home /home rw,noatime - btrfs /dev/nvme1n1p5 rw,ssd
24 22 0:5 / /proc rw - proc proc rw
25 22 0:22 / /run rw - tmpfs tmpfs rw
26 22 259:1 / /boot/efi rw - vfat /dev/nvme1n1p1 rw
27 22 259:7 / /mnt/depo rw - ntfs3 /dev/nvme0n1p2 rw
28 25 8:17 / /run/media/hasan/Yedek rw - exfat /dev/sdb1 rw
29 22 0:50 / /srv/nfs rw - nfs4 nas:/export rw
30 22 0:51 / /mnt/paylas rw - cifs //nas/paylas rw
31 22 0:60 / /var/lib/docker/overlay2/x/merged rw - overlay overlay rw
32 22 7:3 / /snap/core22/1 ro - squashfs /dev/loop3 ro
33 22 0:30 /@home/hasan/proje /srv/proje rw - btrfs /dev/nvme1n1p5 rw
";

fn found() -> Vec<Volume> {
    volumes(&parse_mountinfo(TABLE), &Fixed)
}

fn at<'a>(v: &'a [Volume], root: &str) -> Option<&'a Volume> {
    v.iter().find(|x| x.roots.iter().any(|r| r == root))
}

#[test]
fn every_disk_a_person_keeps_files_on_is_a_volume_and_nothing_else_is() {
    let v = found();
    let roots: Vec<Vec<String>> = v.iter().map(|x| x.roots.clone()).collect();
    assert_eq!(
        roots,
        [
            vec!["/"],
            vec!["/mnt/depo"],
            vec!["/run/media/hasan/Yedek"],
            vec!["/srv/nfs"],
            vec!["/mnt/paylas"],
        ],
        "proc, tmpfs, the EFI partition, a container, a snap and a bind mount are not disks, \
         and `/home` is the same disk as `/`, inside it"
    );
}

#[test]
fn each_volume_says_what_kind_it_is() {
    let v = found();
    let kind = |root| at(&v, root).map(|x| x.kind);
    assert_eq!(kind("/"), Some(Kind::System));
    assert_eq!(kind("/mnt/depo"), Some(Kind::Fixed));
    assert_eq!(kind("/run/media/hasan/Yedek"), Some(Kind::Removable));
    assert_eq!(kind("/srv/nfs"), Some(Kind::Network));
    assert_eq!(kind("/mnt/paylas"), Some(Kind::Network));
    assert!(Kind::Fixed.is_built_in() && Kind::System.is_built_in());
    assert!(!Kind::Removable.is_built_in() && !Kind::Network.is_built_in());
}

#[test]
fn an_id_is_the_filesystems_and_survives_a_remount() {
    let v = found();
    let id = |root| at(&v, root).map(|x| x.id.clone()).unwrap_or_default();
    assert_eq!(id("/"), "uuid:U-nvme1n1p5");
    assert_eq!(id("/mnt/depo"), "uuid:U-nvme0n1p2");
    assert_eq!(id("/srv/nfs"), "net:nas:/export");
}

/// A pool and a subvolume mounted side by side are one disk with two roots;
/// the system's disk is the system's however its places were listed.
#[test]
fn one_filesystem_in_two_separate_places_is_one_volume_with_both() {
    let table = "\
50 1 0:30 /@data /data rw - btrfs /dev/sda2 rw
51 1 0:30 / /pool rw - btrfs /dev/sda2 rw
52 1 0:31 /@ / rw - btrfs /dev/sda3 rw
53 1 0:31 /@home /home rw - btrfs /dev/sda3 rw
";
    let v = volumes(&parse_mountinfo(table), &Fixed);
    assert_eq!(v.len(), 2, "{v:?}");
    assert_eq!(v[0].roots, ["/data", "/pool"]);
    assert_eq!(v[1].roots, ["/"]);
    assert_eq!(v[1].kind, Kind::System);
}

#[test]
fn a_share_is_not_asked_for_its_size_and_a_disk_is() {
    let v = found();
    assert_eq!(at(&v, "/srv/nfs").map(|x| x.size), Some(0));
    assert_eq!(at(&v, "/mnt/depo").map(|x| x.size), Some(1 << 40));
    assert_eq!(
        at(&v, "/run/media/hasan/Yedek").map(|x| x.label.as_str()),
        Some("Yedek")
    );
}

#[test]
fn a_disc_and_a_disk_under_media_are_plugged_in() {
    let table = "\
40 1 8:1 / / rw - ext4 /dev/sda1 rw
41 40 11:0 / /media/cdrom ro - iso9660 /dev/sr0 ro
42 40 8:33 / /data rw - ext4 /dev/sdc1 rw
";
    let v = volumes(&parse_mountinfo(table), &Fixed);
    assert_eq!(
        at(&v, "/media/cdrom").map(|x| x.kind),
        Some(Kind::Removable)
    );
    assert_eq!(at(&v, "/data").map(|x| x.kind), Some(Kind::Fixed));
}

#[cfg(target_os = "linux")]
#[test]
fn this_machine_has_a_system_volume() {
    let v = scour_volumes::look();
    assert!(
        v.iter()
            .any(|x| x.roots.iter().any(|r| r == "/") && x.kind == Kind::System),
        "no `/` among {v:?}"
    );
}

/// Nothing mounted while it waits: it comes back at the timeout, saying no.
/// A mount at that moment would make this a yes, which is why the bound is
/// the only thing asserted when it is one.
#[test]
fn waiting_with_nothing_mounted_ends_at_the_timeout() {
    let mut changes = scour_volumes::Changes::new();
    let began = std::time::Instant::now();
    let changed = changes.wait(std::time::Duration::from_millis(150));
    let took = began.elapsed();
    assert!(took < std::time::Duration::from_secs(2), "{took:?}");
    if !changed {
        assert!(took >= std::time::Duration::from_millis(140), "{took:?}");
    }
}
