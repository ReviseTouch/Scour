//! Every callback the window has, bound to the state it acts on: a function
//! a section of the window, each handed the same things `main` built.

use super::*;

pub(crate) mod clicks;
pub(crate) mod history;
pub(crate) mod hooks;
pub(crate) mod menu;
pub(crate) mod opening;
pub(crate) mod picking;
pub(crate) mod query;
pub(crate) mod rail;
pub(crate) mod report_tab;
pub(crate) mod resources;
pub(crate) mod service;
pub(crate) mod tab_bar;
pub(crate) mod window_size;
pub(crate) mod words;

/// What `main` built and every section binds its callbacks to, by the names
/// `main` gave them.
#[derive(Clone, Copy)]
pub(crate) struct Wiring<'a> {
    pub window: &'a MainWindow,
    pub config: &'a scour_config::Config,
    pub kept: &'a scour_settings::Settings,
    pub cat: &'a Rc<RefCell<Rc<Catalogue>>>,
    pub tabs: &'a Rc<RefCell<tabs::Tabs>>,
    pub tabs_later: &'a Rc<slint::Timer>,
    pub state: &'a Rc<RefCell<State>>,
    pub weighed: &'a Rc<RefCell<report::Kept>>,
    pub menu_path: &'a Rc<RefCell<Option<String>>>,
    pub rows: &'a Rc<rows::Rows>,
    pub picks: &'a Rc<RefCell<Selection>>,
    pub lines: &'a Rc<rows::Lines>,
    pub facets: &'a Rc<VecModel<Facet>>,
    pub rules: &'a [Rc<VecModel<Rule>>; 3],
    pub slots: &'a Rc<rows::Slots>,
    pub table: &'a Rc<RefCell<Table>>,
    pub folders: &'a Rc<RefCell<Vec<String>>>,
    pub folders_off: &'a Rc<RefCell<Vec<String>>>,
    pub folders_unwatched: &'a Rc<RefCell<Vec<String>>>,
    pub addr: &'a String,
    pub link: &'a Rc<Link>,
    pub opened_at: &'a (f32, f32),
}
