//! The search tabs: each its own query and order. The list lives here, the
//! window draws it, and the service keeps it as `Settings::tabs`, so a tab
//! opened in the window is there in the page too.

use scour_settings::SearchTab;

pub struct Tabs {
    list: Vec<SearchTab>,
    active: usize,
}

/// What a new tab starts as: nothing typed, newest first, no content line.
pub fn fresh() -> SearchTab {
    SearchTab {
        query: String::new(),
        sort: scour_core::SortKey::Modified.name().into(),
        descending: Some(true),
        content: String::new(),
    }
}

impl Tabs {
    /// The tabs the settings hold, or one tab of what is on screen when they
    /// hold none.
    pub fn kept(list: &[SearchTab], active: usize, now: SearchTab) -> Tabs {
        if list.is_empty() {
            return Tabs {
                list: vec![now],
                active: 0,
            };
        }
        Tabs {
            list: list.to_vec(),
            active: active.min(list.len() - 1),
        }
    }

    pub fn active(&self) -> usize {
        self.active
    }

    pub fn len(&self) -> usize {
        self.list.len()
    }

    pub fn list(&self) -> &[SearchTab] {
        &self.list
    }

    pub fn front(&self) -> &SearchTab {
        &self.list[self.active]
    }

    /// Write what is on screen into the tab in front, before leaving it.
    pub fn hold(&mut self, now: SearchTab) {
        self.list[self.active] = now;
    }

    /// A new tab after the last, brought to the front.
    pub fn open(&mut self, now: SearchTab) -> SearchTab {
        self.hold(now);
        if self.list.len() >= scour_settings::TABS {
            return self.front().clone();
        }
        self.list.push(fresh());
        self.active = self.list.len() - 1;
        self.front().clone()
    }

    /// Bring a tab to the front. `None` when it already is, or is not there.
    pub fn switch(&mut self, to: usize, now: SearchTab) -> Option<SearchTab> {
        if to == self.active || to >= self.list.len() {
            return None;
        }
        self.hold(now);
        self.active = to;
        Some(self.front().clone())
    }

    /// The next tab along, or the previous, wrapping round.
    pub fn step(&mut self, by: isize, now: SearchTab) -> Option<SearchTab> {
        let n = self.list.len() as isize;
        let to = (self.active as isize + by).rem_euclid(n) as usize;
        self.switch(to, now)
    }

    /// Close a tab. The last one stays: there is always a query line. Returns
    /// the tab now in front when that changed.
    pub fn close(&mut self, at: usize, now: SearchTab) -> Option<SearchTab> {
        if self.list.len() <= 1 || at >= self.list.len() {
            return None;
        }
        self.hold(now);
        self.list.remove(at);
        let was = self.active;
        // The one to the right takes the closed one's place, as a browser does;
        // closing one to the left moves the front along with its index.
        if at < was || was >= self.list.len() {
            self.active = was.saturating_sub(1).min(self.list.len() - 1);
        }
        (at == was).then(|| self.front().clone())
    }

    /// What each tab is called on its strip: its query, or `untitled`.
    pub fn labels(&self, untitled: &str) -> Vec<String> {
        self.list
            .iter()
            .map(|t| {
                let q = t.query.trim();
                if q.is_empty() {
                    untitled.to_owned()
                } else {
                    q.to_owned()
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tab(q: &str) -> SearchTab {
        SearchTab {
            query: q.into(),
            ..fresh()
        }
    }

    fn queries(t: &Tabs) -> Vec<&str> {
        t.list().iter().map(|t| t.query.as_str()).collect()
    }

    #[test]
    fn none_kept_is_one_tab_of_what_is_on_screen() {
        let t = Tabs::kept(&[], 4, tab("pdf"));
        assert_eq!((t.len(), t.active()), (1, 0));
        assert_eq!(t.front().query, "pdf");
        // An index past the end is the last tab.
        let t = Tabs::kept(&[tab("a"), tab("b")], 9, tab(""));
        assert_eq!(t.active(), 1);
    }

    #[test]
    fn opening_keeps_what_was_typed_and_starts_empty() {
        let mut t = Tabs::kept(&[], 0, tab(""));
        let new = t.open(tab("rapor"));
        assert_eq!(new, fresh());
        assert_eq!(queries(&t), ["rapor", ""]);
        assert_eq!(t.active(), 1);
    }

    #[test]
    fn a_new_tab_has_no_content_line_and_the_old_one_keeps_its_own() {
        let reading = SearchTab {
            content: "~rapor".into(),
            ..tab("ext:pdf")
        };
        let mut t = Tabs::kept(&[], 0, tab(""));
        let new = t.open(reading.clone());
        assert_eq!(new.content, "");
        let back = t.switch(0, new).expect("switched");
        assert_eq!(back, reading);
    }

    #[test]
    fn switching_holds_the_tab_left_and_loads_the_other() {
        let mut t = Tabs::kept(&[tab("a"), tab("b")], 0, tab(""));
        assert_eq!(t.switch(0, tab("a2")), None, "already in front");
        let got = t.switch(1, tab("a2")).expect("switched");
        assert_eq!(got.query, "b");
        assert_eq!(queries(&t), ["a2", "b"]);
        // Wrapping both ways.
        assert_eq!(t.step(1, tab("b")).map(|t| t.query), Some("a2".into()));
        assert_eq!(t.step(-1, tab("a2")).map(|t| t.query), Some("b".into()));
    }

    #[test]
    fn closing_moves_the_front_the_way_a_browser_does() {
        let mut t = Tabs::kept(&[tab("a"), tab("b"), tab("c")], 1, tab("b"));
        // The one in front: its right-hand neighbour comes forward.
        assert_eq!(t.close(1, tab("b")).map(|t| t.query), Some("c".into()));
        assert_eq!((queries(&t), t.active()), (vec!["a", "c"], 1));
        // One to the left: the front stays the same tab.
        assert_eq!(t.close(0, tab("c")), None);
        assert_eq!((queries(&t), t.active()), (vec!["c"], 0));
        // The last one stays.
        assert_eq!(t.close(0, tab("c")), None);
        assert_eq!(t.len(), 1);
    }

    #[test]
    fn closing_the_last_tab_in_front_brings_the_one_before() {
        let mut t = Tabs::kept(&[tab("a"), tab("b")], 1, tab("b"));
        assert_eq!(t.close(1, tab("b")).map(|t| t.query), Some("a".into()));
        assert_eq!(t.active(), 0);
    }

    #[test]
    fn an_empty_tab_is_named_and_a_query_is_its_name() {
        let t = Tabs::kept(&[tab("  ext:pdf "), tab("")], 0, tab(""));
        assert_eq!(t.labels("New tab"), ["ext:pdf", "New tab"]);
    }
}
