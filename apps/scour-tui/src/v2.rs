//! Terminal lenses (the place explorer; the time, type and size panels;
//! word choices) and the on-demand inline Grep capsule.

use scour_core::{EntryId, FoundLine, Hit, SourceId};
use scour_proto::{GrepFile, GrepMode, GrepResponse};
use scour_ui::lens::{self, Lens};

use crate::app::{App, Panel, Want};
use crate::draw::PanelLine;

mod content;
mod filters;
mod kinds;
mod place;
mod sizes;
mod spots;
mod time;
mod time_keys;

pub use content::{Deed, Pill};
pub use filters::{Asked, Filters, LensAsk, unix_now};

#[derive(Debug, Clone, Default)]
pub struct V2 {
    pub grep_open: bool,
    pub grep_pattern: String,
    pub grep_mode: GrepMode,
    pub grep_exact: bool,
    pub grep_binary: bool,
    pub grep_markup: bool,
    pub grep_gen: u64,
    pub grep_files: Vec<GrepFile>,
    pub grep_next: Option<u32>,
    pub grep_summary: String,
    /// Where the caret is in the pattern, in characters.
    pub grep_caret: usize,
    /// The capsule a pill is over, if any.
    pub grep_pill: Option<Pill>,
    /// The `:` menu's cursor, and the `:` it was closed on with `Esc`.
    pub grep_menu_at: usize,
    pub grep_menu_shut: Option<usize>,
    pub place_mode: bool,
    pub place_dir: String,
    /// The lens opened on a place term still being typed: the folder it
    /// names and the letters after its last `/`, which narrow that folder's
    /// list. "Add" lets the term go.
    pub place_part: Option<(String, String)>,
    /// The query and caret the place lens last wrote: a term the caret is
    /// in then was written by the lens, not typed.
    pub place_left: Option<(String, usize)>,
    pub tree_dirs: Vec<String>,
    pub lens_items: Vec<(String, String, String, Option<String>)>,
    /// A heading over each item, empty for none: the launcher's groups.
    pub lens_heads: Vec<String>,
    /// The place lens's disks, above its folders.
    pub disks: place::Disks,
    /// The time, type and size panels.
    pub filters: Filters,
    /// The person is typing at the caret: set by a letter typed or rubbed
    /// out, cleared by a lens press or a caret moved. See `lens::Caret`.
    pub editing: bool,
    /// The place lens was opened on an `!under:`: a press stays on one.
    pub place_not: bool,
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

pub fn home_form(p: &str) -> String {
    scour_ui::path::home_form(&home(), p)
}

fn quote_if(s: &str) -> String {
    if s.chars().any(|c| c.is_whitespace() || c == '|' || c == ';') {
        format!("\"{s}\"")
    } else {
        s.to_string()
    }
}

impl App {
    /// The place lens, on the folder the term at the caret names, else on
    /// the one it was last left at.
    pub fn open_place_lens(&mut self) -> Want {
        let hm = home();
        let left = self.v2.place_left.as_ref();
        let ours = left.is_some_and(|(q, c)| *q == self.query && *c == self.caret);
        let at = self
            .place_term()
            .filter(|_| !ours)
            .map(|t| lens::place_at(&t.value, &hm));
        let dir = match &at {
            Some((shown, _)) => shown.clone(),
            None if self.v2.place_dir.is_empty() => lens::place_at("", &hm).0,
            None => self.v2.place_dir.clone(),
        };
        self.v2.place_part = at.filter(|(_, part)| !part.is_empty());
        self.v2.place_not = lens::term_at(&self.query, self.caret.min(self.query.len()))
            .is_some_and(|t| t.not && lens::lens_of(&self.query, &t) == Some(Lens::Place));
        self.v2.place_mode = true;
        self.v2.place_dir = dir.clone();
        self.v2.tree_dirs.clear();
        self.rebuild_lens();
        self.panel = Panel::Lens;
        self.panel_at = 0;
        self.dirty = true;
        let opening = self.place_opened();
        Want::Place { dir, opening }
    }

    pub fn open_context_lens(&mut self) -> Want {
        self.v2.place_mode = false;
        self.v2.filters.view = None;
        self.panel_at = 0;
        self.rebuild_lens();
        self.panel = Panel::Lens;
        self.dirty = true;
        Want::Nothing
    }

