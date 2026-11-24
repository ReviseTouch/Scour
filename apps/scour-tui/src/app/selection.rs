//! The rows picked: one, several, a run.

use super::*;

impl App {
    /// Pick this row and nothing else — what a plain click does everywhere. A
    /// click is a selection, not a cursor move: the deed bar depends on it.
    pub fn pick_only(&mut self, row: usize) -> Want {
        let want = self.go(row);
        self.picked.clear();
        self.anchor = row;
        if let Some(hit) = self.pages.at(row) {
            let bytes = if hit.is_dir { 0 } else { hit.meta.size.max(0) };
            self.picked.insert(hit.path.clone(), bytes);
        }
        self.dirty = true;
        want
    }

    /// Pick or unpick the row under the cursor.
    pub fn pick(&mut self) -> Want {
        let Some(hit) = self.pages.at(self.cursor) else {
            return Want::Nothing;
        };
        let path = hit.path.clone();
        let bytes = if hit.is_dir { 0 } else { hit.meta.size.max(0) };
        if self.picked.remove(&path).is_none() {
            self.picked.insert(path, bytes);
        }
        self.anchor = self.cursor;
        self.dirty = true;
        Want::Nothing
    }

    /// Extend the selection from the anchor to wherever the cursor now is.
    pub fn pick_to(&mut self, row: usize) -> Want {
        let (from, to) = if row < self.anchor {
            (row, self.anchor)
        } else {
            (self.anchor, row)
        };
        for at in from..=to {
            if let Some(hit) = self.pages.at(at) {
                let bytes = if hit.is_dir { 0 } else { hit.meta.size.max(0) };
                self.picked.insert(hit.path.clone(), bytes);
            }
        }
        self.dirty = true;
        self.go(row)
    }

    /// Nothing is picked any more.
    pub fn unpick(&mut self) {
        if !self.picked.is_empty() {
            self.picked.clear();
            self.dirty = true;
        }
    }

    /// Whether a picked path is a folder: the selection keeps sizes, and a
    /// folder's is zero by how it was picked.
    pub(super) fn is_dir_picked(&self, path: &str) -> bool {
        self.picked.get(path).is_some_and(|b| *b == 0)
            && std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
    }
}
