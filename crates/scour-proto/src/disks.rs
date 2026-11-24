//! The disks a machine has, and what the service decided about each.

use serde::{Deserialize, Serialize};

/// A disk and what is being done about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Disk {
    pub volume: scour_volumes::Volume,
    pub state: DiskState,
    /// How it is kept up to date; empty from an older service.
    #[serde(default)]
    pub seen: Seen,
}

/// How a disk or a folder is kept up to date, as a settings page says it:
/// watched live or walked now and then, when it was last walked, and when a
/// change in it last reached the index.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seen {
    /// Its last whole walk in this run of the service.
    pub walked: Option<scour_core::Walked>,
    /// A watcher covers all of it: changes arrive as they happen.
    pub live: bool,
    /// When a change in it last reached the index, unix seconds.
    pub changed: Option<i64>,
    /// Being walked right now, or waiting to be.
    pub walking: bool,
}

/// One folder added from a face, as the settings page shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FolderState {
    pub path: String,
    /// Switched on: in the index.
    pub on: bool,
    /// Watched for changes, rather than only walked.
    pub watched: bool,
    /// Already inside a disk that is indexed, so not a source of its own.
    pub held: bool,
    /// Its last whole walk in this run of the service, if there was one.
    pub walked: Option<scour_core::Walked>,
    /// The rest of [`Seen`]; `walked` stays where older faces read it.
    #[serde(default)]
    pub live: bool,
    #[serde(default)]
    pub changed: Option<i64>,
    #[serde(default)]
    pub walking: bool,
}

/// Whether a disk is indexed, and who decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskState {
    /// Built in, and indexed without asking.
    Auto,
    /// Somebody said yes.
    Chosen,
    /// `config.toml` names it; it is changed there.
    Configured,
    /// Somebody took it out, or said no.
    Off,
    /// Plugged in or on a network, and nobody has said.
    Asking,
    /// Asked, and put off until the service starts again.
    Later,
    /// Built in, but `config.toml` lists the sources, so only if chosen.
    Manual,
}

impl DiskState {
    /// Its files are in the index.
    pub fn indexed(self) -> bool {
        matches!(
            self,
            DiskState::Auto | DiskState::Chosen | DiskState::Configured
        )
    }

    /// The name the wire uses.
    pub fn as_str(self) -> &'static str {
        match self {
            DiskState::Auto => "auto",
            DiskState::Chosen => "chosen",
            DiskState::Configured => "configured",
            DiskState::Off => "off",
            DiskState::Asking => "asking",
            DiskState::Later => "later",
            DiskState::Manual => "manual",
        }
    }
}