    pub fn tree_landed(&mut self, path: String, dirs: Vec<String>) {
        if self.v2.place_dir == path {
            self.v2.tree_dirs = dirs;
            if self.panel == Panel::Lens && self.v2.place_mode {
                self.rebuild_lens();
                self.dirty = true;
            }
        }
    }

    pub fn rebuild_lens(&mut self) {
        self.v2.lens_heads.clear();
        let mut out = Vec::new();
        if self.v2.place_mode {
            let cur = self.v2.place_dir.clone();
            let shown = home_form(&cur);
            out.push((
                format!("+ {} ({shown})", self.say("Add")),
                "Enter".into(),
                format!("under:{}", quote_if(&shown)),
                None,
            ));
            out.push((
                format!("! {} ({shown})", self.say("Exclude")),
                "!".into(),
                format!("!under:{}", quote_if(&shown)),
                None,
            ));
            out.push((
                format!("⊞ {}", self.say("Choose folder…")),
                "o".into(),
                "__pick__".into(),
                None,
            ));
            for (lbl, p) in &self.places {
                out.push((
                    format!("⌂ {lbl}"),
                    home_form(p),
                    format!("under:{}", quote_if(&home_form(p))),
                    Some(p.clone()),
                ));
            }
            let part = match &self.v2.place_part {
                Some((dir, part)) if *dir == cur => part.to_lowercase(),
                _ => String::new(),
            };
            for d in &self.v2.tree_dirs {
                let leaf = scour_ui::path::leaf(d);
                if !leaf.to_lowercase().starts_with(&part) {
                    continue;
                }
                out.push((
                    format!("▸ {leaf}/"),
                    "→".into(),
                    format!("under:{}", quote_if(&home_form(d))),
                    Some(d.clone()),
                ));
            }
            self.v2.lens_items = out;
            self.v2.filters.view = None;
            return;
        }
        if self.filters_rebuild() {
            self.v2.lens_items = out;
            return;
        }

        let t = lens::term_at(&self.query, self.caret.min(self.query.len()));
        match t.as_ref().and_then(|t| lens::lens_of(&self.query, t)) {
            Some(Lens::Word) => {
                if let Some(t) = t {
                    for c in lens::word(&t) {
                        out.push((self.say(&c.label).into_owned(), c.hint, c.term, None));
                    }
                }
            }
            _ => {
                let typed = match &t {
                    Some(t) if lens::lens_of(&self.query, t) == Some(Lens::Launch) => {
                        self.query[t.start..t.end].to_string()
                    }
                    _ => String::new(),
                };
                let mut group = None;
                for s in lens::launch(&self.query, &typed, |m| self.say(m).into_owned()) {
                    let head = if group == Some(s.group) {
                        String::new()
                    } else {
                        self.say(s.group.heading()).into_owned()
                    };
                    group = Some(s.group);
                    self.v2.lens_heads.push(head);
                    let held = if s.held {
                        format!("  · {}", self.say("in the query"))
                    } else {
                        String::new()
                    };
                    out.push((
                        format!("{} ({}){held}", self.say(&s.choice.label), s.choice.term),
                        self.say(&s.choice.hint).into_owned(),
                        s.choice.term,
                        None,
                    ));
                }
            }
        }
        self.v2.lens_items = out;
    }

    /// A launcher line whose filter the query already holds: the caret goes to
    /// the end of that term, and a `:` being typed goes. False for any other.
    fn enter_held(&mut self, term: &str) -> bool {
        let Some(f) = lens::LAUNCH
            .iter()
            .find(|f| term.strip_suffix(':') == Some(f.field))
        else {
            return false;
        };
        let Some(there) = lens::held(&self.query, f.field) else {
            return false;
        };
        let c = self.caret.min(self.query.len());
        let (q, caret) = lens::term_at(&self.query, c)
            .filter(|t| lens::lens_of(&self.query, t) == Some(Lens::Launch))
            .and_then(|t| lens::into_held(&self.query, &t, f.field))
            .unwrap_or_else(|| (self.query.clone(), there.end));
        self.query = q;
        self.caret = caret;
        true
    }

