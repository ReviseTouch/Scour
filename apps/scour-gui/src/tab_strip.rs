//! The search tabs: drawn, switched, kept.

use super::*;
use link::{Ask, Link};
use scour_i18n::Catalogue;
use slint::{ModelRc, VecModel};
use std::cell::RefCell;
use std::rc::Rc;

/// The tab in front as the window has it now: what is typed, the order, and
/// the content line's pattern while the line is open.
pub(super) fn now_tab(s: &State) -> scour_settings::SearchTab {
    scour_settings::SearchTab {
        query: s.query.clone(),
        sort: s.sort.clone(),
        descending: Some(s.descending),
        content: if s.grep.open {
            s.grep.pattern.clone()
        } else {
            String::new()
        },
    }
}

/// The strip: every tab's name and which is in front. A tab ordered other than
/// newest first says by what, and which way: "rapor · Boyut ▾".
pub(super) fn draw_tabs(w: &MainWindow, tabs: &tabs::Tabs, cat: &Catalogue) {
    let untitled = t(cat, "New tab");
    let labels: Vec<slint::SharedString> = tabs
        .labels(&untitled)
        .into_iter()
        .zip(tabs.list())
        .map(|(name, tab)| {
            let key = scour_core::SortKey::named(&tab.sort).unwrap_or_default();
            let down = tab.descending.unwrap_or_else(|| key.starts_descending());
            if key == scour_core::SortKey::Modified && down {
                return name.into();
            }
            let column = scour_ui::COLUMNS
                .iter()
                .find(|c| c.sort == key.name())
                .map_or_else(|| key.name().to_owned(), |c| t(cat, c.msgid).to_string());
            format!("{name} · {column} {}", if down { "▾" } else { "▴" }).into()
        })
        .collect();
    w.set_tab_labels(ModelRc::new(VecModel::from(labels)));
    w.set_tab_active(i32::try_from(tabs.active()).unwrap_or(0));
}

/// Bring a tab's query, order and content line onto the window, and search
/// for it.
pub(super) fn show_tab(
    w: &MainWindow,
    state: &Rc<RefCell<State>>,
    link: &Link,
    cat: &Catalogue,
    tab: &scour_settings::SearchTab,
) {
    let key = scour_core::SortKey::named(&tab.sort).unwrap_or_default();
    let descending = tab.descending.unwrap_or_else(|| key.starts_descending());
    {
        let mut s = state.borrow_mut();
        s.sort = key.name().to_owned();
        s.descending = descending;
    }
    w.set_sorted_by(key.name().into());
    w.set_sorted_down(descending);
    // Before the query: its change holds the tab, and reads the line again
    // when the line is open.
    let open = put_content(w, &mut state.borrow_mut(), &tab.content);
    if !open {
        grep_ui::trigger(w, &mut state.borrow_mut(), link, cat, false);
    }
    w.set_query(tab.query.as_str().into());
    w.invoke_query_changed(tab.query.as_str().into());
    w.invoke_focus_query();
}

/// A tab's content pattern into the line: open with it, or closed and empty.
/// True when the line is open.
pub(super) fn put_content(w: &MainWindow, s: &mut State, pattern: &str) -> bool {
    let open = !pattern.is_empty();
    crate::grep_field::load(w, s, pattern);
    s.grep.open = open;
    w.set_grep_open(open);
    open
}

/// The tabs, for every face: replaced whole, as `Change::tabs` says.
pub(super) fn keep_tabs(link: &Link, tabs: &tabs::Tabs) {
    link.send(Ask::Remember {
        change: scour_settings::Change {
            tabs: Some(tabs.list().to_vec()),
            tab: Some(tabs.active()),
            ..Default::default()
        },
    });
}
