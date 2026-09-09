//! Settings. TOML, because the file is meant to be edited by hand — the exclusion
//! lists in particular. Every table is `#[serde(default)]`, so an older file loads
//! with new fields filled in and nothing has to migrate; unknown keys are rejected
//! rather than ignored, since a typo that silently does nothing is found months on.

mod paths;
mod schema;

pub use paths::{config_path, data_dir, default_index_dir, socket_path};
pub use schema::{Config, ExcludeCfg, IndexCfg, ScanCfg, ServiceCfg, SourceCfg, UiCfg};
