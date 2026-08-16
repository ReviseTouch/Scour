//! What a frontend remembers between runs, kept where every frontend can
//! reach it.
//!
//! ## Why this is not the browser's job
//!
//! It was, and the browser lost it. The window kept its columns, their widths
//! and the last query in `localStorage`, which Chromium buffers and writes on
//! a **clean** shutdown — measured both ways: closed properly, the settings
//! come back; killed, they are gone. A launcher whose own comment reads "a
//! browser that is killed does not tell its launcher" is not a place to keep
//! anything.
//!
//! That is the smaller reason. The larger one is that `localStorage` belongs
//! to one origin in one browser, and this program is meant to have three
//! frontends — a window, a terminal interface, and a Slint one. A person who
//! turns off a column has said something about *Scour*, not about Chromium,
//! and the terminal should already know it the next time they open it.
//!
//! So the service holds it. The service is the only thing all three talk to,
//! it is running anyway, and it writes to disk when the change happens rather
//! than hoping to be shut down politely.
//!
//! ## Typed, and shared on purpose
//!
//! A free-form key-value store would have been less code and would have let
//! each frontend invent its own names — which is exactly the outcome to avoid,
//! because then the window and the terminal keep two unrelated column lists
//! and neither is wrong. One named field per thing a person can decide, and a
//! frontend ignores the fields it has no use for: `widths` means nothing in a
//! terminal, and a terminal leaving it alone is how it stays right for the
//! window. *Leaving it alone* is the part that needed the section below.
//!
//! Every field is `#[serde(default)]`, so a file written by an older version
//! loads and a field added later starts at its default rather than refusing
//! the whole file. A settings file that fails to parse is a person's
//! preferences silently reset.
//!
//! ## Written as changes, not as the whole object
//!
//! **"A frontend ignores the fields it has no use for" was not true.** It
//! could not be: writing meant sending the whole object back, so a terminal
//! that had never heard of `widths` would send the object it knew — without
//! them — and the window's layout would be gone. The protocol said as much in
//! its own words: *the whole object, because a frontend that sent one field
//! would have to know what the others currently are anyway*. It does not, if
//! it does not send them.
//!
//! So a write is a [`Change`]: the fields somebody set, and nothing else. Two
//! frontends can be open at once, each writing what it understands, and
//! neither erases the other. It is also what lets a field be added here
//! without every frontend learning about it first.
//!
//! ## Shared, and frontend-local
//!
//! One named field per thing a *person* decides — which columns, what order,
//! what they searched for. Those are about Scour and every frontend means the
//! same thing by them.
//!
//! [`Settings::view`] is for what only one kind of window can mean: whether a
//! side panel is open, how big a window was left, whether thumbnails are
//! drawn. Free-form on purpose, and namespaced by frontend, because the reason
//! to keep the shared fields typed — that two frontends must not invent two
//! names for one idea — does not apply to a thing no other frontend reads.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How many past queries are kept.
///
/// Enough that yesterday's search is still there, short enough that the list
/// stays something a person can look down. The cap is applied on write, so the
/// file cannot grow without bound however long the service runs.
pub const HISTORY: usize = 100;

