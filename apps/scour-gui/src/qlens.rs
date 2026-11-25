//! The lens as this window keeps it: which term it is open on, its choices,
//! and the answers it asked the service for. The choices themselves come from
//! `scour_ui::lens`, so the page and the window offer the same ones.

use std::collections::HashMap;

use scour_proto::{Disk, Request, Response};
use scour_ui::disks::Tile;
use scour_ui::lens::{self, Lens};
use scour_ui::path::home_form;

use crate::qpanel::{COUNT_CAP, Want};
use crate::{qplace, qsize, qtime, qtype};

/// One line of the open lens.
#[derive(Debug, Clone, Default)]
pub struct Item {
    pub label: String,
    pub hint: String,
    pub term: String,
    /// What it would leave; negative is a floor.
    pub n: Option<i64>,
    pub count: bool,
    pub reads: bool,
    pub stays: bool,
    /// A folder, which Tab goes into.
    pub dir: Option<String>,
    pub group: String,
    pub wait: bool,
    pub launch: bool,
    /// A disk tile of the place lens, by its index in `Open::tiles`.
    pub tile: Option<usize>,
    /// A launcher line: its field, its capsule's picture, the letters that
    /// pick it, and whether the query already holds one.
    pub field: &'static str,
    pub icon: &'static str,
    pub key: String,
    pub held: bool,
    /// A kind or an extension the type panel toggles: its field and value.
    pub value: Option<(&'static str, String)>,
}

pub struct Open {
    pub kind: Lens,
    pub a: usize,
    pub b: usize,
    pub term: lens::Term,
    raw: String,
    pub sel: usize,
    pub items: Vec<Item>,
    pub title: String,
    pub foot: String,
    pub usage: Option<scour_core::UsageResponse>,
    pub tree: Option<Vec<scour_core::TreeNode>>,
    pub disks: Option<Vec<Disk>>,
    pub sources: Option<Vec<scour_core::SourceInfo>>,
    /// The place lens's disks, once `volumes` and `sources` have answered.
    pub tiles: Vec<Tile>,
    pub place: qplace::View,
    /// The time, type and size panels' answers.
    pub got: crate::qpanel::Got,
}

/// What the window knows that a lens shows: words and places.
pub struct Env<'a> {
    pub tr: &'a dyn Fn(&str) -> String,
    pub home: &'a str,
    pub places: &'a [(String, String)],
}

/// What choosing did.
pub enum Chosen {
    /// The query is now this, the caret here; the lens stays open if `keep`.
    Edit {
        query: String,
        caret: usize,
        keep: bool,
    },
    /// A disk tile, by its index in `Open::tiles`: Enter on it is a plain click.
    Tile(usize),
    /// What the files hold: not a filter in the query but the line under the
    /// field. The query is this — the `:` that asked taken out — the caret here.
    Inside { query: String, caret: usize },
    /// A kind or extension of the type panel, toggled; the lens stays open if `keep`.
    Value {
        field: &'static str,
        value: String,
        keep: bool,
    },
}

#[derive(Default)]
pub struct Lenses {
    pub open: Option<Open>,
    pub generation: u64,
    seen: HashMap<String, Response>,
    /// What this generation has asked and not yet heard: an answer refills
    /// the lens, which must not ask a second time for what is on its way.
    asked: std::collections::HashSet<String>,
    /// The folder the place lens shows while the query is still this text:
    /// set by a click that wrote the query, so the list stays where it was.
    pub browse: Option<(String, String)>,
    /// The keys have moved the place lens's line since anything was typed:
    /// it stays where they put it. Until then it rests on the line Enter
    /// would most likely mean.
    stepped: bool,
    /// The next opening is the lens's own press rewriting the query, not typing.
    pub hold: bool,
    /// The time panel's scale and mode, kept while it stays open.
    pub look: qtime::Look,
    /// The time term the calendar last lit, `None` on a fresh opening.
    pub cal_for: Option<String>,
    /// What the panels asked, by generation and slot: the key its answer is
    /// kept under, so an answer outliving its generation still counts.
    pending: HashMap<(u64, u32), String>,
}

