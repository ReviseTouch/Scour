use super::*;

const HOME: &str = "/home/hasan";

fn tiles() -> Vec<scour_ui::disks::Tile> {
    let roots = |r: &[&str]| r.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let (cachy, depo, stick) = (
        roots(&["/"]),
        roots(&["/mnt/depo"]),
        roots(&["/run/media/S"]),
    );
    let (home_src, sys_src, depo_src) = (
        roots(&["/home/hasan"]),
        roots(&["/usr"]),
        roots(&["/mnt/depo"]),
    );
    let disk = |id, label, kind, roots, state| scour_ui::disks::DiskIn {
        id,
        label,
        kind,
        fs: "fs",
        roots,
        size: 1,
        state,
        live: false,
        walking: false,
    };
    let ins = [
        disk("c", "CACHY", "system", &cachy, "manual"),
        disk("d", "Depo", "fixed", &depo, "configured"),
        disk("s", "STICK", "removable", &stick, "asking"),
    ];
    let src = |name, roots| scour_ui::disks::SourceIn { name, roots };
    let srcs = [
        src("home", &home_src),
        src("sys", &sys_src),
        src("depo", &depo_src),
    ];
    scour_ui::disks::tiles(&ins, &srcs, |_| None)
}

#[test]
fn the_chip_names_what_the_group_holds_and_rings_a_disk_waiting() {
    let t = tiles();
    let chip = |q: &str| chip(&t, q, HOME, "Everywhere");
    let all = chip("rapor");
    assert_eq!(all.label, "Everywhere");
    assert_eq!(
        all.dots,
        [
            ("system", false, true),
            ("fixed", false, true),
            ("removable", true, false)
        ]
    );
    let depo = chip("rapor under:/mnt/depo");
    assert_eq!(depo.label, "Depo");
    assert_eq!(depo.dots[0], ("system", false, false));
    // A whole tile by its name, a folder by its last step.
    let both = chip("under:~|under:/usr|under:/mnt/depo/PROJELER");
    assert_eq!(both.label, "CACHY +1");
    assert_eq!(
        chip("under:~|under:/usr|under:/mnt/depo/x").label,
        "CACHY + x"
    );
    assert!(both.dots[1].2);
    assert_eq!(chip("under:~/Belgeler").label, "Belgeler");
}
