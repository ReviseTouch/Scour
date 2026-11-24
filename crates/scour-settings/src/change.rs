//! A change names only what it touches, and is merged into what is kept.

use super::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// A change to the settings: the fields somebody set, and nothing else. Absent is not
/// "clear it" but "no opinion", which is what lets two frontends write at once.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Change {
    pub exclude_paths: Option<Vec<String>>,
    pub exclude_dirs: Option<Vec<String>>,
    pub exclude_files: Option<Vec<String>>,
    pub exclude_allow: Option<Vec<String>>,
    /// See [`Settings::exclude_off`]. Rule ids, replaced outright.
    pub exclude_off: Option<Vec<String>>,
    pub columns: Option<Vec<String>>,
    pub widths: Option<BTreeMap<String, u32>>,
    pub sort: Option<String>,
    pub descending: Option<bool>,
    /// Replace the tabs outright, capped at [`TABS`]. See [`Settings::tabs`].
    pub tabs: Option<Vec<SearchTab>>,
    pub tab: Option<usize>,
    pub grid_lines: Option<bool>,
    pub grid_columns: Option<bool>,
    pub system_icons: Option<bool>,
    pub tabs_off: Option<bool>,
    pub rail_shown: Option<bool>,
    pub ribbon_shown: Option<bool>,
    pub accents_exact: Option<bool>,
    /// `off`, `always`, or `""` to offer. See [`Settings::typos`].
    pub typos: Option<String>,
    pub typos_strict: Option<bool>,
    pub field_words: Option<bool>,
    pub dupes_open: Option<bool>,
    pub key_hint_seen: Option<bool>,
    /// A BCP-47 tag, or `""` to hand the decision back to the config file and the
    /// environment. `None` is "no opinion", as everywhere else here.
    pub language: Option<String>,
    /// `detail`, `icons`, `large` — or `""` to go back to no opinion at all.
    pub layout: Option<String>,
    /// `window`, `browser`, `tui` — or `""` to go back to no opinion.
    pub face: Option<String>,
    /// `light`, `dark`, or `""` to follow the desktop.
    pub theme: Option<String>,
    /// See [`Settings::removable`] and [`Settings::network`].
    pub removable: Option<String>,
    pub network: Option<String>,
    /// Replace the list outright. See [`Settings::folders`].
    pub folders: Option<Vec<String>>,
    /// Replace the list outright. See [`Settings::folders_off`].
    pub folders_off: Option<Vec<String>>,
    /// Replace the list outright. See [`Settings::folders_unwatched`].
    pub folders_unwatched: Option<Vec<String>>,
    /// See [`Settings::folders_every`].
    pub folders_every: Option<u32>,
    /// See [`Settings::refresh_every`].
    pub refresh_every: Option<u32>,
    /// `now`, `1m`, `5m`, or `""` to go by the machine. See [`Settings::memory`].
    pub memory: Option<String>,
    /// See [`Settings::narrow_off`].
    pub narrow_off: Option<bool>,
    /// See [`Settings::search_threads`].
    pub search_threads: Option<u32>,
    /// See [`Settings::fresh_within`].
    pub fresh_within: Option<u32>,
    /// See [`Settings::texts_off`].
    pub texts_off: Option<bool>,
    /// See [`Settings::texts_max_mb`].
    pub texts_max_mb: Option<u32>,
    /// See [`Settings::texts_dir`].
    pub texts_dir: Option<String>,
    /// Whether the preview panel stays open. See [`Settings::preview`].
    pub preview: Option<bool>,
    /// Replace the list outright. For clearing it, mostly.
    pub history: Option<Vec<String>>,
    /// Put one query at the front instead. The [`HISTORY`] cap is applied here, so no
    /// frontend has to know the number.
    pub remember: Option<String>,
    /// Frontend-local state, merged into [`Settings::view`] under its name, key by key,
    /// so saving "the side panel is open" says nothing about the window's size. `null`
    /// removes a key, per [RFC 7386](https://www.rfc-editor.org/rfc/rfc7386).
    pub view: BTreeMap<String, Value>,
}

impl Change {
    /// Whether this changes how the service spends memory and CPU, so it is
    /// tuned again. See [`Settings::release_after`].
    pub fn tunes(&self) -> bool {
        self.memory.is_some() || self.search_threads.is_some() || self.fresh_within.is_some()
    }

    /// Whether this changes how documents' text is kept.
    pub fn moves_texts(&self) -> bool {
        self.texts_off.is_some() || self.texts_max_mb.is_some() || self.texts_dir.is_some()
    }

