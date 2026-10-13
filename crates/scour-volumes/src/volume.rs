//! What a volume is, as every face and the service see it.

use serde::{Deserialize, Serialize};

/// One filesystem a person would call a disk: mounted, with files on it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Volume {
    /// The same across reboots, remounts and a new drive letter: a filesystem's
    /// UUID, a Windows volume serial, a network share's address.
    pub id: String,
    /// Where it is reached: `/`, `/mnt/data`, `D:\`. One filesystem mounted in
    /// two places that do not hold each other — a btrfs pool and a subvolume
    /// beside it — has both; one mounted inside the other has the outer one.
    pub roots: Vec<String>,
    /// The name its owner gave it, if any. Nothing here translates.
    pub label: String,
    pub kind: Kind,
    /// The filesystem as the system names it: `btrfs`, `ntfs3`, `NTFS`, `nfs4`.
    pub fs: String,
    /// In bytes; zero when it was not asked, as for a network share.
    pub size: u64,
}

/// What decides whether a volume is indexed without asking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// The one the system runs from: `/`, or the Windows drive.
    System,
    /// Built in: indexed unless somebody says otherwise.
    #[default]
    Fixed,
    /// Plugged in — USB, a card, an optical disc: asked about first.
    Removable,
    /// Reached over a network: asked about first.
    Network,
}

impl Kind {
    /// Indexed without asking.
    pub fn is_built_in(self) -> bool {
        matches!(self, Kind::System | Kind::Fixed)
    }
}
