//! The settings panel: its sections and lines, and the questions it asks.

use super::*;
use crate::link::Deed;

impl App {
    /// Left and right in the menu's strip; nothing elsewhere.
    pub fn panel_side(&mut self, by: isize) -> bool {
        let strip = self.strip_len();
        if self.panel != Panel::Menu || self.panel_at >= strip || strip == 0 {
            return false;
        }
        self.panel_at = self.panel_at.saturating_add_signed(by).min(strip - 1);
        self.dirty = true;
        true
    }

    /// Open the settings on this section's panel, and say what it needs
    /// asked for. The key's panel is asked for by the caller, which has the
    /// desktop's words for it.
    pub fn open_settings(&mut self, panel: Panel) -> Want {
        self.settings = true;
        self.helping = false;
        self.panel = panel;
        self.panel_at = 0;
        self.note.clear();
        self.dirty = true;
        match panel {
            Panel::Rules => Want::Rules,
            Panel::Disks => Want::Disks,
            _ => Want::Nothing,
        }
    }

    pub fn close_settings(&mut self) {
        self.settings = false;
        self.panel = Panel::None;
        self.panel_at = 0;
        self.dirty = true;
    }

    /// Which section a panel belongs to: the key and the faces are reached
    /// from the first, a folder being typed from the fourth.
    pub fn section_of(panel: Panel) -> usize {
        match panel {
            Panel::Columns => 1,
            Panel::Searching => 2,
            Panel::Disks => 3,
            Panel::Later | Panel::Ask => 4,
            Panel::Rules => 5,
            Panel::Keys => 6,
            Panel::About => 7,
            _ => 0,
        }
    }

    /// The next section, or the one before, round.
    pub fn settings_step(&mut self, by: isize) -> Want {
        let at = Self::section_of(self.panel) as isize;
        let n = SECTIONS.len() as isize;
        let next = (at + by).rem_euclid(n) as usize;
        self.open_settings(SECTIONS[next].0)
    }

    /// One of a few values, the next or the one before, round.
    fn cycle(values: &[&str], now: &str, by: isize) -> String {
        let at = values.iter().position(|v| *v == now).unwrap_or(0) as isize;
        values[(at + by).rem_euclid(values.len() as isize) as usize].to_string()
    }

    /// The first section's lines: what each is, and what it is set to.
    pub fn general_lines(&self) -> Vec<(String, String)> {
        let language = if self.language_choice.is_empty() {
            self.say("The desktop's").into_owned()
        } else {
            scour_i18n::LANGUAGES
                .iter()
                .find(|(tag, _)| *tag == self.language_choice)
                .map(|(_, name)| (*name).to_string())
                .unwrap_or_else(|| self.language_choice.clone())
        };
        let face = match self.face_default.as_str() {
            "browser" => "Browser",
            "tui" => "Terminal",
            _ => "Window",
        };
        let theme = match self.theme.as_str() {
            "light" => "Light",
            "dark" => "Dark",
            _ => "The desktop's",
        };
        vec![
            (self.say("Language").into_owned(), format!("‹ {language} ›")),
            (
                self.say("When Scour opens").into_owned(),
                format!("‹ {} ›", self.say(face)),
            ),
            (
                self.say("Theme").into_owned(),
                format!("‹ {} ›", self.say(theme)),
            ),
            (
                format!("{}…", self.say("Keyboard shortcut")),
                self.key_bound
                    .clone()
                    .unwrap_or_else(|| self.say("not bound").into_owned()),
            ),
            (
                format!("{}…", self.say("Open another face now")),
                String::new(),
            ),
        ]
    }

    /// Change the setting on this line of the first section: `by` is left or
    /// right; a press is right.
    pub fn general_side(&mut self, by: isize) -> Want {
        let change = match self.panel_at {
            0 => {
                let mut tags: Vec<&str> = vec![""];
                tags.extend(scour_i18n::LANGUAGES.iter().map(|(tag, _)| *tag));
                let next = Self::cycle(&tags, &self.language_choice, by);
                self.language_choice = next.clone();
                // Spoken now: the same choice the panel made.
                let speak = scour_i18n::choose(&next, "");
                self.words = scour_i18n::Catalogue::for_language(&speak);
                scour_settings::Change {
                    language: Some(next),
                    ..Default::default()
                }
            }
            1 => {
                let next = Self::cycle(&["window", "browser", "tui"], &self.face_default, by);
                self.face_default = next.clone();
                scour_settings::Change {
                    face: Some(next),
                    ..Default::default()
                }
            }
            2 => {
                let next = Self::cycle(&["", "light", "dark"], &self.theme, by);
                self.theme = next.clone();
                scour_settings::Change {
                    theme: Some(next),
                    ..Default::default()
                }
            }
            _ => return Want::Nothing,
        };
        self.dirty = true;
        Want::Remember(change)
    }

