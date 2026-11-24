//! The report: a folder weighed, its kinds, walking into it and back up.

use super::*;

impl App {
    /// What a selection comes to: how many, how many of them folders, and the
    /// bytes of the files among them.
    pub fn weighed(&self) -> (usize, usize, u64) {
        let folders = self.picked.values().filter(|b| **b == 0).count();
        let bytes = self.picked.values().map(|b| *b as u64).sum();
        (self.picked.len(), folders, bytes)
    }

    /// Show the report, or go back to the list. Asked for when it opens rather
    /// than kept fresh: finding duplicates reads files.
    pub fn report(&mut self) -> Want {
        self.reporting = !self.reporting;
        self.dirty = true;
        if !self.reporting {
            return Want::Nothing;
        }
        // It opens on the home directory: weighing everything indexed answers
        // with one child, the filesystem root. `Backspace` still walks out.
        if self.usage.is_none() {
            self.weighing = std::env::var("HOME").unwrap_or_default();
        }
        Want::Report(self.scope())
    }

    /// The folder the report is about: the one being weighed, or the home
    /// directory before anything has been. Asked for by name rather than
    /// assumed, or a report reopened deeper answers about the home again.
    pub fn scope(&self) -> String {
        match self.weighing.is_empty() && self.usage.is_none() {
            true => std::env::var("HOME").unwrap_or_default(),
            false => self.weighing.clone(),
        }
    }

    /// Open or close the peek, and ask for what it shows.
    pub fn peek(&mut self) -> Want {
        self.peeking = !self.peeking;
        self.peek = None;
        self.dirty = true;
        if !self.peeking {
            return Want::Nothing;
        }
        match self.here() {
            Some(hit) if !hit.is_dir => Want::Peek(hit.path.clone()),
            _ => {
                self.peeking = false;
                Want::Nothing
            }
        }
    }

    /// The cursor moved while the peek is open: it follows.
    pub fn repeek(&mut self) -> Want {
        if !self.peeking {
            return Want::Nothing;
        }
        self.peek = None;
        match self.here() {
            Some(hit) if !hit.is_dir => Want::Peek(hit.path.clone()),
            _ => Want::Nothing,
        }
    }

    /// Weigh a folder — a child of the one being weighed, or its parent.
    pub fn weigh(&mut self, path: String) -> Want {
        self.weighing = path.clone();
        self.usage = None;
        self.weigh_at = 0;
        // The kinds belong to the folder being weighed: kept, they would say
        // the folder above's answer about this one.
        self.scope_kinds.clear();
        self.dirty = true;
        Want::Weigh(path)
    }

    /// What the report's scope is made of, by kind. Off its own question, not
    /// the rail's: the rail counts the search, this counts the folder.
    pub fn scope_counted(&mut self, kinds: Vec<(String, u64)>) {
        self.scope_kinds = kinds;
        self.dirty = true;
    }

    /// Move the cursor over the weighed children.
    pub fn weigh_walk(&mut self, by: isize) {
        let count = self
            .usage
            .as_ref()
            .map(|u| u.children.len().min(WEIGHED))
            .unwrap_or(0);
        if count == 0 {
            return;
        }
        self.weigh_at = self
            .weigh_at
            .saturating_add_signed(by)
            .min(count.saturating_sub(1));
        self.dirty = true;
    }

    /// Go into the child under the cursor.
    pub fn weigh_into(&mut self) -> Want {
        let Some(usage) = &self.usage else {
            return Want::Nothing;
        };
        let Some(child) = usage.children.get(self.weigh_at) else {
            return Want::Nothing;
        };
        let path = child.path.clone();
        self.weigh(path)
    }

    /// Back out to the folder above.
    pub fn weigh_up(&mut self) -> Want {
        if self.weighing.is_empty() {
            return Want::Nothing;
        }
        let up = scour_ui::path::folder(&self.weighing).to_string();
        // `/` has no parent that means anything here; the whole index does.
        let up = if up == "/" { String::new() } else { up };
        self.weigh(up)
    }
}
