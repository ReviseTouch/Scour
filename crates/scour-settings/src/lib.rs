//! What a frontend remembers between runs, kept in the service — the only thing all
//! the frontends talk to. One typed field per thing a *person* decides, so two of
//! them cannot invent two names for one idea; [`Settings::view`] is free-form and
//! namespaced for what only one window can mean. Every field is `#[serde(default)]`,
//! and a write is a [`Change`]: absent means "no opinion", never "clear it".

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How a rule is named in [`Settings::exclude_off`]: `dir:target`, `path:/proc`. One
/// function, so a switch written by one frontend and read by another matches exactly.
pub fn rule_id(kind: &str, value: &str) -> String {
    format!("{kind}:{value}")
}

/// How many past queries are kept. Applied on write, so the file cannot grow without
/// bound however long the service runs.
pub const HISTORY: usize = 100;

/// Everything a frontend remembers.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Settings {
    /// Directory names, file names and path prefixes a person added from a window, and
    /// the prefixes that take precedence over them. Three sources reach the walk — the
    /// built-in set, this file and `config.toml` — and only this one is editable.
    #[serde(default)]
    pub exclude_paths: Vec<String>,
    #[serde(default)]
    pub exclude_dirs: Vec<String>,
    #[serde(default)]
    pub exclude_files: Vec<String>,
    #[serde(default)]
    pub exclude_allow: Vec<String>,
    /// Rules switched off, by id — `dir:target`, `path:/proc`. Off rather than deleted,
    /// which is the only way a built-in or `config.toml` rule can be reached; ids, since
    /// one value in two lists is two rules. Never [`scour_core::ScanOptions::deny`].
    #[serde(default)]
    pub exclude_off: Vec<String>,
    /// Columns to show, in the order they are shown, by id. Empty means "whatever this
    /// frontend calls its default", not "no columns".
    #[serde(default)]
    pub columns: Vec<String>,
    /// Width per column id, in pixels — **the same names `columns` uses**, not whatever
    /// a frontend sorts by. A frontend with no pixels ignores it and must leave it
    /// alone rather than clear it, which writing a [`Change`] gives for free.
    #[serde(default)]
    pub widths: BTreeMap<String, u32>,
    /// What the list is ordered by, as **the protocol's own name** — `modified`, not
    /// whatever a frontend calls that column. One vocabulary, because this file is read
    /// by frontends that do not share a column list.
    #[serde(default)]
    pub sort: String,
    #[serde(default)]
    pub descending: Option<bool>,
    /// Whether the duplicates panel in the report is open. Closed by default: opening
    /// the report must not also hunt duplicates over thirty thousand candidates.
    #[serde(default)]
    pub dupes_open: bool,
    /// What language the interface speaks, as a BCP-47 tag. Empty is "nobody has
    /// chosen", not English: with nothing here a frontend falls back to `config.toml`'s
    /// `ui.language` and then `LANG`, an order `scour_i18n::choose` owns. Not validated.
    #[serde(default)]
    pub language: String,
    /// What shape the result list is drawn in: `detail` a table of rows, `icons` a grid
    /// of small tiles, `large` a grid of big ones. Shared, because any frontend with a
    /// screen can mean it. Empty is "nobody has chosen". Not validated here.
    #[serde(default)]
    pub layout: String,
    /// Which face opens when somebody asks for Scour without saying which: `window`,
    /// `browser` or `tui`, written by whichever face was switched *to*. Empty is "nobody
    /// has chosen", which the launcher reads as the window. Not validated here.
    #[serde(default)]
    pub face: String,

    /// Whether the preview panel is open — a mode, not a gesture: the key opens it and
    /// the button pins it. `false` is what a face with no preview panel writes.
    #[serde(default)]
    pub preview: bool,
    /// Past queries, most recent first — only ones somebody meant. A search box runs a
    /// query per keystroke, so the frontend decides what counts as committed.
    #[serde(default)]
    pub history: Vec<String>,
    /// What one kind of window remembers and no other can read. Keyed by frontend —
    /// `web`, `slint`, `tui` — and free-form inside. Nothing here is read across the
    /// boundary: a frontend that wants another's setting needs a shared field above.
    #[serde(default)]
    pub view: BTreeMap<String, Value>,
}

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
    pub dupes_open: Option<bool>,
    /// A BCP-47 tag, or `""` to hand the decision back to the config file and the
    /// environment. `None` is "no opinion", as everywhere else here.
    pub language: Option<String>,
    /// `detail`, `icons`, `large` — or `""` to go back to no opinion at all.
    pub layout: Option<String>,
    /// `window`, `browser`, `tui` — or `""` to go back to no opinion.
    pub face: Option<String>,
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

impl Settings {
    /// Put a query at the front, once. Deduplicated by exact text, so searching the
    /// same thing twice moves it up rather than appearing twice.
    pub fn remember(&mut self, query: &str) {
        let query = query.trim();
        if query.is_empty() {
            return;
        }
        self.history.retain(|q| q != query);
        self.history.insert(0, query.to_owned());
        self.history.truncate(HISTORY);
    }

