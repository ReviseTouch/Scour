//! What the service holds on this machine — its files, its memory, its share
//! of the processor, and Scour's other programs running beside it — for the
//! panel a face's status bar opens. The counters are running totals: a face
//! asks twice and compares, so the service keeps no history of its own.

use serde::{Deserialize, Serialize};

/// The answer to [`crate::Request::Resources`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Resources {
    /// The service's own version.
    #[serde(default)]
    pub version: String,
    /// Since this service started.
    #[serde(default)]
    pub uptime_ms: u64,
    /// What it keeps on disk, in the order a panel lists it.
    #[serde(default)]
    pub files: Vec<Held>,
    /// Rows outside the stored order: what `Maintain { Rebuild }` puts in order.
    #[serde(default)]
    pub unsorted: u64,
    #[serde(default)]
    pub memory: Memory,
    #[serde(default)]
    pub cpu: Cpu,
    /// Scour's other programs running for this account, by kind.
    #[serde(default)]
    pub running: Vec<Running>,
    /// [`crate::Request::Restart`] brings it back: something starts it again
    /// behind itself. False where nothing would.
    #[serde(default)]
    pub restartable: bool,
}

/// One kind of thing the service keeps on disk.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeldKind {
    /// The index itself. Cleared, it is walked again from nothing.
    #[default]
    Index,
    /// Text read out of documents, kept for content search.
    Texts,
    /// Previews made of documents: office files turned into PDF.
    Previews,
    /// The page window's browser profile, when the launcher made one.
    Page,
    /// Settings and logs. Never cleared from a panel.
    State,
}

/// What one [`HeldKind`] takes, and where.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Held {
    pub kind: HeldKind,
    /// The folder, as a full path.
    pub path: String,
    pub bytes: u64,
    /// Entries for the index, documents for texts; 0 where it means nothing.
    #[serde(default)]
    pub items: u64,
    /// The index's segments; 0 for the rest.
    #[serde(default)]
    pub parts: u32,
    /// The size it is held under, in bytes; 0 for none.
    #[serde(default)]
    pub limit: u64,
    /// Why [`crate::Request::Clear`] would refuse it now — `open` while the
    /// page's browser holds its profile, `kept` for settings — or empty.
    #[serde(default)]
    pub busy: String,
}

/// Bytes. Zero where the system does not say.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Memory {
    /// The service's resident memory now, and the most it has held.
    pub service: u64,
    pub peak: u64,
    /// What the service holds of its own, in memory or swapped out; and the
    /// files it has mapped in, the index's pages above all, which are the
    /// system's cache: it takes them back when it needs the memory. Both 0
    /// where the system does not say, and `service` is then all there is.
    #[serde(default)]
    pub own: u64,
    #[serde(default)]
    pub mapped: u64,
    /// The machine's memory, and how much of it is free to use.
    pub total: u64,
    pub available: u64,
}

/// Running totals; a face takes the difference of two answers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cpu {
    /// Processor time the service has used, all its threads together.
    pub service_ms: u64,
    /// The machine's processor time, busy and in all, in the system's ticks.
    pub busy: u64,
    pub total: u64,
    /// Hardware threads.
    pub threads: u32,
    /// The one-minute load average times a hundred; 0 where there is none.
    pub load_x100: u32,
}

/// Scour's programs of one kind running for this account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Running {
    /// `window`, `page`, `terminal`, `mcp` or `extractor`.
    pub kind: String,
    pub count: u32,
    /// Their resident memory together.
    pub bytes: u64,
    /// How long the oldest has been running.
    pub oldest_s: u64,
}