    /// How many lines the lens has: the place lens's disks, then its items.
    pub fn lens_len(&self) -> usize {
        if let Some(n) = self.filter_len() {
            return n;
        }
        self.disk_row_count() + self.v2.lens_items.len()
    }

    /// The line of the place lens's items that is `k`-th after the disks.
    pub fn place_line(&self, k: usize) -> usize {
        self.disk_row_count() + k
    }

    pub fn lens_press(&mut self, drill: bool) -> Want {
        if let Some(want) = self.filter_press() {
            return want;
        }
        let disks = self.disk_row_count();
        if self.panel_at < disks {
            let row = self.disk_rows(&self.place_tiles())[self.panel_at];
            return self.disk_press(row, drill);
        }
        let at = self.panel_at - disks;
        let Some((_, _, term, dir)) = self.v2.lens_items.get(at).cloned() else {
            return Want::Nothing;
        };
        if drill && let Some(next_dir) = dir {
            self.v2.place_dir = next_dir.clone();
            self.v2.tree_dirs.clear();
            self.panel_at = 0;
            self.rebuild_lens();
            self.dirty = true;
            return Want::Tree(next_dir);
        }
        if self.v2.place_mode {
            return self.place_press(at, dir);
        }
        if self.enter_held(&term) {
            self.rebuild_lens();
            return self.typed();
        }
        let c = self.caret.min(self.query.len());
        if let Some(t) = lens::term_at(&self.query, c)
            && lens::lens_of(&self.query, &t).is_some()
        {
            let (q, caret) = lens::replace(&self.query, &t, &term);
            self.query = q;
            self.caret = caret;
        } else {
            self.query = scour_ui::query::toggle(&self.query, &term);
            self.caret = self.query.len();
        }
        if !term.ends_with(':') {
            self.panel = Panel::None;
        } else {
            self.rebuild_lens();
        }
        self.typed()
    }

    /// The caret, and whether the person is typing at it.
    pub fn caret_at(&self) -> lens::Caret {
        lens::Caret {
            at: self.caret.min(self.query.len()),
            editing: self.v2.editing,
        }
    }

    /// The positive place term the caret is in.
    fn place_term(&self) -> Option<lens::Term> {
        let t = lens::term_at(&self.query, self.caret.min(self.query.len()))?;
        (!t.not && lens::lens_of(&self.query, &t) == Some(Lens::Place)).then_some(t)
    }

    /// Press a folder line of the place lens: "Add" puts the folder into the
    /// place group, "Exclude" writes or takes away its `!under:`, any other
    /// folder is the only place searched. The lens closes on it.
    fn place_press(&mut self, at: usize, dir: Option<String>) -> Want {
        let home = home();
        let here = self.v2.place_dir.clone();
        let written = match (at, dir) {
            (0, _) => lens::place_add_at(&self.query, &home, &here, self.caret_at()),
            (1, _) => lens::place_exclude(&self.query, &home, &here),
            (2, _) => {
                let start = std::path::Path::new(&here);
                let Some(p) = scour_openers::pick_folder(Some(start)) else {
                    return Want::Nothing;
                };
                let norm = scour_core::path::from_path(std::path::Path::new(&p));
                self.v2.place_dir = norm.clone();
                lens::place_toggle(&self.query, &home, &norm, lens::Pick::Only)
            }
            (_, Some(d)) => lens::place_toggle(&self.query, &home, &d, lens::Pick::Only),
            (_, None) => return Want::Nothing,
        };
        self.panel = Panel::None;
        self.place_wrote(written)
    }

    pub fn lens_climb_up(&mut self) -> Want {
        if !self.v2.place_mode || self.v2.place_dir == "/" {
            return Want::Nothing;
        }
        let up = match self.v2.place_dir.rfind('/') {
            Some(0) | None => "/".to_string(),
            Some(i) => self.v2.place_dir[..i].to_string(),
        };
        self.v2.place_dir = up.clone();
        self.v2.tree_dirs.clear();
        self.panel_at = 0;
        self.rebuild_lens();
        self.dirty = true;
        Want::Tree(up)
    }

