//! Columns shown and the rules that skip folders, switched from the list.

use super::*;

impl App {
    /// The rules arrived. Kept as the answer gave them.
    pub fn ruled(
        &mut self,
        added: Vec<(String, String)>,
        config: Vec<(String, String)>,
        builtin: Vec<(String, String)>,
        off: Vec<String>,
    ) {
        let is_off = |id: &str| off.iter().any(|o| o.eq_ignore_ascii_case(id));
        let mut out = Vec::new();
        for (group, removable) in [(added, true), (config, false), (builtin, false)] {
            for (kind, value) in group {
                let id = scour_settings::rule_id(&kind, &value);
                let off = is_off(&id);
                out.push((id, value, off, removable));
            }
        }
        self.rules = out;
        self.dirty = true;
    }

    /// Switch the rule under the panel's cursor off, or back on. Returns the
    /// whole switched-off list: the service replaces it outright.
    pub fn toggle_rule(&mut self) -> Option<Vec<String>> {
        let (_, _, off, _) = self.rules.get_mut(self.panel_at)?;
        *off = !*off;
        self.dirty = true;
        Some(
            self.rules
                .iter()
                .filter(|(_, _, off, _)| *off)
                .map(|(id, _, _, _)| id.clone())
                .collect(),
        )
    }

    /// Open a panel, or close the one that is open.
    pub fn show(&mut self, panel: Panel) {
        self.panel = if self.panel == panel {
            Panel::None
        } else {
            panel
        };
        self.panel_at = 0;
        self.note.clear();
        self.dirty = true;
    }

    /// Does this path sit on a volume that has stopped recording reads? The
    /// deepest mount wins, and nothing known answers no.
    pub fn frozen_atime(&self, path: &str) -> bool {
        let mut owner: Option<&scour_places::Mount> = None;
        for m in &self.mounts {
            let under = m.at == "/" || format!("{path}/").starts_with(&format!("{}/", m.at));
            if under && owner.is_none_or(|o| m.at.len() > o.at.len()) {
                owner = Some(m);
            }
        }
        owner.is_some_and(|m| !m.reads)
    }

    /// The line the cursor is on in the column panel: switch it, and keep the
    /// panel open — turning three columns on is three presses.
    pub fn pick_column(&mut self) -> Want {
        match scour_ui::COLUMNS.get(self.panel_at) {
            Some(c) => {
                let id = c.id;
                self.toggle_column(id);
            }
            // Past the end is the line that puts them all back.
            None => {
                self.columns = scour_ui::DEFAULT_COLUMNS
                    .iter()
                    .filter_map(|id| scour_ui::column(id))
                    .collect();
            }
        }
        Want::Remember(scour_settings::Change {
            columns: Some(self.columns.iter().map(|c| c.id.to_owned()).collect()),
            ..Default::default()
        })
    }

    /// Take the saved column list, dropping anything this build does not have.
    /// An empty result leaves the default: a table of no columns is not a table.
    pub fn columns_from(&mut self, saved: &[String]) {
        let shown: Vec<&'static scour_ui::Column> =
            saved.iter().filter_map(|id| scour_ui::column(id)).collect();
        if !shown.is_empty() {
            self.columns = shown;
        }
    }

    /// Switch one column on or off, putting it back where its neighbours expect
    /// it without moving them: rebuilding in table order loses an arrangement.
    pub fn toggle_column(&mut self, id: &str) {
        let showing = self.columns.iter().any(|c| c.id == id);
        if showing && self.columns.len() == 1 {
            return;
        }
        if let Some(at) = self.columns.iter().position(|c| c.id == id) {
            self.columns.remove(at);
            return;
        }
        let Some(col) = scour_ui::column(id) else {
            return;
        };
        let rank = |x: &str| scour_ui::COLUMNS.iter().position(|c| c.id == x);
        let at = self
            .columns
            .iter()
            .position(|c| rank(c.id) > rank(col.id))
            .unwrap_or(self.columns.len());
        self.columns.insert(at, col);
    }
}
