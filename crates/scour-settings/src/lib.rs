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
//! window.
//!
//! Every field is `#[serde(default)]`, so a file written by an older version
//! loads and a field added later starts at its default rather than refusing
//! the whole file. A settings file that fails to parse is a person's
//! preferences silently reset.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

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
    /// A terminal has no pixels and ignores this; it must also leave it alone
    /// rather than clearing it, or opening the terminal once would cost the
    /// window its layout.
    #[serde(default)]
    pub widths: BTreeMap<String, u32>,
    /// What the list is ordered by, as the protocol's own name for it.
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
    /// Past queries, most recent first.
    ///
    /// **Only queries somebody meant.** A search box runs a query per
    /// keystroke, so recording what was searched would fill this with `r`,
    /// `ra`, `rap`, `rapo`. What goes in is what a person committed to —
    /// pressed Enter on, or opened a result from — which is a decision the
    /// frontend makes because only it knows what its keys mean.
    #[serde(default)]
    pub history: Vec<String>,
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
