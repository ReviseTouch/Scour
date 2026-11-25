//! The owner's machine as tiles, for the place tests: the system disk `/`
//! indexed through `home` and `sistem`, Depo at `/mnt/depo`, a stick asking.

use crate::disks::{DiskIn, SourceIn, Tile, tiles};

pub const HOME: &str = "/home/hasan";

/// The system disk's roots in the order its tile gives them.
pub const SYSTEM: &str = "under:/etc|under:~|under:/opt|under:/usr|under:/var";

pub fn machine() -> Vec<Tile> {
    let owned = |r: &[&str]| r.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let (slash, depo, stick) = (
        owned(&["/"]),
        owned(&["/mnt/depo"]),
        owned(&["/run/media/hasan/STICK"]),
    );
    let disk = |id, label, kind, roots, state| DiskIn {
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
    let disks = [
        disk("cachy", "CACHY", "system", &slash, "manual"),
        disk("depo", "Depo", "fixed", &depo, "configured"),
        disk("stick", "STICK", "removable", &stick, "asking"),
    ];
    let (home, sistem) = (owned(&[HOME]), owned(&["/usr", "/etc", "/opt", "/var"]));
    let sources = [
        SourceIn {
            name: "home",
            roots: &home,
        },
        SourceIn {
            name: "sistem",
            roots: &sistem,
        },
        SourceIn {
            name: "depo",
            roots: &depo,
        },
    ];
    tiles(&disks, &sources, |_| None)
}