fn reads(q: &str) -> bool {
    lens::reads(q)
}

impl Lenses {
    pub fn close(&mut self) {
        self.open = None;
        self.generation += 1;
        self.asked.clear();
        self.pending.clear();
        self.cal_for = None;
    }

    /// Ask again for what the open lens shows, as it now is.
    pub fn refill(&mut self, q: &str, env: &Env) -> Vec<(u32, Request)> {
        self.fill(q, env)
    }

    /// Open, follow or close the lens for where the caret is. `asked` is a
    /// press on the term or Ctrl+Space; otherwise a lens opens by itself only
    /// at the end of a filter being typed. Returns what to ask the service.
    pub fn sync(&mut self, q: &str, caret: usize, asked: bool, env: &Env) -> Vec<(u32, Request)> {
        let Some(t) = lens::term_at(q, caret) else {
            self.close();
            return Vec::new();
        };
        // `empty:` names no field the model knows; it is the size panel's.
        let Some(kind) =
            lens::lens_of(q, &t).or_else(|| lens::is_empty_term(q, &t).then_some(Lens::Size))
        else {
            self.close();
            return Vec::new();
        };
        let keep = self.open.as_ref().is_some_and(|o| o.a == t.start);
        let by_itself = lens::opens_itself(kind) && caret == t.end;
        if !(asked || keep || by_itself) {
            self.close();
            return Vec::new();
        }
        // The place lens keeps the line the keys chose across the terms its
        // own presses write; typing puts it back on its resting line.
        let typed = !std::mem::take(&mut self.hold)
            && self
                .open
                .as_ref()
                .is_none_or(|o| o.raw != q[t.start..t.end]);
        if typed || self.open.is_none() {
            self.stepped = false;
            self.look.armed = false;
        }
        let sel = match &self.open {
            Some(o) if kind == Lens::Place && o.kind == kind && self.stepped => o.sel,
            Some(o) if keep && o.kind == kind => o.sel,
            _ => 0,
        };
        if !self.seen.is_empty() && self.open.is_none() {
            self.seen.clear();
        }
        if kind == Lens::Time && self.open.as_ref().is_none_or(|o| o.kind != kind) {
            self.look = qtime::Look::of(&t.value);
        }
        self.open = Some(Open {
            kind,
            a: t.start,
            b: t.end,
            raw: q[t.start..t.end].to_string(),
            term: t,
            sel,
            items: Vec::new(),
            title: String::new(),
            foot: String::new(),
            usage: None,
            tree: None,
            disks: None,
            sources: None,
            tiles: Vec::new(),
            place: qplace::View::default(),
            got: Default::default(),
        });
        self.generation += 1;
        self.asked.clear();
        self.fill(q, env)
    }

