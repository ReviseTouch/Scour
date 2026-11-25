//! Terminal lenses (Place/Tree explorer, Sparkline & Fuzzy choices) and the
//! on-demand inline Grep capsule.

use scour_core::{EntryId, FoundLine, Hit, SourceId};
use scour_proto::{GrepFile, GrepMode, GrepResponse};
use scour_ui::lens::{self, Lens};

use crate::app::{App, Panel, Want};
use crate::draw::PanelLine;

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
    pub place_mode: bool,
    pub place_dir: String,
    pub tree_dirs: Vec<String>,
    pub lens_items: Vec<(String, String, String, Option<String>)>,
}

fn home() -> String {
    std::env::var("HOME").unwrap_or_default()
}

pub fn home_form(p: &str) -> String {
    scour_ui::path::home_form(&home(), p)
}

fn quote_if(s: &str) -> String {
    if s.chars().any(char::is_whitespace) {
        format!("\"{s}\"")
    } else {
        s.to_string()
    }
}

impl App {
    pub fn open_place_lens(&mut self) -> Want {
        let dir = if self.v2.place_dir.is_empty() {
            let hm = home();
            if hm.is_empty() { "/".into() } else { hm }
        } else {
            self.v2.place_dir.clone()
        };
        self.v2.place_mode = true;
        self.v2.place_dir = dir.clone();
        self.v2.tree_dirs.clear();
        self.rebuild_lens();
        self.panel = Panel::Lens;
        self.panel_at = 0;
        self.dirty = true;
        Want::Tree(dir)
    }

    pub fn open_context_lens(&mut self) -> Want {
        self.v2.place_mode = false;
        self.rebuild_lens();
        self.panel = Panel::Lens;
        self.panel_at = 0;
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
        let mut out = Vec::new();
        if self.v2.place_mode {
            let cur = self.v2.place_dir.clone();
            let shown = home_form(&cur);
            out.push((
                format!("✓ {} ({shown})", self.say("Include folder")),
                "Enter".into(),
                format!("under:{}", quote_if(&shown)),
                None,
            ));
            out.push((
                format!("! {} ({shown})", self.say("Exclude folder")),
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
            for d in &self.v2.tree_dirs {
                let leaf = scour_ui::path::leaf(d);
                out.push((
                    format!("▸ {leaf}/"),
                    "→".into(),
                    format!("under:{}", quote_if(&home_form(d))),
                    Some(d.clone()),
                ));
            }
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
            Some(Lens::Time) => {
                if let Some(t) = t {
                    for c in lens::bands(&t, lens::TIMES) {
                        out.push((self.say(&c.label).into_owned(), c.hint, c.term, None));
                    }
                }
            }
            Some(Lens::Size) => {
                if let Some(t) = t {
                    for c in lens::bands(&t, lens::SIZES) {
                        out.push((self.say(&c.label).into_owned(), c.hint, c.term, None));
                    }
                }
            }
            _ => {
                for (f, lbl, about) in lens::LAUNCH {
                    out.push((
                        format!("{} ({f})", self.say(lbl)),
                        self.say(about).into_owned(),
                        (*f).to_owned(),
                        None,
                    ));
                }
            }
        }
        self.v2.lens_items = out;
    }

    pub fn lens_press(&mut self, drill: bool) -> Want {
        let Some((_, _, term, dir)) = self.v2.lens_items.get(self.panel_at).cloned() else {
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
        if term == "__pick__" {
            let start = std::path::Path::new(&self.v2.place_dir);
            if let Some(p) = scour_openers::pick_folder(Some(start)) {
                let norm = scour_core::path::from_path(std::path::Path::new(&p));
                self.v2.place_dir = norm.clone();
                self.query = scour_ui::query::toggle(
                    &self.query,
                    &format!("under:{}", quote_if(&home_form(&norm))),
                );
                self.caret = self.query.len();
                self.panel = Panel::None;
                return self.typed();
            }
            return Want::Nothing;
        }
        let c = self.caret.min(self.query.len());
        if !self.v2.place_mode
            && let Some(t) = lens::term_at(&self.query, c)
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

    pub fn lens_panel_lines(&self) -> (String, Vec<PanelLine>) {
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
        let lines = self
            .v2
            .lens_items
            .iter()
            .map(|(lbl, key, _, _)| PanelLine {
                text: lbl.clone(),
                key: key.clone(),
                dimmed: false,
                careful: lbl.starts_with('!'),
                rule: false,
            })
            .collect();
        (title, lines)
    }

    pub fn toggle_grep(&mut self) -> Want {
        self.v2.grep_open = !self.v2.grep_open;
        self.dirty = true;
        if self.v2.grep_open {
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
        assert!(matches!(want, Want::Tree(_)));

        app.v2.place_dir = "/home/demo/Projeler".into();
        app.tree_landed(
            "/home/demo/Projeler".into(),
            vec!["/home/demo/Projeler/Scour".into()],
        );
        // Drill into the subfolder at the end of the lens list.
        app.panel_at = app.v2.lens_items.len() - 1;
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
    fn sparkline_scales_bars_and_marks_zeros() {
        let bar = sparkline_bar(&[(30, 0), (7, 50), (1, 100)]);
        assert_eq!(bar, "·▅█");
    }
}
