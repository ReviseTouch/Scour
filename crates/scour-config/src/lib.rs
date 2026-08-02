//! Settings.
//!
//! TOML, because the file is meant to be opened and edited by hand — the
//! exclusion lists in particular, which are the first place someone looks when
//! a file they expected is missing from the index.
//!
//! Every table is `#[serde(default)]`, so an older file loads with new fields
//! filled in and nothing has to migrate. Unknown keys are rejected rather than
//! ignored: a typo in a setting that silently does nothing is worse than one
//! that says so, because the first kind is discovered months later.

mod paths;
mod schema;

pub use paths::{config_path, data_dir, default_index_dir, socket_path};
pub use schema::{Config, ContentCfg, ExcludeCfg, IndexCfg, ScanCfg, ServiceCfg, SourceCfg, UiCfg};