/// Everything a frontend remembers.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Settings {
    /// Columns to show, in the order they are shown, by id.
    ///
    /// Empty means "whatever this frontend calls its default" — not "no
    /// columns". A frontend that stored an empty list would otherwise show a
    /// blank table, and the difference between "not chosen yet" and "chosen to
    /// be nothing" is not worth a second field.
    #[serde(default)]
    pub columns: Vec<String>,
    /// Width per column id, in pixels.
    ///
    /// **By column id, the same names `columns` uses** — not by whatever a
    /// frontend happens to call the thing it sorts by. The window keyed these
    /// by sort key for a while, which agreed with the id for seven columns and
    /// differed for five (`ctime`/`created`, `atime`/`accessed`,
    /// `perm`/`mode`, `user`/`uid`, `group`/`gid`), so half a layout survived
    /// a restart and half did not.
    ///
    /// A terminal has no pixels and ignores this; it must also leave it alone
    /// rather than clearing it, or opening the terminal once would cost the
    /// window its layout. That is now something a terminal gets for free — it
    /// writes a [`Change`], and what it does not name it does not touch.
    #[serde(default)]
    pub widths: BTreeMap<String, u32>,
    /// What the list is ordered by, as **the protocol's own name** for it —
    /// `modified`, not whatever a frontend calls that column.
    ///
    /// One vocabulary, and it has to be the protocol's, because this file is
    /// read by frontends that do not share a column list. The window wrote
    /// `mtime` here and validated what it read against its own columns, so a
    /// file written by the Slint window — which says `modified`, the same word
    /// the CLI and the MCP server use — was silently rejected and the sort
    /// fell back to the default.
    #[serde(default)]
    pub sort: String,
    #[serde(default)]
    pub descending: Option<bool>,
    /// Whether the duplicates panel in the report is open.
    ///
    /// **Closed by default, and the default is the point.** It went in as
    /// always-on, which meant opening the report to look at what a folder
    /// weighs also ran a duplicate hunt over thirty thousand candidates —
    /// work nobody asked for, in a tab opened for something else. It is one
    /// click away and it stays where it was put.
    #[serde(default)]
    pub dupes_open: bool,
    /// What language the interface speaks, as a BCP-47 tag. Empty means
    /// "nobody has chosen", which is not the same as English.
    ///
    /// **A typed field rather than a corner of [`Settings::view`]**, because it
    /// is the definition of a shared field: a person who switches the window to
    /// English has said something about *Scour*, and a terminal opened
    /// afterwards should already be in English. Put in `view.web` it would have
    /// been one window's private opinion, and the second frontend would have
    /// grown its own.
    ///
    /// Empty is load-bearing and is why this is not defaulted to `"en"` or to
    /// `"tr"`. A tag written here is an explicit choice and outranks everything;
    /// with nothing here the frontend falls back to `config.toml`'s `ui.language`
    /// and then to the environment's `LANG`, which is the POSIX answer and the
    /// one the CLI and the Slint window already give. Writing a default in
    /// would make every machine's first run claim a decision nobody made — and
    /// on this machine, where `LANG=tr_TR.UTF-8`, it would be the wrong one half
    /// the time. `scour_i18n::choose` is the single place that order lives.
    ///
    /// Not validated here. An unknown tag degrades to English one string at a
    /// time in `scour-i18n`, and a settings file that refuses to load because
    /// somebody typed `tr-` is worse than a window in the wrong language.
    #[serde(default)]
    pub language: String,
    /// What shape the result list is drawn in: `detail` a table of rows,
    /// `icons` a grid of small tiles, `large` a grid of big ones.
    ///
    /// **Shared rather than [`Settings::view`], and the boundary is worth
    /// stating** — whether thumbnails are drawn lives in `view.web` and this
    /// does not. The difference is who can mean it: whether a *picture* goes
    /// in a row is something only a window with pictures decides, while "show
    /// me these as tiles rather than as lines" is an answer any frontend with
    /// a screen gives, and the Slint window will be asked the same question
    /// the day it grows a second shape. Two frontends inventing two names for
    /// that is what the typed fields exist to stop.
    ///
    /// Empty means "nobody has chosen", which every frontend reads as its own
    /// default. Not validated here, for the reason [`Settings::language`] is
    /// not: an unknown word degrades to that default, and a settings file that
    /// refuses to load because one frontend wrote a shape another has not
    /// heard of is a person's preferences gone.
    #[serde(default)]
    pub layout: String,
    /// Past queries, most recent first.
    ///
    /// **Only queries somebody meant.** A search box runs a query per
    /// keystroke, so recording what was searched would fill this with `r`,
    /// `ra`, `rap`, `rapo`. What goes in is what a person committed to —
    /// pressed Enter on, or opened a result from — which is a decision the
    /// frontend makes because only it knows what its keys mean.
    #[serde(default)]
    pub history: Vec<String>,
    /// What one kind of window remembers and no other can read.
    ///
    /// Keyed by frontend — `web`, `slint`, `tui` — and free-form inside.
    /// Whether a side panel is open, how large a window was left, whether
    /// thumbnails are drawn: real preferences, but ones a terminal cannot act
    /// on and must not be given a typed field for, because a typed field is a
    /// promise that every frontend means the same thing by it.
    ///
    /// **Nothing here is read across the boundary.** A frontend that wants
    /// another's setting is asking for a shared field, and should be given
    /// one above.
    #[serde(default)]
    pub view: BTreeMap<String, Value>,
}

