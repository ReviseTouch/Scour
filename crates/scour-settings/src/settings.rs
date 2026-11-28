//! What a person decided, one typed field each.

use super::*;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// How a rule is named in [`Settings::exclude_off`]: `dir:target`, `path:/proc`. One
/// function, so a switch written by one frontend and read by another matches exactly.
pub fn rule_id(kind: &str, value: &str) -> String {
    format!("{kind}:{value}")
}

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
    /// Disks indexed because somebody said yes — plugged-in and network ones,
    /// which are asked about first — by `scour_volumes::Volume::id`.
    #[serde(default)]
    pub volumes_on: Vec<String>,
    /// Disks somebody took out, or said no to, by the same id. Out wins over on.
    #[serde(default)]
    pub volumes_off: Vec<String>,
    /// What happens when a disk is plugged in that nobody has answered for:
    /// `""` asks, `always` indexes it, `never` leaves it out without asking.
    /// An answer already given for that disk wins over this.
    #[serde(default)]
    pub removable: String,
    /// The same for a network disk — SMB, NFS, SSHFS. Not validated here.
    #[serde(default)]
    pub network: String,
    /// The same for a CD, DVD or Blu-ray, but **`""` leaves it out**: `ask`
    /// asks, `always` indexes it. A disc is read slowly and mostly holds what
    /// is somewhere else too.
    #[serde(default)]
    pub optical: String,
    /// Folders indexed on their own, added from a face: a disk is the usual
    /// unit, and this is the one for "only this folder of that disk". Absolute
    /// paths. One already inside an indexed disk adds nothing.
    #[serde(default)]
    pub folders: Vec<String>,
    /// Folders of [`Settings::folders`] switched off: kept in the list, out of
    /// the index until switched on again.
    #[serde(default)]
    pub folders_off: Vec<String>,
    /// Folders of [`Settings::folders`] not watched: walked at the start and
    /// when asked, and otherwise left alone.
    #[serde(default)]
    pub folders_unwatched: Vec<String>,
    /// Minutes between walks of a folder nothing reports changes for — a share
    /// whose server sends none, one only walked. Zero is the default, ten.
    #[serde(default)]
    pub folders_every: u32,
    /// Seconds a face in use leaves between re-reading the rows in sight after
    /// the index moved. Zero is the default, two: every second redrew a list
    /// that moves under a busy machine, 13 to 21% of a core for the window.
    #[serde(default)]
    pub refresh_every: u32,
    /// How soon the service gives back what a query brought into memory — the
    /// index's pages and the folder table — once nobody asks: `now`, a few
    /// seconds after the last question, `1m`, `5m`, or `""` to go by the
    /// machine: under [`SMALL_MACHINE`] `now`, `1m` above. Not validated here.
    #[serde(default)]
    pub memory: String,
    /// The narrowing line under the query bar, switched off: its two walks are
    /// then never asked.
    #[serde(default)]
    pub narrow_off: bool,
    /// Threads one large search is split over. Zero is the default, the cores
    /// up to eight; one keeps a search off every core but one.
    #[serde(default)]
    pub search_threads: u32,
    /// Seconds the index may stay stale while nothing is watching it. Zero
    /// keeps `config.toml`'s `commit_idle_ms`.
    #[serde(default)]
    pub fresh_within: u32,
    /// Documents' text — PDF, legacy Office, e-mail — not kept: every
    /// search reads them again.
    #[serde(default)]
    pub texts_off: bool,
    /// Megabytes of compressed text kept at most; zero is the service's default.
    #[serde(default)]
    pub texts_max_mb: u32,
    /// Where it is kept: a `scour-texts` folder inside this one; empty is
    /// beside the index's settings.
    #[serde(default)]
    pub texts_dir: String,
    /// Read documents' text ahead of any search, while nobody uses the
    /// machine: slowly, at the lowest priority.
    #[serde(default)]
    pub texts_ahead: bool,
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
    /// The search tabs, in order. Empty is one tab — the query line as it stands,
    /// which is what every face had before there were tabs.
    #[serde(default)]
    pub tabs: Vec<SearchTab>,
    /// Which of [`Settings::tabs`] is in front. Out of range reads as the last.
    #[serde(default)]
    pub tab: usize,
    /// A rule under every row of the list. Off by default: the list is read by
    /// its alignment, and lines are for somebody who wants a grid. The name is
    /// from when one switch drew both kinds of line.
    #[serde(default)]
    pub grid_lines: bool,
    /// A rule between each pair of the list's columns. Off by default.
    #[serde(default)]
    pub grid_columns: bool,
    /// Native desktop file and folder icons instead of built-in kind glyphs.
    #[serde(default)]
    pub system_icons: bool,
    /// Search tabs switched off: no strip, no keys, one query line. The tabs
    /// stay in [`Settings::tabs`] and come back with the switch.
    #[serde(default)]
    pub tabs_off: bool,
    /// The kinds-and-places rail beside the list, brought back. Away unless
    /// somebody asks for it: the query bar's lens and its narrowing strip do
    /// its work. A new name, so an old `rail_hidden: false` does not bring it.
    #[serde(default)]
    pub rail_shown: bool,
    /// The time ribbon under the list, brought back; away by default.
    #[serde(default)]
    pub ribbon_shown: bool,
    /// A letter and the same letter with a mark are different letters to a
    /// search: `sirket` does not find `şirket`. Off by default, where only a
    /// word typed with a mark is that strict.
    #[serde(default)]
    pub accents_exact: bool,
    /// What a search does about a word that may be mistyped: `""` says how many
    /// files it nearly found when it finds none, `always` forgives every word
    /// its mistakes, `off` does neither. Not validated here.
    #[serde(default)]
    pub typos: String,
    /// How a Word, Excel or PowerPoint file is previewed: `""` drawn as it
    /// looks (its paragraphs, sheets or slides), `text` its plain text. Changed
    /// beside the preview itself and in the settings.
    #[serde(default)]
    pub office_view: String,
    /// A misspelling is forgiven only past a word's first four letters, so a
    /// longer word never finds more than a shorter one. Off by default: a
    /// mistake is forgiven anywhere in a match that begins at a word.
    #[serde(default)]
    pub typos_strict: bool,
    /// A filter's header in the query bar written out (`dm:`) rather than drawn
    /// as a picture (a clock). Pictures by default.
    #[serde(default)]
    pub field_words: bool,
    /// Whether the duplicates panel in the report is open. Closed by default: opening
    /// the report must not also hunt duplicates over thirty thousand candidates.
    #[serde(default)]
    pub dupes_open: bool,
    /// The window's first-run line about binding a key has been answered, one way
    /// or the other; it is not shown again. Shared, so one face's answer counts.
    #[serde(default)]
    pub key_hint_seen: bool,
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
    /// What a face opens on: `""` the tabs as they were left, the one in front
    /// again; `new_tab` the same tabs with an empty one in front; `fresh` a new
    /// screen of one empty tab, the others let go. See [`Settings::opened`].
    #[serde(default)]
    pub start: String,
    /// `light` or `dark`, or `""` to follow the desktop — the default.
    #[serde(default)]
    pub theme: String,

    /// Whether the preview panel is open — a mode, not a gesture: the key opens it and
    /// the button pins it. `false` is what a face with no preview panel writes.
    #[serde(default)]
    pub preview: bool,
    /// Whether the preview's facts are all shown, or the one line of them. Kept
    /// the way it was last left, in every face.
    #[serde(default)]
    pub preview_details: bool,
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

