//! The place lens's head: where the query looks ("Searching in:", from
//! `scour_ui::lens::spots`), then a line a disk, above the folders. What a
//! disk's line says comes from `scour_ui::disks::tiles`, what a choice writes
//! from `scour_ui::lens::tile_toggle`.

use std::collections::{HashMap, HashSet};

use scour_ui::disks::{DiskIn, Shown, SourceIn, Tile, tiles};
use scour_ui::lens::{self, Pick};

use super::{home, home_form};
use crate::app::{App, Want};
use crate::draw::{Mark, PanelLine};

/// What an asking disk is offered: what the service takes, the words, the key.
pub const ANSWERS: [(&str, &str, char); 3] = [
    ("index", "Index it", 'i'),
    ("later", "Not this time", 'l'),
    ("leave", "Never", 'n'),
];

/// Where the name starts on a disk's line: after the held tick and the mark.
const INDENT: usize = 4;

/// What the lens knows about the disks for one opening of it.
#[derive(Debug, Clone, Default)]
pub struct Disks {
    /// Which opening this is: an answer to an older one is dropped.
    pub opening: u64,
    /// The last `sources` answer, by name and roots; kept to draw from while
    /// this opening's own is on its way.
    pub sources: Option<Vec<(String, Vec<String>)>>,
    /// This opening's `sources` has come, so counts may be asked.
    pub fresh: bool,
    /// Files per disk id, counted in this opening.
    pub files: HashMap<String, u64>,
    /// The disks this opening has asked to count.
    pub asked: HashSet<String>,
}

/// One line of the lens's head.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Row {
    /// "Searching in:", always the first line: a digit takes its k-th out.
    Spots,
    /// The disk, by its place among the tiles.
    Tile(usize),
    /// Under a disk indexed through some of its folders, `Partly` or being
    /// walked: those folders.
    Roots(usize),
    /// One of [`ANSWERS`] to the question an asking disk carries.
    Answer(usize, usize),
}

impl Row {
    fn tile(self) -> Option<usize> {
        match self {
            Row::Spots => None,
            Row::Tile(i) | Row::Roots(i) | Row::Answer(i, _) => Some(i),
        }
    }
}

/// `s` cut to `wide` characters, the end given up for a `…`.
pub(super) fn head_of(s: &str, wide: usize) -> String {
    match s.chars().count() > wide {
        true => s
            .chars()
            .take(wide.saturating_sub(1))
            .chain(['…'])
            .collect(),
        false => s.to_string(),
    }
}

/// `s` cut to `wide` characters, the start given up: a path's leaf says most.
fn tail_of(s: &str, wide: usize) -> String {
    let n = s.chars().count();
    match n > wide {
        true => ['…']
            .into_iter()
            .chain(s.chars().skip(n + 1 - wide.max(1)))
            .collect(),
        false => s.to_string(),
    }
}

impl App {
    /// A new opening of the place lens: counts start again.
    pub(super) fn place_opened(&mut self) -> u64 {
        let d = &mut self.v2.disks;
        d.opening += 1;
        d.fresh = false;
        d.files.clear();
        d.asked.clear();
        d.opening
    }

    /// The disks as the lens shows them.
    pub fn place_tiles(&self) -> Vec<Tile> {
        let disks: Vec<DiskIn> = self
            .disks
            .iter()
            .map(|d| DiskIn {
                id: &d.volume.id,
                label: &d.volume.label,
                kind: d.volume.kind.as_str(),
                fs: &d.volume.fs,
                roots: &d.volume.roots,
                size: d.volume.size,
                state: d.state.as_str(),
                live: d.seen.live,
                walking: d.seen.walking,
            })
            .collect();
        let sources: Vec<SourceIn> = self
            .v2
            .disks
            .sources
            .iter()
            .flatten()
            .map(|(name, roots)| SourceIn { name, roots })
            .collect();
        tiles(&disks, &sources, |id| self.v2.disks.files.get(id).copied())
    }