/// A change to the settings: the fields somebody set, and nothing else.
///
/// Absent is not "clear it" — absent is "I have no opinion about this", which
/// is what makes it safe for two frontends to write at once. See the note at
/// the top of this file for what the whole-object write cost.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Change {
    pub columns: Option<Vec<String>>,
    pub widths: Option<BTreeMap<String, u32>>,
    pub sort: Option<String>,
    pub descending: Option<bool>,
    pub dupes_open: Option<bool>,
    /// A BCP-47 tag, or `""` to hand the decision back to the config file and
    /// the environment.
    ///
    /// `None` is "I have no opinion", as everywhere else here — which is what
    /// lets the window change the language while a terminal is writing its
    /// sort order, and neither undoes the other.
    pub language: Option<String>,
    /// `detail`, `icons`, `large` — or `""` to go back to no opinion at all.
    pub layout: Option<String>,
    /// Replace the list outright. For clearing it, mostly.
    pub history: Option<Vec<String>>,
    /// Put one query at the front instead.
    ///
    /// **The cap belongs here rather than in the frontend**, and it was in the
    /// frontend: [`HISTORY`] existed and nothing in the service ever applied
    /// it, because the only writer sent a list it had already trimmed itself.
    /// Every frontend would have had to know the number, and one that did not
    /// would grow the file without bound.
    pub remember: Option<String>,
    /// Frontend-local state, merged into [`Settings::view`] under its name.
    ///
    /// Merged rather than replaced, key by key, so a window that saves "the
    /// side panel is open" does not also say anything about its own size.
    /// `null` removes a key — [RFC 7386]'s rule, because it is the one
    /// convention people already know for this.
    ///
    /// [RFC 7386]: https://www.rfc-editor.org/rfc/rfc7386
    pub view: BTreeMap<String, Value>,
}

impl Change {
    /// Fold this into the settings.
    pub fn apply(self, to: &mut Settings) {
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

/// [RFC 7386] merge-patch: objects merge key by key, `null` removes, anything
/// else replaces.
///
/// [RFC 7386]: https://www.rfc-editor.org/rfc/rfc7386
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
    /// Put a query at the front, once.
    ///
    /// Deduplicated by exact text: searching the same thing twice should move
    /// it up rather than appear twice, which is what every launcher does and
    /// what makes the list shorter the more it is used.
    pub fn remember(&mut self, query: &str) {
        let query = query.trim();
        if query.is_empty() {
            return;
        }
        self.history.retain(|q| q != query);
        self.history.insert(0, query.to_owned());
        self.history.truncate(HISTORY);
    }

    /// Where the file lives, given the directory the caller keeps state in.
    pub fn path_in(dir: &Path) -> PathBuf {
        dir.join("settings.json")
    }

    /// Read them, or start from nothing.
    ///
    /// **A missing or unreadable file is not an error**, and neither is one
    /// that does not parse: the worst outcome here is refusing to start
    /// because somebody's column list has a stray comma in it. Defaults are
    /// always a usable answer, which is not true of most files a program
    /// reads.
    pub fn load(dir: &Path) -> Settings {
        std::fs::read_to_string(Self::path_in(dir))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Write them, atomically.
    ///
    /// Written beside the target and renamed over it, because the alternative
    /// is a truncated file where the settings were: a rename is atomic on the
    /// same filesystem and a half-written `settings.json` is a person's
    /// preferences gone. The same rule the index uses for its manifest.
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

    /// **The shape of the list is a shared field, and behaves like one.**
    ///
    /// It survives a write from a frontend that has never heard of it, which
    /// is the whole reason it is a [`Change`] field rather than something a
    /// window sends as part of a whole object. The empty string is kept as a
    /// distinct answer — "nobody has chosen" is not "detail", and a frontend
    /// whose default is something else has to be able to tell the two apart.
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

    /// **The thing the whole-object write could not do.**
    ///
    /// A terminal that has never heard of column widths saves what it does
    /// know, and the window's layout is still there afterwards. Before this,
    /// writing meant sending the whole object, so whatever the writer did not
    /// carry was erased by the writing.
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

    /// **A language chosen in one window survives another window saving.**
    ///
    /// The same property as the test above, asserted separately because the
    /// language is the field most likely to be written by a frontend that knows
    /// nothing else: a terminal interface has no columns and no widths, and if
    /// its save carried the whole object the window's language would go back to
    /// unset every time somebody ran `scour settings`.
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