    /// The lines of the section on disks that come later.
    pub fn later_lines(&self) -> Vec<(String, String)> {
        let word = |v: &str| match v {
            "always" => "Always scan",
            "never" => "Never",
            _ => "Ask",
        };
        let mut lines = vec![
            (
                self.say("A disk plugged in").into_owned(),
                format!("‹ {} ›", self.say(word(&self.removable))),
            ),
            (
                self.say("Network disks").into_owned(),
                format!("‹ {} ›", self.say(word(&self.network))),
            ),
        ];
        for f in &self.folders {
            lines.push((f.clone(), format!("× {}", self.say("remove"))));
        }
        lines.push((format!("{}…", self.say("Add a folder")), String::new()));
        lines
    }

    /// How words are read: what each choice is, and what it is set to.
    pub fn searching_lines(&self) -> Vec<(String, String)> {
        let typos = match self.typos.as_str() {
            "off" => "Off",
            "always" => "Always",
            _ => "When nothing is found",
        };
        let accents = if self.accents_exact { "Off" } else { "On" };
        vec![
            (
                self.say("Ignore accents").into_owned(),
                format!("‹ {} ›", self.say(accents)),
            ),
            (
                self.say("Typing mistakes").into_owned(),
                format!("‹ {} ›", self.say(typos)),
            ),
        ]
    }

    /// Left or right on one of them; a press is right.
    pub fn searching_side(&mut self, by: isize) -> Want {
        let change = match self.panel_at {
            0 => {
                self.accents_exact = !self.accents_exact;
                scour_settings::Change {
                    accents_exact: Some(self.accents_exact),
                    ..Default::default()
                }
            }
            _ => {
                let now = match self.typos.as_str() {
                    "off" | "always" => self.typos.clone(),
                    _ => String::new(),
                };
                self.typos = Self::cycle(&["off", "", "always"], &now, by);
                scour_settings::Change {
                    typos: Some(self.typos.clone()),
                    ..Default::default()
                }
            }
        };
        self.dirty = true;
        Want::Remember(change)
    }

    /// The query that shows what the search in sight nearly found, in place
    /// of the one that found nothing.
    pub fn show_similar(&mut self) -> Want {
        let Some(similar) = self.similar.take() else {
            return Want::Nothing;
        };
        self.query = similar.query;
        self.caret = self.query.len();
        self.typed()
    }

    /// Left or right on a rule; a press is right, and on a folder takes it out,
    /// and on the last line asks for one.
    pub fn later_side(&mut self, by: isize, pressed: bool) -> Want {
        let folders = self.folders.len();
        let change = match self.panel_at {
            0 => {
                let next = Self::cycle(&["", "always", "never"], &self.removable, by);
                self.removable = next.clone();
                scour_settings::Change {
                    removable: Some(next),
                    ..Default::default()
                }
            }
            1 => {
                let next = Self::cycle(&["", "never"], &self.network, by);
                self.network = next.clone();
                scour_settings::Change {
                    network: Some(next),
                    ..Default::default()
                }
            }
            at if at < 2 + folders => {
                if !pressed {
                    return Want::Nothing;
                }
                self.folders.remove(at - 2);
                self.panel_at = self.panel_at.min(self.panel_lines().saturating_sub(1));
                scour_settings::Change {
                    folders: Some(self.folders.clone()),
                    ..Default::default()
                }
            }
            _ => {
                if !pressed {
                    return Want::Nothing;
                }
                self.pending = Some(("folder".into(), Vec::new()));
                self.ask_title = self.say("Add a folder").into_owned();
                self.ask_typing = true;
                self.ask_text.clear();
                self.ask_yes = self.say("add").into_owned();
                self.panel = Panel::Ask;
                self.panel_at = 0;
                self.dirty = true;
                return Want::Nothing;
            }
        };
        self.dirty = true;
        Want::Remember(change)
    }