    /// The head's lines: "Searching in:", then the disks. No disk before any
    /// `sources` answer: without it the system disk reads as not indexed.
    pub fn disk_rows(&self, tiles: &[Tile]) -> Vec<Row> {
        let mut rows = vec![Row::Spots];
        if self.v2.disks.sources.is_none() {
            return rows;
        }
        for (i, t) in tiles.iter().enumerate() {
            rows.push(Row::Tile(i));
            // Indexed through some of its folders, whatever it is doing now.
            if !t.whole && !t.roots.is_empty() {
                rows.push(Row::Roots(i));
            }
            if t.shown == Shown::Asking {
                let offered = (0..ANSWERS.len()).filter(|&k| k > 0 || t.index_it);
                rows.extend(offered.map(|k| Row::Answer(i, k)));
            }
        }
        rows
    }

    /// How many lines the head takes at the top of the place lens.
    pub fn disk_row_count(&self) -> usize {
        match self.v2.place_mode {
            true => self.disk_rows(&self.place_tiles()).len(),
            false => 0,
        }
    }

    /// This opening's sources came: the disks can be counted.
    pub fn sources_came(&mut self, opening: u64, sources: Vec<(String, Vec<String>)>) -> Want {
        if opening != self.v2.disks.opening {
            return Want::Nothing;
        }
        let before = self.disk_row_count();
        self.v2.disks.sources = Some(sources);
        self.v2.disks.fresh = true;
        // A cursor already moved stays on the line it was on.
        if self.panel_at > 0 {
            self.panel_at += self.disk_row_count().saturating_sub(before);
        }
        self.dirty = true;
        self.disk_counts()
    }

    /// Ask to count every disk that can be chosen and has not been asked for
    /// in this opening. Nothing while the lens is shut or its sources are old.
    pub fn disk_counts(&mut self) -> Want {
        let open = self.panel == crate::app::Panel::Lens && self.v2.place_mode;
        if !open || !self.v2.disks.fresh {
            return Want::Nothing;
        }
        let asks: Vec<(String, String)> = self
            .place_tiles()
            .into_iter()
            .filter(|t| !self.v2.disks.asked.contains(&t.id))
            .filter_map(|t| Some((t.id, t.count?)))
            .collect();
        if asks.is_empty() {
            return Want::Nothing;
        }
        let d = &mut self.v2.disks;
        d.asked.extend(asks.iter().map(|(id, _)| id.clone()));
        Want::DiskCounts {
            opening: d.opening,
            asks,
        }
    }

    pub fn disk_counted(&mut self, opening: u64, id: &str, total: u64) {
        if opening != self.v2.disks.opening {
            return;
        }
        self.v2.disks.files.insert(id.to_string(), total);
        self.dirty = true;
    }

    /// Press a line of the head. `drill` goes into the disk's root; the
    /// "Searching in:" line answers to its digits only.
    pub(super) fn disk_press(&mut self, row: Row, drill: bool) -> Want {
        let tiles = self.place_tiles();
        let Some(t) = row.tile().and_then(|i| tiles.get(i)) else {
            return Want::Nothing;
        };
        match row {
            Row::Answer(..) if drill => Want::Nothing,
            Row::Answer(_, k) => Want::Disk {
                volume: t.id.clone(),
                what: ANSWERS[k].0.into(),
            },
            _ if drill => {
                self.v2.place_dir = t.root.clone();
                self.v2.tree_dirs.clear();
                self.rebuild_lens();
                self.dirty = true;
                Want::Tree(t.root.clone())
            }
            _ if !t.choosable() => Want::Nothing,
            _ => {
                let written = lens::tile_toggle(&self.query, &home(), &t.roots, Pick::Only);
                self.place_wrote(written)
            }
        }
    }