    /// Whether this changes what is indexed — the disks asked about, the
    /// folders added — so the service plans its sources again.
    pub fn moves_sources(&self) -> bool {
        self.removable.is_some()
            || self.network.is_some()
            || self.folders.is_some()
            || self.folders_off.is_some()
            || self.folders_unwatched.is_some()
            || self.folders_every.is_some()
            || self.refresh_every.is_some()
    }

    /// Fold this into the settings.
    pub fn apply(self, to: &mut Settings) {
        if let Some(v) = self.exclude_paths {
            to.exclude_paths = v;
        }
        if let Some(v) = self.exclude_dirs {
            to.exclude_dirs = v;
        }
        if let Some(v) = self.exclude_files {
            to.exclude_files = v;
        }
        if let Some(v) = self.exclude_allow {
            to.exclude_allow = v;
        }
        if let Some(v) = self.exclude_off {
            to.exclude_off = v;
        }
        if let Some(v) = self.columns {
            to.columns = v;
        }
        if let Some(v) = self.widths {
            to.widths = v;
        }
        if let Some(v) = self.sort {
            to.sort = v;
        }
        if let Some(v) = self.descending {
            to.descending = Some(v);
        }
        if let Some(mut v) = self.tabs {
            v.truncate(TABS);
            to.tabs = v;
        }
        if let Some(v) = self.tab {
            to.tab = v;
        }
        if let Some(v) = self.grid_lines {
            to.grid_lines = v;
        }
        if let Some(v) = self.grid_columns {
            to.grid_columns = v;
        }
        if let Some(v) = self.system_icons {
            to.system_icons = v;
        }
        if let Some(v) = self.tabs_off {
            to.tabs_off = v;
        }
        if let Some(v) = self.rail_shown {
            to.rail_shown = v;
        }
        if let Some(v) = self.ribbon_shown {
            to.ribbon_shown = v;
        }
        if let Some(v) = self.accents_exact {
            to.accents_exact = v;
        }
        if let Some(v) = self.typos {
            to.typos = v;
        }
        if let Some(v) = self.typos_strict {
            to.typos_strict = v;
        }
        if let Some(v) = self.field_words {
            to.field_words = v;
        }
        if let Some(v) = self.key_hint_seen {
            to.key_hint_seen = v;
        }
        if let Some(v) = self.dupes_open {
            to.dupes_open = v;
        }
        if let Some(v) = self.language {
            to.language = v;
        }
        if let Some(v) = self.preview {
            to.preview = v;
        }
        if let Some(v) = self.face {
            to.face = v;
        }
        if let Some(v) = self.layout {
            to.layout = v;
        }
        if let Some(v) = self.theme {
            to.theme = v;
        }
        if let Some(v) = self.removable {
            to.removable = v;
        }
        if let Some(v) = self.network {
            to.network = v;
        }
        if let Some(v) = self.folders {
            to.folders = v;
        }
        if let Some(v) = self.folders_off {
            to.folders_off = v;
        }
        if let Some(v) = self.folders_every {
            to.folders_every = v;
        }
        if let Some(v) = self.refresh_every {
            to.refresh_every = v;
        }
        if let Some(v) = self.memory {
            to.memory = v;
        }
        if let Some(v) = self.narrow_off {
            to.narrow_off = v;
        }
        if let Some(v) = self.search_threads {
            to.search_threads = v;
        }
        if let Some(v) = self.fresh_within {
            to.fresh_within = v;
        }
        if let Some(v) = self.texts_off {
            to.texts_off = v;
        }
        if let Some(v) = self.texts_max_mb {
            to.texts_max_mb = v;
        }
        if let Some(v) = self.texts_dir {
            to.texts_dir = v;
        }
        if let Some(v) = self.folders_unwatched {
            to.folders_unwatched = v;
        }
        if let Some(v) = self.history {
            to.history = v;
            to.history.truncate(HISTORY);
        }
        if let Some(q) = self.remember {
            to.remember(&q);
        }
        for (who, what) in self.view {
            merge(to.view.entry(who).or_insert(Value::Null), what);
        }
    }

    /// Is there anything in here to do?
    pub fn is_empty(&self) -> bool {
        *self == Change::default()
    }
}

/// [RFC 7386](https://www.rfc-editor.org/rfc/rfc7386) merge-patch: objects merge key
/// by key, `null` removes, anything else replaces.
fn merge(into: &mut Value, patch: Value) {
    match patch {
        Value::Object(fields) => {
            if !into.is_object() {
                *into = Value::Object(serde_json::Map::new());
            }
            let slot = into.as_object_mut().expect("just made one");
            for (k, v) in fields {
                if v.is_null() {
                    slot.remove(&k);
                } else {
                    merge(slot.entry(k).or_insert(Value::Null), v);
                }
            }
        }
        other => *into = other,
    }
}
