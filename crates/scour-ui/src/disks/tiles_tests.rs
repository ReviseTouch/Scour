use super::super::{STRIP_FLOOR, strip};
use super::*;

struct Disk {
    id: &'static str,
    label: &'static str,
    kind: &'static str,
    fs: &'static str,
    roots: Vec<String>,
    size: u64,
    state: &'static str,
    live: bool,
    walking: bool,
}

fn disk(id: &'static str, root: &str, kind: &'static str, state: &'static str) -> Disk {
    Disk {
        id,
        label: "",
        kind,
        fs: "ext4",
        roots: vec![root.into()],
        size: 1 << 30,
        state,
        live: false,
        walking: false,
    }
}

fn borrow(disks: &[Disk]) -> Vec<DiskIn<'_>> {
    disks
        .iter()
        .map(|d| DiskIn {
            id: d.id,
            label: d.label,
            kind: d.kind,
            fs: d.fs,
            roots: &d.roots,
            size: d.size,
            state: d.state,
            live: d.live,
            walking: d.walking,
        })
        .collect()
}

fn roots(rs: &[&str]) -> Vec<String> {
    rs.iter().map(|r| r.to_string()).collect()
}

/// The owner's machine: the system disk not indexed as a whole but holding
/// `home` and `sistem`, Depo set in config.toml, a stick waiting for an
/// answer and a network share.
fn machine() -> (Vec<Disk>, Vec<(&'static str, Vec<String>)>) {
    let mut cachy = disk("uuid:cachy", "/", "system", "manual");
    cachy.label = "CACHY";
    cachy.fs = "btrfs";
    let mut depo = disk("uuid:depo", "/mnt/depo", "fixed", "configured");
    depo.label = "Depo";
    depo.fs = "ntfs3";
    depo.live = true;
    let stick = disk("uuid:AB12", "/run/media/hasan/STICK", "removable", "asking");
    let mut share = disk("net:nas", "/mnt/nas", "network", "chosen");
    share.size = 0;
    let sources = vec![
        ("home", roots(&["/home/hasan"])),
        ("depo", roots(&["/mnt/depo"])),
        ("sistem", roots(&["/usr", "/etc", "/opt", "/var"])),
        ("net:nas", roots(&["/mnt/nas"])),
    ];
    (vec![cachy, depo, stick, share], sources)
}

fn tiles_of(disks: &[Disk], sources: &[(&'static str, Vec<String>)]) -> Vec<Tile> {
    let src: Vec<SourceIn> = sources
        .iter()
        .map(|(name, roots)| SourceIn { name, roots })
        .collect();
    let files = |id: &str| match id {
        "uuid:cachy" => Some(4_803_676),
        "uuid:depo" => Some(1_755_949),
        "uuid:AB12" => Some(5),
        "net:nas" => Some(1_000),
        _ => None,
    };
    tiles(&borrow(disks), &src, files)
}

#[test]
fn the_owners_machine() {
    let (disks, sources) = machine();
    let t = tiles_of(&disks, &sources);
    let shown: Vec<Shown> = t.iter().map(|t| t.shown).collect();
    assert_eq!(
        shown,
        [Shown::Partly, Shown::Live, Shown::Asking, Shown::Polled]
    );
    assert_eq!(t[0].name, "CACHY");
    assert_eq!(t[0].kind, "system");
    assert_eq!(t[0].root, "/");
    assert_eq!(t[0].roots, ["/etc", "/home/hasan", "/opt", "/usr", "/var"]);
    assert!(!t[0].whole);
    assert!(t[1].whole && t[1].roots == ["/mnt/depo"]);
    assert_eq!(t[2].name, "/run/media/hasan/STICK", "no label, the root");
}

#[test]
fn a_disk_is_counted_over_the_sources_on_it() {
    let (disks, sources) = machine();
    let t = tiles_of(&disks, &sources);
    assert_eq!(
        t[0].count.as_deref(),
        Some("file: under:/etc|under:/home/hasan|under:/opt|under:/usr|under:/var")
    );
    assert_eq!(t[1].count.as_deref(), Some("file: under:/mnt/depo"));
    assert_eq!(t[2].count, None, "nothing on the stick is indexed");
    assert_eq!(t[2].files, None, "nor counted");
    assert_eq!(t[0].files, Some(4_803_676));
}

#[test]
fn a_disk_holding_the_root_leaves_the_disks_inside_it_out() {
    let mut cachy = disk("uuid:cachy", "/", "system", "auto");
    cachy.roots.push("/home".into());
    let disks = [cachy, disk("uuid:depo", "/mnt/depo/", "fixed", "auto")];
    let sources = [
        ("uuid:cachy", roots(&["/"])),
        ("uuid:depo", roots(&["/mnt/depo"])),
    ];
    let t = tiles_of(&disks, &sources);
    assert_eq!(
        t[0].count.as_deref(),
        Some("file: under:/ !under:/mnt/depo"),
        "its own second root is not another disk"
    );
    assert!(t[0].whole);
    assert_eq!(t[1].root, "/mnt/depo");
}

#[test]
fn an_indexed_disk_is_counted_before_its_source_is_known() {
    let disks = [disk("uuid:x", "/mnt/x", "fixed", "chosen")];
    let t = tiles(&borrow(&disks), &[], |_| None);
    assert_eq!(t[0].count.as_deref(), Some("file: under:/mnt/x"));
    assert_eq!(t[0].shown, Shown::Polled);
    let quoted = [disk(
        "uuid:y",
        "/run/media/u/MY STICK",
        "removable",
        "chosen",
    )];
    let t = tiles(&borrow(&quoted), &[], |_| None);
    assert_eq!(
        t[0].count.as_deref(),
        Some(r#"file: under:"/run/media/u/MY STICK""#)
    );
}

#[test]
fn only_a_disk_with_nothing_indexed_may_be_indexed() {
    let (disks, sources) = machine();
    let t = tiles_of(&disks, &sources);
    let index_it: Vec<bool> = t.iter().map(|t| t.index_it).collect();
    assert_eq!(
        index_it,
        [false, false, true, false],
        "never the partly one"
    );
    let manual = [disk("uuid:m", "/", "system", "manual")];
    let t = tiles(&borrow(&manual), &[], |_| None);
    assert_eq!(t[0].shown, Shown::Off);
    assert!(t[0].index_it && !t[0].choosable());
    let later = [disk("uuid:l", "/run/media/u/L", "removable", "later")];
    assert_eq!(
        tiles(&borrow(&later), &[], |_| None)[0].shown,
        Shown::Asking
    );
}

#[test]
fn walking_wins_over_live_and_partly() {
    let (mut disks, sources) = machine();
    disks[0].walking = true;
    disks[1].walking = true;
    let t = tiles_of(&disks, &sources);
    assert_eq!(t[0].shown, Shown::Walking);
    assert_eq!(t[1].shown, Shown::Walking);
    assert!(t[0].roots.len() == 5, "its tags stay");
}

#[test]
fn every_kind_is_known_and_a_strange_one_is_fixed() {
    for k in super::super::KINDS {
        let d = [disk("i", "/x", k, "auto")];
        assert_eq!(tiles(&borrow(&d), &[], |_| None)[0].kind, k);
    }
    let d = [disk("i", "/x", "tape", "auto")];
    assert_eq!(tiles(&borrow(&d), &[], |_| None)[0].kind, "fixed");
}

#[test]
fn a_path_is_on_the_disk_with_the_longest_root_above_it() {
    let (disks, _) = machine();
    let d = borrow(&disks);
    assert_eq!(disk_of("/mnt/depo/x", &d), Some("uuid:depo"));
    assert_eq!(disk_of("/mnt/depot", &d), Some("uuid:cachy"));
    assert_eq!(disk_of("/mnt/depo", &d), Some("uuid:depo"));
    assert_eq!(disk_of("relative", &d), None);
}

#[test]
fn the_strip_shares_by_files_with_a_floor() {
    let (disks, sources) = machine();
    let t = tiles_of(&disks, &sources);
    let s = strip(&t);
    let at: Vec<usize> = s.iter().map(|s| s.at).collect();
    assert_eq!(at, [0, 1, 3], "the stick is not on the strip");
    let sum: f64 = s.iter().map(|s| s.share).sum();
    assert!((sum - 1.0).abs() < 1e-9, "{sum}");
    assert_eq!(s[2].share, STRIP_FLOOR, "a thousand files still show");
    let cachy = 4_803_676.0 / (4_803_676.0 + 1_755_949.0) * (1.0 - STRIP_FLOOR);
    assert!((s[0].share - cachy).abs() < 1e-9);
}

#[test]
fn an_uncounted_strip_is_even() {
    let (disks, sources) = machine();
    let src: Vec<SourceIn> = sources
        .iter()
        .map(|(name, roots)| SourceIn { name, roots })
        .collect();
    let t = tiles(&borrow(&disks), &src, |_| None);
    let s = strip(&t);
    assert!(s.iter().all(|s| (s.share - 1.0 / 3.0).abs() < 1e-9));
    assert!(strip(&[]).is_empty());
}

#[test]
fn many_small_disks_all_reach_the_floor() {
    let disks: Vec<Disk> = (0..10)
        .map(|i| {
            disk(
                ["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"][i],
                "/x",
                "fixed",
                "auto",
            )
        })
        .collect();
    let src: Vec<SourceIn> = Vec::new();
    let t = tiles(&borrow(&disks), &src, |id| {
        Some(if id == "a" { 1_000_000 } else { 1 })
    });
    let s = strip(&t);
    assert!(s[1..].iter().all(|s| s.share == STRIP_FLOOR));
    assert!((s[0].share - (1.0 - 9.0 * STRIP_FLOOR)).abs() < 1e-9);
}
