//! The row menu: what it offers, and doing it.

use super::*;

impl App {
    /// What can be done with a selection, in the order the bar shows them — the
    /// same three the window offers, in the same words.
    pub fn deeds(&self) -> [(&'static str, char); 3] {
        [("copy paths", 'y'), ("open folders", 'o'), ("clear", 'x')]
    }

    /// Do one of them.
    pub fn deed(&mut self, which: usize) -> Want {
        match which {
            0 => {
                let paths: Vec<String> = self.picked.keys().cloned().collect();
                let n = paths.len();
                // Nothing picked, nothing done: copying an empty selection
                // empties somebody's clipboard rather than leaving it alone.
                if n == 0 {
                    self.note = self.say("nothing picked").into_owned();
                    self.dirty = true;
                    return Want::Nothing;
                }
                self.note = match copy(&paths.join("\n")) {
                    Ok(how) if n == 1 => format!("{} ({how})", self.say("path copied")),
                    Ok(how) => format!(
                        "{} ({how})",
                        self.say("{n} paths copied").replace("{n}", &n.to_string())
                    ),
                    Err(why) => format!("{}: {why}", self.say("nothing copied")),
                };
            }
            1 => {
                // Each folder once, however many of its files are picked.
                let mut folders: Vec<&str> = self
                    .picked
                    .keys()
                    .map(|p| scour_ui::path::folder(p))
                    .collect();
                folders.sort_unstable();
                folders.dedup();
                // Each picked row selected in its folder, as the window and
                // the page show them; the count below is still of folders.
                let native: Vec<std::path::PathBuf> = self
                    .picked
                    .keys()
                    .map(|p| scour_core::path::to_path(p))
                    .collect();
                let paths: Vec<&std::path::Path> =
                    native.iter().map(std::path::PathBuf::as_path).collect();
                scour_openers::reveal(&paths);
                self.note = self
                    .say("opening {n} folders")
                    .replace("{n}", &folders.len().to_string());
            }
            _ => {
                self.unpick();
                self.note.clear();
            }
        }
        self.dirty = true;
        Want::Nothing
    }

    /// Build the menu for the row the cursor is on, and open it. What is in it
    /// is `scour-ui::menu`'s decision, asked as `Face::Terminal`.
    pub fn open_menu(&mut self) {
        let Some(hit) = self.here().cloned() else {
            return;
        };
        let picked = self.picked.len();
        let count = scour_ui::format::grouped(picked.max(1) as u64, self.mark().0);
        let all: Vec<_> =
            scour_ui::menu::items_for(picked, hit.is_dir, scour_ui::faces::Face::Terminal)
                .collect();
        self.menu.clear();
        for item in scour_ui::menu::strip(&all) {
            self.menu.push(MenuLine {
                id: item.id.to_string(),
                label: self.say(item.short).into_owned(),
                key: item.key.to_string(),
                rule: false,
                careful: item.weight == scour_ui::menu::Weight::Careful,
                heavy: false,
                strip: true,
            });
        }
        let mut last: Option<u8> = None;
        for item in scour_ui::menu::list(&all) {
            self.menu.push(MenuLine {
                id: item.id.to_string(),
                label: self.say(item.msgid).replace("{n}", &count),
                key: item.key.to_string(),
                rule: last.is_some_and(|l| l != item.group),
                careful: item.weight == scour_ui::menu::Weight::Careful,
                heavy: item.weight == scour_ui::menu::Weight::Heavy,
                strip: false,
            });
            last = Some(item.group);
        }
        let decimal = self.mark().1;
        if picked > 1 {
            let dirs = self.picked.keys().filter(|p| self.is_dir_picked(p)).count();
            let files = picked - dirs;
            let bytes: i64 = self.picked.values().map(|b| (*b).max(0)).sum();
            let mark = self.mark().0;
            let mut said = Vec::new();
            if files > 0 {
                said.push(
                    self.say("{n} files")
                        .replace("{n}", &scour_ui::format::grouped(files as u64, mark)),
                );
            }
            if dirs > 0 {
                said.push(
                    self.say("{n} folders")
                        .replace("{n}", &scour_ui::format::grouped(dirs as u64, mark)),
                );
            }
            if files > 0 {
                said.push(scour_ui::format::size(bytes as u64, decimal));
            }
            self.menu_title = self.say("{n} selected").replace("{n}", &count);
            self.menu_detail = said.join(" · ");
        } else {
            let folder = scour_ui::path::folder(&hit.path);
            let folder = match std::env::var("HOME") {
                Ok(home)
                    if !home.is_empty()
                        && (folder == home || folder.starts_with(&format!("{home}/"))) =>
                {
                    format!("~{}", &folder[home.len()..])
                }
                _ => folder.to_string(),
            };
            self.menu_title = hit.name().to_string();
            self.menu_detail = if hit.is_dir {
                folder
            } else {
                format!(
                    "{folder} · {}",
                    scour_ui::format::size(hit.meta.size.max(0) as u64, decimal)
                )
            };
        }
        self.panel = Panel::Menu;
        // On the list's first item, which opens: Enter means what it always did.
        self.panel_at = self.strip_len();
        self.note.clear();
        self.dirty = true;
    }

    /// What a drag from this row carries: the selection when the row is in
    /// it, else the row alone — what a file manager does.
    pub fn drag_rows(&self, row: usize) -> Vec<String> {
        let Some(hit) = self.pages.at(row) else {
            return Vec::new();
        };
        if self.picked.len() > 1 && self.picked.contains_key(&hit.path) {
            self.picked.keys().cloned().collect()
        } else {
            vec![hit.path.clone()]
        }
    }

    /// Drag these out by way of the window's drag tray: a terminal's window is
    /// the terminal's, so no drag can start from here.
    pub fn drag_out(&mut self, paths: &[String]) -> Want {
        if paths.is_empty() {
            return Want::Nothing;
        }
        let mut command = std::process::Command::new(sibling("scour-gui"));
        command
            .arg("--drag")
            .args(paths)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        // Its own process group, or closing this terminal closes the tray.
        scour_ui::faces::detach(&mut command);
        self.note = match command.spawn() {
            Ok(_) => self
                .say("drag them from the window that opened")
                .into_owned(),
            Err(e) => format!("scour-gui: {e}"),
        };
        self.dirty = true;
        Want::Nothing
    }

    /// Which rows the menu is about: the selection, or the row under it.
    fn menu_rows(&self) -> Vec<String> {
        if self.picked.len() > 1 {
            self.picked.keys().cloned().collect()
        } else {
            self.here()
                .map(|h| vec![h.path.clone()])
                .unwrap_or_default()
        }
    }

    /// Do whatever the menu's cursor is on.
    pub fn menu_pick(&mut self) -> Want {
        let Some(line) = self.menu.get(self.panel_at).cloned() else {
            return Want::Nothing;
        };
        let rows = self.menu_rows();
        let Some(first) = rows.first().cloned() else {
            return Want::Nothing;
        };
        let is_dir = self.here().map(|h| h.is_dir).unwrap_or(false);
        let leaf = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
        let folder = |p: &str| scour_ui::path::folder(p).to_string();

        // Everything but the two that ask closes the menu on the press.
        if line.id != "trash" && line.id != "open-all" && line.id != "delete" {
            self.panel = Panel::None;
            self.dirty = true;
        }

        match line.id.as_str() {
            "open" => {
                launch(&first);
                Want::Nothing
            }
            // The row itself, selected in its folder: opening the folder alone
            // did nothing to one already open, and showed nothing.
            "folder" => {
                scour_openers::reveal(&[scour_core::path::to_path(&first).as_path()]);
                Want::Nothing
            }
            "folders" => {
                let native: Vec<std::path::PathBuf> =
                    rows.iter().map(|p| scour_core::path::to_path(p)).collect();
                let paths: Vec<&std::path::Path> =
                    native.iter().map(std::path::PathBuf::as_path).collect();
                scour_openers::reveal(&paths);
                Want::Nothing
            }
            "clear" => {
                self.picked.clear();
                Want::Nothing
            }

            // The three that exist because there is an index.
            "search-here" => {
                let scope = if is_dir {
                    first.clone()
                } else {
                    folder(&first)
                };
                self.query = format!("under:{scope}");
                self.caret = self.query.len();
                self.typed()
            }
            "search-kind" => {
                let name = leaf(&first);
                match name.rfind('.').filter(|at| *at > 0) {
                    Some(at) => {
                        let ext = name[at + 1..].to_lowercase();
                        self.query = format!("ext:{ext}");
                        self.caret = self.query.len();
                        self.typed()
                    }
                    None => {
                        self.note = self.say("no extension to search for").into_owned();
                        Want::Nothing
                    }
                }
            }
            "duplicates" => Want::Report(self.scope()),
            "usage" => Want::Weigh(first.clone()),
            "skip" => {
                self.note = leaf(&first);
                self.show(Panel::Rules);
                Want::Nothing
            }

            "copy-path" | "copy-name" => {
                let text = if line.id == "copy-name" {
                    rows.iter().map(|p| leaf(p)).collect::<Vec<_>>().join("\n")
                } else {
                    rows.join("\n")
                };
                self.note = match scour_clip::text(&text) {
                    Ok(()) => self.say("path copied").into_owned(),
                    Err(e) => e.to_string(),
                };
                Want::Nothing
            }
            "copy-file" => {
                let native: Vec<std::path::PathBuf> =
                    rows.iter().map(|p| scour_core::path::to_path(p)).collect();
                let paths: Vec<&std::path::Path> =
                    native.iter().map(std::path::PathBuf::as_path).collect();
                self.note = match scour_clip::files(&paths) {
                    Ok(()) => self.say("path copied").into_owned(),
                    Err(e) => e.to_string(),
                };
                Want::Nothing
            }
            "details" => Want::Peek(first.clone()),
            "drag" => self.drag_out(&rows),
            "csv" => self.write_sheet(),

            "open-with" => {
                let name = leaf(&first);
                let mime = scour_thumbs::known::known().mime_of(&name).unwrap_or("");
                self.openers = scour_openers::openers(mime)
                    .into_iter()
                    .map(|o| {
                        (
                            o.id,
                            if o.preferred {
                                format!("★ {}", o.name)
                            } else {
                                o.name
                            },
                        )
                    })
                    .collect();
                if self.openers.is_empty() {
                    self.note = self.say("nothing on this machine claims it").into_owned();
                    return Want::Nothing;
                }
                self.panel = Panel::Openers;
                self.panel_at = 0;
                self.dirty = true;
                Want::Nothing
            }

            "send" => {
                // The places take the openers' panel, told apart by their ids.
                self.openers = scour_sendto::targets()
                    .into_iter()
                    .map(|t| {
                        let label = if t.named {
                            t.label
                        } else {
                            self.say(&t.label).into_owned()
                        };
                        (format!("send:{}", t.id), label)
                    })
                    .collect();
                if self.openers.is_empty() {
                    self.note = self.say("nowhere to send to on this machine").into_owned();
                    return Want::Nothing;
                }
                self.panel = Panel::Openers;
                self.panel_at = 0;
                self.dirty = true;
                Want::Nothing
            }

            "rename" => {
                self.ask_title = self.say("Rename…").into_owned();
                self.ask_text = leaf(&first);
                self.ask_yes = self.say("Rename").into_owned();
                self.ask_typing = true;
                self.pending = Some(("rename".into(), vec![first.clone()]));
                self.panel = Panel::Ask;
                // The cursor sits on *Cancel* while the letters go into the
                // line above it: one keyboard, two places it could be typing.
                self.panel_at = 1;
                self.dirty = true;
                Want::Nothing
            }

            "trash" | "open-all" | "delete" => {
                // A folder's name reads like a file's; it takes its tree with it.
                // Asked of the disk: a pick remembers its size and not its kind.
                let dir = |p: &String| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir());
                // Eight names and then how many are left: a list that runs off
                // the panel is one nobody read before pressing yes.
                let mut names: Vec<String> = rows
                    .iter()
                    .take(8)
                    .map(|p| if dir(p) { leaf(p) + "/" } else { leaf(p) })
                    .collect();
                if rows.len() > 8 {
                    names.push(format!("… +{}", rows.len() - 8));
                }
                self.ask_title = format!("{}  —  {}", line.label, names.join(", "));
                if line.id == "delete" {
                    self.ask_title =
                        format!("{}  {}", self.ask_title, self.say("This cannot be undone."));
                    if rows.iter().any(dir) {
                        self.ask_title = format!(
                            "{}  {}",
                            self.ask_title,
                            self.say("Folders go with everything in them.")
                        );
                    }
                }
                self.ask_yes = self
                    .say(match line.id.as_str() {
                        "trash" => "Move",
                        "delete" => "Delete",
                        _ => "Open",
                    })
                    .into_owned();
                self.ask_typing = false;
                self.pending = Some((line.id.clone(), rows));
                self.panel = Panel::Ask;
                // The cursor starts on "no": a terminal cannot dim what is
                // behind a question, so the cursor is what marks the safe answer.
                self.panel_at = 1;
                self.dirty = true;
                Want::Nothing
            }

            _ => {
                self.note = self.say("not in this face yet").into_owned();
                Want::Nothing
            }
        }
    }