    /// A folder typed in, checked here, where the disk can be looked at.
    pub fn add_folder(&mut self, typed: &str) -> Want {
        let typed = typed.trim();
        let path = match typed.strip_prefix('~') {
            Some(rest) if rest.is_empty() || rest.starts_with('/') => {
                format!("{}{rest}", std::env::var("HOME").unwrap_or_default())
            }
            _ => typed.to_owned(),
        };
        let native = std::path::Path::new(&path);
        let wrong = if typed.is_empty() {
            Some("Type a folder's path first.")
        } else if !native.is_absolute() {
            Some("A folder's full path, from the root.")
        } else if !native.is_dir() {
            Some("There is no folder there.")
        } else {
            None
        };
        if let Some(msgid) = wrong {
            self.note = self.say(msgid).into_owned();
            return Want::Nothing;
        }
        let path = scour_core::path::from_path(native);
        let path = match path.trim_end_matches('/') {
            "" => "/".to_owned(),
            p => p.to_owned(),
        };
        if self.folders.contains(&path) {
            self.note = self.say("That folder is already in the list.").into_owned();
            return Want::Nothing;
        }
        self.folders.push(path);
        Want::Remember(scour_settings::Change {
            folders: Some(self.folders.clone()),
            ..Default::default()
        })
    }

    /// Answer the question that is up: `0` is the line being typed into, `1` is
    /// *Cancel*, and only `2` is yes.
    pub fn ask_answer(&mut self, which: usize) -> Want {
        // A folder typed in the settings goes back to where it was asked.
        if self
            .pending
            .as_ref()
            .is_some_and(|(what, _)| what == "folder")
        {
            self.pending = None;
            let typed = std::mem::take(&mut self.ask_text);
            self.ask_typing = false;
            self.panel = Panel::Later;
            self.panel_at = self.panel_lines().saturating_sub(1);
            self.dirty = true;
            return if which < 2 {
                Want::Nothing
            } else {
                self.add_folder(&typed)
            };
        }
        self.panel = Panel::None;
        self.dirty = true;
        let Some((what, paths)) = self.pending.take() else {
            return Want::Nothing;
        };
        // 0 is the line being typed into and 1 is *Cancel*; only 2 is yes.
        if which < 2 {
            return Want::Nothing;
        }
        let typing = std::mem::take(&mut self.ask_text);
        self.ask_typing = false;
        if what == "rename" {
            let Some(from) = paths.first() else {
                return Want::Nothing;
            };
            return match scour_name::rename(&scour_core::path::to_path(from), &typing) {
                Ok(now) => Want::Recheck(vec![from.clone(), scour_core::path::from_path(&now)]),
                Err(why) => {
                    self.note = self.say(why.msgid()).into_owned();
                    Want::Nothing
                }
            };
        }
        if what == "open-all" {
            for p in &paths {
                launch(p);
            }
            return Want::Nothing;
        }
        if what == "delete" {
            let mut gone = 0usize;
            let mut refused: Option<String> = None;
            for p in &paths {
                match scour_trash::erase(&scour_core::path::to_path(p)) {
                    Ok(()) => gone += 1,
                    Err(e) => {
                        refused.get_or_insert_with(|| e.to_string());
                    }
                }
            }
            self.note = match refused {
                Some(why) => why,
                None => self.say("{n} deleted").replace("{n}", &gone.to_string()),
            };
            return Want::Recheck(paths);
        }
        // The move is this process's. The service is only told to look again.
        let mut gone = 0usize;
        let mut refused: Option<String> = None;
        for p in &paths {
            match scour_trash::trash(&scour_core::path::to_path(p)) {
                Ok(_) => gone += 1,
                Err(e) => {
                    refused.get_or_insert_with(|| e.to_string());
                }
            }
        }
        self.note = match refused {
            Some(why) => why,
            None => self
                .say("{n} moved to trash")
                .replace("{n}", &gone.to_string()),
        };
        Want::Recheck(paths)
    }