    /// Has this rule been switched off? `kind` is `path`, `dir`, `file` or `allow` and
    /// `value` is the rule; together they make the id in [`Settings::exclude_off`].
    /// Compared without case, like the merge that builds the rule lists.
    pub fn rule_off(&self, kind: &str, value: &str) -> bool {
        let id = rule_id(kind, value);
        self.exclude_off.iter().any(|o| o.eq_ignore_ascii_case(&id))
    }

    /// Where the file lives, given the directory the caller keeps state in.
    pub fn path_in(dir: &Path) -> PathBuf {
        dir.join("settings.json")
    }

    /// Read them, or start from nothing. A missing, unreadable or unparsable file is
    /// not an error: refusing to start over a stray comma is the worse outcome.
    pub fn load(dir: &Path) -> Settings {
        std::fs::read_to_string(Self::path_in(dir))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Write them, atomically: written beside the target and renamed over it, because
    /// a half-written `settings.json` is a person's preferences gone.
    pub fn save(&self, dir: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let target = Self::path_in(dir);
        let tmp = target.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&tmp, &target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(name: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("scour-settings-{name}"));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn what_was_saved_comes_back() {
        let d = dir("roundtrip");
        let mut s = Settings {
            columns: vec!["name".into(), "atime".into()],
            sort: "accessed".into(),
            descending: Some(false),
            ..Settings::default()
        };
        s.widths.insert("name".into(), 240);
        s.remember("rapor ext:pdf");
        s.save(&d).expect("save");
        assert_eq!(Settings::load(&d), s);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// **Defaults rather than a refusal.** The worst thing this could do is
    /// stop the service because somebody's file has a stray comma in it.
    #[test]
    fn a_file_that_makes_no_sense_is_not_a_failure() {
        let d = dir("broken");
        std::fs::create_dir_all(&d).expect("mkdir");
        std::fs::write(Settings::path_in(&d), b"{ this is not json").expect("write");
        assert_eq!(Settings::load(&d), Settings::default());
        // And a directory nobody has written yet.
        assert_eq!(
            Settings::load(Path::new("/nonexistent")),
            Settings::default()
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A file from an older version keeps what it has and gains the rest.
    #[test]
    fn a_field_added_later_does_not_reject_the_file() {
        let d = dir("older");
        std::fs::create_dir_all(&d).expect("mkdir");
        std::fs::write(Settings::path_in(&d), br#"{"columns":["name","size"]}"#).expect("write");
        let s = Settings::load(&d);
        assert_eq!(s.columns, ["name", "size"]);
        assert!(s.history.is_empty());
        assert_eq!(s.descending, None);
        let _ = std::fs::remove_dir_all(&d);
    }

    /// Which face opens is a preference, not a launcher's guess. The empty string stays
    /// a distinct answer, which is what makes the window the default.
    #[test]
    fn the_face_somebody_switched_to_is_the_one_that_opens() {
        let mut s = Settings::default();
        assert_eq!(s.face, "", "nobody has chosen, to start with");

        Change {
            face: Some("tui".into()),
            ..Change::default()
        }
        .apply(&mut s);
        assert_eq!(s.face, "tui");

        // A frontend saying something else says nothing about this.
        Change {
            language: Some("tr".into()),
            ..Change::default()
        }
        .apply(&mut s);
        assert_eq!(s.face, "tui", "a language change is not a face change");

        Change {
            face: Some(String::new()),
            ..Change::default()
        }
        .apply(&mut s);
        assert_eq!(s.face, "", "and it can be handed back");
    }

    /// The shape of the list survives a write from a frontend that has never heard of
    /// it, and `""` stays distinct from `detail`.
    #[test]
    fn the_list_shape_outlives_a_frontend_that_does_not_know_it() {
        let mut s = Settings::default();
        assert_eq!(s.layout, "", "nobody has chosen, to start with");

        Change {
            layout: Some("large".into()),
            ..Change::default()
        }
        .apply(&mut s);
        assert_eq!(s.layout, "large");

        // A terminal saving its sort order says nothing about the shape.
        Change {
            sort: Some("size".into()),
            ..Change::default()
        }
        .apply(&mut s);
        assert_eq!(s.layout, "large");

        // And it can be handed back.
        Change {
            layout: Some(String::new()),
            ..Change::default()
        }
        .apply(&mut s);
        assert_eq!(s.layout, "");
    }

    #[test]
    fn the_same_search_twice_moves_up_rather_than_repeating() {
        let mut s = Settings::default();
        s.remember("bir");
        s.remember("iki");
        s.remember("bir");
        assert_eq!(s.history, ["bir", "iki"]);
    }

    #[test]
    fn nothing_is_not_a_search() {
        let mut s = Settings::default();
        s.remember("");
        s.remember("   ");
        assert!(s.history.is_empty());
    }

    /// A terminal that has never heard of column widths saves what it does know, and
    /// the window's layout is still there afterwards.
    #[test]
    fn one_frontend_saving_does_not_erase_another() {
        let mut s = Settings::default();
        Change {
            columns: Some(vec!["name".into(), "size".into()]),
            widths: Some(BTreeMap::from([("name".to_owned(), 240)])),
            view: BTreeMap::from([("web".to_owned(), serde_json::json!({ "icons": true }))]),
            ..Change::default()
        }
        .apply(&mut s);

        // The terminal knows about sort and history and nothing else.
        Change {
            sort: Some("size".into()),
            remember: Some("rapor".into()),
            ..Change::default()
        }
        .apply(&mut s);

        assert_eq!(s.columns, ["name", "size"], "columns survived");
        assert_eq!(s.widths.get("name"), Some(&240), "widths survived");
        assert_eq!(s.view["web"]["icons"], serde_json::json!(true));
        assert_eq!(s.sort, "size");
        assert_eq!(s.history, ["rapor"]);
    }

    /// A language chosen in one window survives another window saving — the field most
    /// likely to be written by a frontend that knows nothing else.
    #[test]
    fn a_language_survives_another_frontend_saving() {
        let mut s = Settings::default();
        Change {
            language: Some("en".into()),
            ..Change::default()
        }
        .apply(&mut s);
        assert_eq!(s.language, "en");

        // Somebody else, writing a field that has nothing to do with language.
        Change {
            sort: Some("size".into()),
            remember: Some("rapor".into()),
            ..Change::default()
        }
        .apply(&mut s);
        assert_eq!(s.language, "en", "still English");

        // And back to "nobody has chosen", which is a choice a menu can offer
        // and is not the same as choosing English.
        Change {
            language: Some(String::new()),
            ..Change::default()
        }
        .apply(&mut s);
        assert!(s.language.is_empty());
    }

    /// A settings file written before this field existed still loads, and the
    /// language it does not mention is unset rather than wrong.
    #[test]
    fn a_file_from_before_the_language_existed_is_not_in_english() {
        let s: Settings =
            serde_json::from_str(r#"{"columns":["name"],"sort":"modified"}"#).expect("parse");
        assert!(
            s.language.is_empty(),
            "unset, so the environment still decides"
        );
    }

    /// Frontend-local state merges key by key, and `null` takes one away.
    #[test]
    fn a_window_setting_one_thing_says_nothing_about_the_rest() {
        let mut s = Settings::default();
        Change {
            view: BTreeMap::from([(
                "web".to_owned(),
                serde_json::json!({ "icons": true, "side": false }),
            )]),
            ..Change::default()
        }
        .apply(&mut s);
        Change {
            view: BTreeMap::from([("web".to_owned(), serde_json::json!({ "side": true }))]),
            ..Change::default()
        }
        .apply(&mut s);
        assert_eq!(s.view["web"]["icons"], serde_json::json!(true), "untouched");
        assert_eq!(s.view["web"]["side"], serde_json::json!(true), "changed");

        Change {
            view: BTreeMap::from([("web".to_owned(), serde_json::json!({ "icons": null }))]),
            ..Change::default()
        }
        .apply(&mut s);
        assert!(
            !s.view["web"]
                .as_object()
                .expect("object")
                .contains_key("icons")
        );
        assert_eq!(
            s.view["web"]["side"],
            serde_json::json!(true),
            "still there"
        );

        // And one frontend's corner is not another's.
        Change {
            view: BTreeMap::from([("tui".to_owned(), serde_json::json!({ "side": false }))]),
            ..Change::default()
        }
        .apply(&mut s);
        assert_eq!(s.view["web"]["side"], serde_json::json!(true));
        assert_eq!(s.view["tui"]["side"], serde_json::json!(false));
    }

    /// An empty change is a change to nothing, not a reset.
    #[test]
    fn saying_nothing_changes_nothing() {
        let mut s = Settings {
            columns: vec!["name".into()],
            sort: "modified".into(),
            history: vec!["bir".into()],
            ..Settings::default()
        };
        let before = s.clone();
        assert!(Change::default().is_empty());
        Change::default().apply(&mut s);
        assert_eq!(s, before);
    }

    /// The cap is the service's job now, whatever a frontend sends.
    #[test]
    fn the_service_caps_the_history_whoever_writes_it() {
        let mut s = Settings::default();
        Change {
            history: Some((0..HISTORY * 3).map(|i| format!("q{i}")).collect()),
            ..Change::default()
        }
        .apply(&mut s);
        assert_eq!(s.history.len(), HISTORY);

        let mut s = Settings::default();
        for i in 0..HISTORY * 2 {
            Change {
                remember: Some(format!("q{i}")),
                ..Change::default()
            }
            .apply(&mut s);
        }
        assert_eq!(s.history.len(), HISTORY);
    }

    #[test]
    fn the_list_stops_growing() {
        let mut s = Settings::default();
        for i in 0..HISTORY * 2 {
            s.remember(&format!("q{i}"));
        }
        assert_eq!(s.history.len(), HISTORY);
        assert_eq!(
            s.history[0],
            format!("q{}", HISTORY * 2 - 1),
            "newest first"
        );
    }
}