    /// The lens's title and lines; `wide` is the characters a line has.
    pub fn lens_panel_lines(&self, wide: usize) -> (String, Vec<PanelLine>) {
        if let Some(out) = self.filter_lines(wide) {
            return out;
        }
        let title = if self.v2.place_mode {
            format!(
                "{} · {}",
                self.say("Location"),
                home_form(&self.v2.place_dir)
            )
        } else {
            format!(
                "{}  {}",
                self.say("Word matching"),
                sparkline_bar(&self.strip)
            )
        };
        let mut lines = match self.v2.place_mode {
            true => self.disk_list_lines(wide),
            false => Vec::new(),
        };
        let disks = lines.len();
        lines.extend(
            self.v2
                .lens_items
                .iter()
                .enumerate()
                .map(|(at, (lbl, key, _, _))| {
                    let head = self.v2.lens_heads.get(at).cloned().unwrap_or_default();
                    PanelLine {
                        text: lbl.clone(),
                        key: key.clone(),
                        dimmed: false,
                        careful: lbl.starts_with('!'),
                        // The folders under a rule of their own, below the disks.
                        rule: !head.is_empty() || (at == 0 && disks > 0),
                        head,
                        mark: None,
                        lit: None,
                    }
                }),
        );
        (title, lines)
    }

    pub fn toggle_grep(&mut self) -> Want {
        self.v2.grep_open = !self.v2.grep_open;
        self.dirty = true;
        if self.v2.grep_open {
            self.v2.grep_caret = self.v2.grep_pattern.chars().count();
            self.dispatch_grep(false)
        } else {
            self.v2.grep_summary.clear();
            self.typed()
        }
    }

    pub fn dispatch_grep(&mut self, append: bool) -> Want {
        if !self.v2.grep_open || self.v2.grep_pattern.is_empty() {
            self.v2.grep_files.clear();
            self.v2.grep_next = None;
            self.v2.grep_summary.clear();
            return Want::Nothing;
        }
        self.v2.grep_gen += 1;
        let offset = if append {
            self.v2.grep_next.unwrap_or(0)
        } else {
            0
        };
        if !append {
            self.v2.grep_files.clear();
            self.v2.grep_next = None;
            self.v2.grep_summary = self.say("Scanning matching files…").into_owned();
        }
        self.dirty = true;
        Want::Grep {
            generation: self.v2.grep_gen,
            append,
            query: self.asking(),
            pattern: self.v2.grep_pattern.clone(),
            mode: self.v2.grep_mode,
            exact_case: self.v2.grep_exact,
            binary: self.v2.grep_binary,
            markup: self.v2.grep_markup,
            sort: self.sort,
            descending: self.descending,
            offset,
        }
    }