    /// The query less the term the lens is on.
    fn rest(q: &str, o: &Open) -> String {
        let s = format!("{}{}", &q[..o.a], &q[o.b..]);
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn fill(&mut self, q: &str, env: &Env) -> Vec<(u32, Request)> {
        let Some(o) = self.open.as_mut() else {
            return Vec::new();
        };
        let rest = Self::rest(q, o);
        let mut want = Vec::new();
        let mut panel: Vec<Want> = Vec::new();
        match o.kind {
            Lens::Time => panel = qtime::fill(o, &self.seen, q, &self.look),
            Lens::Size => panel = qsize::fill(o, &self.seen, q),
            Lens::Kind | Lens::Ext => panel = qtype::fill(o, &self.seen, q),
            Lens::Place => {
                let pinned = qplace::pinned(&self.browse, q);
                want = qplace::fill(o, &self.seen, q, pinned, env);
            }
            _ => {}
        }
        o.items = items(o, q, env);
        if o.kind == Lens::Place && !self.stepped {
            o.sel = qplace::resting(o);
        }
        // A preset panel rests on the preset the term is, else on none:
        // Enter then keeps what is written.
        if matches!(o.kind, Lens::Time | Lens::Size) && !self.stepped {
            let chosen = match o.kind {
                Lens::Time => scour_ui::lens::time_pick(&o.term.value).and_then(|p| p.preset),
                _ => qsize::chosen(o, q),
            };
            o.sel = chosen.unwrap_or(o.items.len());
        }
        // `ext:` opens the type panel on its extensions.
        if o.kind == Lens::Ext && !self.stepped && o.sel == 0 {
            o.sel = scour_ui::lens::KIND_LABELS
                .len()
                .min(o.items.len().saturating_sub(1));
        }
        for (slot, key, request) in panel {
            if self.asked.insert(key.clone()) {
                self.pending.insert((self.generation, slot), key);
                want.push((slot, request));
            }
        }
        if !reads(&rest) {
            for (i, it) in o.items.iter_mut().enumerate() {
                if !it.count {
                    continue;
                }
                let whole = if rest.is_empty() {
                    it.term.clone()
                } else {
                    format!("{rest} {}", it.term)
                };
                match self.seen.get(&format!("count {whole}")) {
                    Some(Response::Count { total, capped, .. }) => {
                        it.n = Some(if *capped {
                            -(*total as i64)
                        } else {
                            *total as i64
                        });
                    }
                    _ => want.push((
                        100 + i as u32,
                        Request::Count {
                            query: whole,
                            cap: COUNT_CAP,
                        },
                    )),
                }
            }
        }
        want.retain(|(slot, r)| self.asked.insert(format!("{slot} {r:?}")));
        want
    }

    /// An answer to something this lens asked. `true` when it changed what is shown.
    pub fn answer(
        &mut self,
        generation: u64,
        slot: u32,
        reply: Response,
        q: &str,
        env: &Env,
    ) -> Vec<(u32, Request)> {
        if let Some(key) = self.pending.remove(&(generation, slot)) {
            self.seen.insert(key, reply);
            return if self.open.is_some() {
                self.fill(q, env)
            } else {
                Vec::new()
            };
        }
        if generation != self.generation {
            return Vec::new();
        }
        let Some(o) = self.open.as_mut() else {
            return Vec::new();
        };
        let rest = Self::rest(q, o);
        if o.kind == Lens::Place {
            let pinned = qplace::pinned(&self.browse, q);
            let Some(key) = qplace::key(o, slot, &reply, q, pinned, env) else {
                return Vec::new();
            };
            self.seen.insert(key, reply);
            return self.fill(q, env);
        }
        match (&reply, slot) {
            (Response::Count { .. }, n) if n >= 100 => {
                let Some(it) = o.items.get((n - 100) as usize) else {
                    return Vec::new();
                };
                let whole = if rest.is_empty() {
                    it.term.clone()
                } else {
                    format!("{rest} {}", it.term)
                };
                self.seen.insert(format!("count {whole}"), reply);
            }
            _ => return Vec::new(),
        }
        self.fill(q, env)
    }

    pub fn step(&mut self, by: i32) {
        let Some(o) = self.open.as_mut() else { return };
        self.stepped = true;
        let n = o.items.len() as i32;
        if n == 0 {
            return;
        }
        // From no line, the first step lands on an end.
        let mut s = if o.sel as i32 >= n {
            if by > 0 { -1 } else { n }
        } else {
            o.sel as i32
        };
        for _ in 0..n {
            s = (s + by).rem_euclid(n);
            if !o.items[s as usize].wait {
                break;
            }
        }
        o.sel = s as usize;
    }

    /// Write a choice into the query. `stay` (Tab) keeps the lens open: a
    /// list takes another value, a folder is gone into.
    pub fn choose(&mut self, i: usize, stay: bool, q: &str) -> Option<Chosen> {
        let o = self.open.as_mut()?;
        let Some(it) = o.items.get(i).cloned() else {
            // Enter on a preset panel resting on none: what is written stays.
            if !matches!(o.kind, Lens::Time | Lens::Size) {
                return None;
            }
            let caret = o.b;
            self.close();
            return Some(Chosen::Edit {
                query: q.to_string(),
                caret,
                keep: false,
            });
        };
        if it.wait {
            return None;
        }
        if let Some(k) = it.tile {
            return Some(Chosen::Tile(k));
        }
        if let Some((field, value)) = it.value {
            if !stay {
                self.close();
            }
            return Some(Chosen::Value {
                field,
                value,
                keep: stay,
            });
        }
        // A filter the query already holds is gone into, not written twice.
        if it.launch
            && it.held
            && let Some((query, caret)) = lens::into_held(q, &o.term, it.field)
        {
            self.close();
            return Some(Chosen::Edit {
                query,
                caret,
                keep: true,
            });
        }
        let mut text = it.term.clone();
        if it.launch && text == "content:" {
            // The `:` alone goes; the space before it stays, so the caret is
            // left outside the term in front.
            let query = format!("{}{}", &q[..o.a], &q[o.b..]);
            let caret = o.a;
            self.close();
            return Some(Chosen::Inside { query, caret });
        }
        let deeper = stay && it.dir.is_some();
        let quoted = text.ends_with('"');
        if deeper {
            if quoted {
                text.insert(text.len() - 1, '/');
            } else {
                text.push('/');
            }
        }
        let keep = it.launch || (stay && (it.stays || deeper));
        let tail = if !keep && !q[o.b..].starts_with(char::is_whitespace) {
            " "
        } else {
            ""
        };
        let query = format!("{}{text}{tail}{}", &q[..o.a], &q[o.b..]);
        let caret = o.a + text.len() - usize::from(deeper && quoted) + tail.len();
        if it.launch || deeper {
            o.sel = 0;
        }
        if !keep {
            self.close();
        }
        Some(Chosen::Edit { query, caret, keep })
    }

    /// The chosen line goes deeper or takes another value: a folder, or a list.
    pub fn deepens(&self) -> bool {
        self.open
            .as_ref()
            .and_then(|o| o.items.get(o.sel))
            .is_some_and(|i| i.stays || i.dir.is_some())
    }

    /// Ask again next time: what was answered no longer holds.
    pub fn forget(&mut self, key: &str) {
        self.seen.remove(key);
    }
}

pub fn home_path(home: &str, p: &str) -> String {
    home_form(home, p)
}

fn it(label: String, hint: &str, term: String) -> Item {
    Item {
        label,
        hint: hint.to_string(),
        term,
        ..Default::default()
    }
}

fn items(o: &mut Open, q: &str, env: &Env) -> Vec<Item> {
    let tr = env.tr;
    let t = &o.term;
    match o.kind {
        Lens::Launch => {
            o.title = tr("Add a filter");
            o.foot = tr("Type a few letters to narrow it. Enter starts the filter.");
            let mut group = None;
            lens::launch(q, &o.raw, |m| tr(m))
                .into_iter()
                .map(|s| {
                    let heading = if group == Some(s.group) {
                        String::new()
                    } else {
                        tr(s.group.heading())
                    };
                    group = Some(s.group);
                    Item {
                        launch: true,
                        group: heading,
                        field: s.field,
                        icon: s.icon,
                        key: s.key,
                        held: s.held,
                        ..it(tr(&s.choice.label), &tr(&s.choice.hint), s.choice.term)
                    }
                })
                .collect()
        }
        Lens::Time => qtime::items(o, env),
        Lens::Size => qsize::items(o, env),
        Lens::Word => {
            o.title = tr("How “{w}” is read").replace("{w}", &t.value);
            o.foot = tr("Only this word changes. Every word at once: Settings → Searching.");
            lens::word(t)
                .into_iter()
                .map(|c| {
                    let reads = c.hint == "content:";
                    Item {
                        count: !reads,
                        reads,
                        ..it(tr(&c.label), &c.hint, c.term)
                    }
                })
                .collect()
        }
        Lens::Content => {
            o.title = tr("Inside the files");
            o.foot = tr(
                "Only the files the rest of the query names are read: narrow the place and the type first.",
            );
            lens::content(t)
                .into_iter()
                .map(|c| {
                    let reads = c.hint == "content:";
                    Item {
                        count: !reads,
                        reads,
                        ..it(tr(&c.label), &c.hint, c.term)
                    }
                })
                .collect()
        }
        Lens::Kind | Lens::Ext => qtype::items(o, q, env),
        Lens::Place => qplace::items(o, env),
    }
}