    /// Start the program the openers list is on.
    pub fn open_with(&mut self) -> Want {
        let Some((id, _)) = self.openers.get(self.panel_at).cloned() else {
            return Want::Nothing;
        };
        self.panel = Panel::None;
        self.dirty = true;
        // A place to send to rather than a program to open with.
        if let Some(to) = id.strip_prefix("send:") {
            let rows = self.menu_rows();
            let native: Vec<std::path::PathBuf> =
                rows.iter().map(|p| scour_core::path::to_path(p)).collect();
            let paths: Vec<&std::path::Path> =
                native.iter().map(std::path::PathBuf::as_path).collect();
            self.note = match scour_sendto::send(to, &paths) {
                Ok(scour_sendto::Sent::Linked(_)) => {
                    self.say("a link is on the desktop").into_owned()
                }
                Ok(scour_sendto::Sent::Handed) => self.say("handed over").into_owned(),
                Ok(scour_sendto::Sent::Packing(at)) => self
                    .say("packing into {path}")
                    .replace("{path}", &at.to_string_lossy()),
                Ok(scour_sendto::Sent::Copying(at)) => self
                    .say("copying to {path}")
                    .replace("{path}", &at.to_string_lossy()),
                Err(e) => self.say(e.msgid()).into_owned(),
            };
            return Want::Nothing;
        }
        let Some(path) = self.here().map(|h| h.path.clone()) else {
            return Want::Nothing;
        };
        let name = path.rsplit('/').next().unwrap_or(&path).to_string();
        let mime = scour_thumbs::known::known().mime_of(&name).unwrap_or("");
        if let Some(chosen) = scour_openers::openers(mime)
            .into_iter()
            .find(|o| o.id == id)
            && let Err(e) = scour_openers::launch(&chosen, &scour_core::path::to_path(&path))
        {
            self.note = e.to_string();
        }
        Want::Nothing
    }
}