    /// How many lines the open panel offers.
    pub fn panel_lines(&self) -> usize {
        match self.panel {
            Panel::Rules => self.rules.len(),
            Panel::Disks => self.disk_lines(),
            // The line typed into and the one that removes. The state under
            // them is drawn but not walked: a cursor on a fact is a press that
            // does nothing. Where nothing can be bound there is only the
            // sentence, and nothing to press at all.
            Panel::Key if self.key_can => 2,
            Panel::Key => 0,
            Panel::Faces => 3,
            Panel::Menu => self.menu.len(),
            Panel::Openers => self.openers.len(),
            // Every column, and the line that goes back to the default.
            Panel::Columns => scour_ui::COLUMNS.len() + 1,
            Panel::Ask => 3,
            Panel::General => 5,
            // The two rules, a line a folder, and the one that adds.
            Panel::Later => 3 + self.folders.len(),
            Panel::Searching => 2,
            Panel::Lens => self.v2.lens_items.len(),
            Panel::Keys | Panel::About | Panel::None => 0,
        }
    }

    /// Move the cursor inside whatever panel is open.
    pub fn panel_walk(&mut self, by: isize) {
        let lines = self.panel_lines();
        if lines == 0 {
            return;
        }
        // The strip is one row: down leaves it for the list, up from the
        // list's first item enters it at its start.
        let strip = if self.panel == Panel::Menu {
            self.strip_len()
        } else {
            0
        };
        if strip > 0 {
            if self.panel_at < strip {
                if by > 0 {
                    self.panel_at = strip.min(lines - 1);
                }
            } else if self.panel_at == strip && by < 0 {
                self.panel_at = 0;
            } else {
                self.panel_at = self
                    .panel_at
                    .saturating_add_signed(by)
                    .clamp(strip, lines - 1);
            }
            self.dirty = true;
            return;
        }
        self.panel_at = self
            .panel_at
            .saturating_add_signed(by)
            .min(lines.saturating_sub(1));
        self.dirty = true;
    }

    /// Bind what has been typed into the key panel. Nothing is read here: what
    /// counts as a combination is `scour-hotkey`'s judgement, and it answers on
    /// the worker lane.
    pub fn bind_key(&mut self) -> Want {
        let typed = self.key_typed.trim().to_owned();
        if typed.is_empty() {
            self.key_note = self.say("type a combination and press Enter").into_owned();
            self.key_bad = false;
            self.dirty = true;
            return Want::Nothing;
        }
        self.key_note.clear();
        self.key_bad = false;
        self.dirty = true;
        Want::Hotkey(Deed::Bind(typed))
    }

    /// Take the binding off, whatever was typed.
    pub fn drop_key(&mut self) -> Want {
        self.key_typed.clear();
        self.key_note.clear();
        self.key_bad = false;
        self.dirty = true;
        Want::Hotkey(Deed::Clear)
    }

    /// What the desktop said about its key. On the event, never the draw.
    pub fn keyed(
        &mut self,
        desktop: String,
        key: Option<String>,
        command: String,
        can_bind: bool,
        trouble: String,
        later: bool,
    ) {
        self.key_desktop = desktop;
        self.key_bound = key;
        self.key_command = command;
        self.key_can = can_bind;
        self.key_bad = !trouble.is_empty();
        self.key_note = if self.key_bad {
            // A refusal that is a sentence of the catalogue's; a tool's own
            // words come back as they are.
            scour_core::Catalog::get(&self.words, &trouble).into_owned()
        } else if later {
            self.say("It takes effect after the next login.")
                .into_owned()
        } else {
            String::new()
        };
        // What was typed has been taken; leaving it in the field invites a
        // second press of a key that is already bound.
        if !self.key_bad {
            self.key_typed.clear();
        }
        self.dirty = true;
    }

    /// Start another face, and remember that it is the one to open: 0 is the
    /// window, 1 is this, 2 is the browser. Through `scour-open`.
    pub fn run_face(&mut self, which: usize) -> Want {
        let face = match which {
            0 => "window",
            1 => "tui",
            _ => "browser",
        };
        if face == "tui" {
            self.note = self.say("already running here").into_owned();
            self.dirty = true;
            return Want::Nothing;
        }
        // Detached, or closing this terminal closes what it just opened.
        let spawned = match scour_launch::face(face) {
            Some(mut command) => command.spawn(),
            None => Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "not installed",
            )),
        };
        match spawned {
            Ok(_) => {
                self.note = self.say("starting…").into_owned();
                // And this one goes: switching is moving, not opening a second.
                self.leaving = true;
            }
            Err(e) => self.note = format!("{face}: {e}"),
        }
        self.panel = Panel::None;
        self.dirty = true;
        Want::Remember(scour_settings::Change {
            face: Some(face.to_string()),
            ..Default::default()
        })
    }
}