/// Below this much memory a machine gives back what a query used at once, if
/// nobody chose otherwise: 8 GiB.
pub const SMALL_MACHINE: u64 = 8 << 30;

impl Settings {
    /// How long after the last question the service keeps what it read, on a
    /// machine with `memory` bytes. See [`Settings::memory`].
    pub fn release_after(&self, memory: u64) -> std::time::Duration {
        let seconds = match self.memory.as_str() {
            "now" => 3,
            "5m" => 300,
            "1m" => 60,
            _ if memory > 0 && memory < SMALL_MACHINE => 3,
            _ => 60,
        };
        std::time::Duration::from_secs(seconds)
    }

    /// How long a face in use waits between refreshes. See [`Settings::refresh_every`].
    pub fn refresh_after(&self) -> std::time::Duration {
        std::time::Duration::from_secs(match self.refresh_every {
            0 => 2,
            n => u64::from(n),
        })
    }

    /// Index this disk, or leave it out for good: the one way the two lists
    /// change, so a disk is never in both.
    pub fn choose(&mut self, volume: &str, index: bool) {
        self.volumes_on.retain(|v| v != volume);
        self.volumes_off.retain(|v| v != volume);
        let to = if index {
            &mut self.volumes_on
        } else {
            &mut self.volumes_off
        };
        to.push(volume.to_owned());
    }

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

    /// As a face opening reads them: **newest change first**, in every tab,
    /// whatever order was chosen last time. An order picked is kept for the
    /// session, and is still written, but a window always opens on what is new.
    /// The tabs are as [`Settings::start`] says.
    pub fn opened(mut self) -> Settings {
        self.sort.clear();
        self.descending = None;
        for tab in &mut self.tabs {
            tab.sort.clear();
            tab.descending = None;
        }
        match self.start.as_str() {
            "fresh" => {
                self.tabs.clear();
                self.tab = 0;
            }
            "new_tab" if !self.tabs.is_empty() => {
                // An empty tab left from before is the new one: opening after
                // opening does not pile them up.
                let empty = |t: &SearchTab| t.query.trim().is_empty() && t.content.is_empty();
                match self.tabs.iter().rposition(empty) {
                    Some(i) => self.tab = i,
                    None if self.tabs.len() < TABS => {
                        self.tabs.push(SearchTab::default());
                        self.tab = self.tabs.len() - 1;
                    }
                    None => {}
                }
            }
            _ => {}
        }
        self
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