    /// A key of the line under the cursor: a spot's digit on "Searching in:",
    /// an answer's letter on one of that disk's answers.
    pub fn disk_key(&mut self, key: char) -> Option<Want> {
        let rows = self.disk_rows(&self.place_tiles());
        if rows.get(self.panel_at) == Some(&Row::Spots) {
            let k = key.to_digit(10).filter(|&k| k > 0)? as usize;
            return Some(self.spot_out(k).unwrap_or(Want::Nothing));
        }
        let Some(Row::Answer(i, _)) = rows.get(self.panel_at).copied() else {
            return None;
        };
        let k = ANSWERS.iter().position(|a| a.2 == key)?;
        let at = rows.iter().position(|r| *r == Row::Answer(i, k))?;
        self.panel_at = at;
        Some(self.disk_press(Row::Answer(i, k), false))
    }

    /// The disk list as panel lines, `wide` characters each with the count.
    pub(super) fn disk_list_lines(&self, wide: usize) -> Vec<PanelLine> {
        let tiles = self.place_tiles();
        let rows = self.disk_rows(&tiles);
        let group = self.mark().0;
        let count = |n: u64| {
            self.say("{n} files")
                .replace("{n}", &scour_ui::format::grouped(n, group))
        };
        let states: Vec<String> = tiles
            .iter()
            .map(|t| self.say(t.shown.label()).into_owned())
            .collect();
        let state_w = states.iter().map(|s| s.chars().count()).max().unwrap_or(0);
        let name_w = tiles
            .iter()
            .map(|t| t.name.chars().count())
            .max()
            .unwrap_or(0)
            .min(16);
        // The widest count there will be, so the columns hold when it comes.
        let count_w = count(9_999_999).chars().count();
        let root_w = wide.saturating_sub(INDENT + name_w + 2 + 2 + state_w + 2 + count_w);
        let root_w = if root_w < 8 { 0 } else { root_w };
        let home = home();
        let scope = lens::scope(&self.query, &home);
        let line = |text: String, key: String, dimmed: bool| PanelLine {
            text,
            key,
            dimmed,
            careful: false,
            rule: false,
            head: String::new(),
            mark: None,
            lit: None,
        };
        let mut out: Vec<PanelLine> = rows
            .iter()
            .map(|row| match *row {
                Row::Spots => self.spots_line(&lens::spots(&self.query, &home, &tiles), wide),
                Row::Tile(i) => {
                    let t = &tiles[i];
                    let held = lens::tile_held(t, &scope);
                    let mut text = format!(
                        "{} ■ {:<name_w$}",
                        if held { "✓" } else { " " },
                        head_of(&t.name, name_w)
                    );
                    if root_w > 0 {
                        text.push_str(&format!(
                            "  {:<root_w$}",
                            tail_of(&home_form(&t.root), root_w)
                        ));
                    }
                    text.push_str("  ");
                    text.push_str(&states[i]);
                    let mut l = line(
                        text,
                        t.files.map(count).unwrap_or_default(),
                        t.shown == Shown::Off,
                    );
                    l.mark = Some(Mark {
                        at: 2,
                        kind: t.kind,
                    });
                    l
                }
                Row::Roots(i) => {
                    let roots: Vec<String> = tiles[i].roots.iter().map(|r| home_form(r)).collect();
                    let text = head_of(&roots.join(" · "), wide.saturating_sub(INDENT));
                    line(format!("{:INDENT$}{text}", ""), String::new(), true)
                }
                Row::Answer(_, k) => line(
                    format!("{:INDENT$}{}", "", self.say(ANSWERS[k].1)),
                    ANSWERS[k].2.to_string(),
                    false,
                ),
            })
            .collect();
        if let Some(first) = out.get_mut(1) {
            first.rule = true;
            first.head = self.say("Disks").into_owned();
        }
        out
    }
}

#[cfg(test)]
#[path = "place_tests.rs"]
pub(super) mod tests;