    pub fn grep_landed(&mut self, generation: u64, append: bool, reply: GrepResponse) {
        if generation != self.v2.grep_gen || !self.v2.grep_open {
            return;
        }
        if append {
            self.v2.grep_files.extend(reply.files);
        } else {
            self.v2.grep_files = reply.files;
        }
        self.v2.grep_next = reply.next;
        let mark = self.mark();
        let mut s = self
            .say("{f} files · {s} scanned · {b} read")
            .replace(
                "{f}",
                &scour_ui::format::grouped(self.v2.grep_files.len() as u64, mark.0),
            )
            .replace("{s}", &scour_ui::format::grouped(reply.scanned, mark.0))
            .replace("{b}", &scour_ui::format::compact_bytes(reply.read, mark.1));
        if self.v2.grep_next.is_some() {
            s.push_str(&format!("  ·  F6 {}", self.say("Read more")));
        }
        self.v2.grep_summary = s;
        self.took_us = reply.took_us;

        let hits: Vec<Hit> = self
            .v2
            .grep_files
            .iter()
            .map(|gf| Hit {
                id: EntryId::path_hash(SourceId(0), &gf.path),
                path: gf.path.clone(),
                is_dir: false,
                kind: gf.kind,
                meta: gf.meta,
                under: None,
                found: gf
                    .hits
                    .iter()
                    .map(|h| FoundLine {
                        line: h.line,
                        text: h.text.clone(),
                        at: h.at,
                        marks: h.marks.clone(),
                    })
                    .collect(),
            })
            .collect();
        let n = hits.len();
        self.pages.empty();
        let _ = self.pages.put(0, hits, n);
        self.capped = self.v2.grep_next.is_some();
        self.cursor = 0;
        self.top = 0;
        self.dirty = true;
    }
}

pub fn sparkline_bar(strip: &[(u32, u64)]) -> String {
    const BLOCKS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    if strip.is_empty() {
        return String::new();
    }
    let max = strip.iter().map(|(_, n)| *n).max().unwrap_or(0).max(1);
    strip
        .iter()
        .map(|(_, n)| {
            if *n == 0 {
                '·'
            } else {
                let idx = ((*n as f64 / max as f64) * 7.0).round() as usize;
                BLOCKS[idx.min(7)]
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn place_lens_drills_and_climbs_and_writes_under_term() {
        let mut app = App::default();
        let want = app.open_place_lens();
        assert_eq!(app.panel, Panel::Lens);
        assert!(matches!(want, Want::Place { opening: 1, .. }));

        app.v2.place_dir = "/home/demo/Projeler".into();
        app.tree_landed(
            "/home/demo/Projeler".into(),
            vec!["/home/demo/Projeler/Scour".into()],
        );
        // Drill into the subfolder at the end of the lens list.
        app.panel_at = app.place_line(app.v2.lens_items.len() - 1);
        let drill = app.lens_press(true);
        assert_eq!(drill, Want::Tree("/home/demo/Projeler/Scour".into()));
        assert_eq!(app.v2.place_dir, "/home/demo/Projeler/Scour");

        // Climb back up with Left.
        let up = crate::keys::press(&mut app, KeyEvent::new(KeyCode::Left, KeyModifiers::NONE));
        assert_eq!(up, Want::Tree("/home/demo/Projeler".into()));

        // Press `!` to exclude the current folder.
        let excl = crate::keys::press(
            &mut app,
            KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE),
        );
        assert!(matches!(excl, Want::Page { .. }));
        assert!(app.query.contains("!under:"));
        assert_eq!(app.panel, Panel::None);
    }

    #[test]
    fn grep_capsule_stays_closed_by_default_and_streams_on_ctrl_g() {
        let mut app = App::default();
        assert!(!app.v2.grep_open);

        let w = crate::keys::press(
            &mut app,
            KeyEvent::new(KeyCode::Char('g'), KeyModifiers::CONTROL),
        );
        assert!(app.v2.grep_open);
        assert_eq!(w, Want::Nothing, "empty pattern waits quietly");

        let w = crate::keys::press(
            &mut app,
            KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE),
        );
        assert!(matches!(
            w,
            Want::Grep {
                ref pattern,
                generation: 1,
                ..
            } if pattern == "f"
        ));
        assert!(
            app.query.is_empty(),
            "typing goes into grep capsule while open"
        );

        let _ = crate::keys::press(&mut app, KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!app.v2.grep_open);
    }

    #[test]
    fn the_launcher_says_its_groups_and_goes_into_a_held_filter() {
        let mut app = App {
            query: "rapor !ext:pdf :".into(),
            caret: 16,
            ..Default::default()
        };
        app.open_context_lens();
        let (_, lines) = app.lens_panel_lines(60);
        let heads: Vec<&str> = lines.iter().map(|l| l.head.as_str()).collect();
        assert_eq!(heads, ["Narrow", "", "", "", "", "How it searches", ""]);
        assert!(
            lines[3].text.ends_with("· in the query"),
            "{}",
            lines[3].text
        );
        assert!(!lines[0].text.contains("in the query"));

        // A press lands on the line drawn there, not on the heading above it.
        let area = ratatui::layout::Rect::new(0, 0, 100, 40);
        let top = crate::draw::panel_rect(&app, area, lines.len() + 2).y + 2;
        assert_eq!(crate::draw::panel_line_at(&app, area, 50, top), None);
        assert_eq!(crate::draw::panel_line_at(&app, area, 50, top + 1), Some(0));
        assert_eq!(crate::draw::panel_line_at(&app, area, 50, top + 6), None);
        assert_eq!(crate::draw::panel_line_at(&app, area, 50, top + 7), Some(5));

        app.panel_at = 3;
        app.lens_press(false);
        assert_eq!(app.query, "rapor !ext:pdf");
        assert_eq!(app.caret, 14);
    }

    #[test]
    fn sparkline_scales_bars_and_marks_zeros() {
        let bar = sparkline_bar(&[(30, 0), (7, 50), (1, 100)]);
        assert_eq!(bar, "·▅█");
    }
}
