//! The lens as this window keeps it: which term it is open on, its choices,
//! and the answers it asked the service for. The choices themselves come from
//! `scour_ui::lens`, so the page and the window offer the same ones.

use std::collections::HashMap;

use scour_core::FacetBy;
use scour_proto::{Disk, Request, Response};
use scour_ui::disks::Tile;
use scour_ui::lens::{self, Lens};
use scour_ui::path::home_form;

use crate::qplace;

/// Counted up to this and no further: a choice that keeps more is "100,000+".
const COUNT_CAP: u32 = 100_000;

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
    facets: Option<Vec<(String, u64)>>,
    pub usage: Option<scour_core::UsageResponse>,
    pub tree: Option<Vec<scour_core::TreeNode>>,
    pub disks: Option<Vec<Disk>>,
    pub sources: Option<Vec<scour_core::SourceInfo>>,
    /// The place lens's disks, once `volumes` and `sources` have answered.
    pub tiles: Vec<Tile>,
    pub place: qplace::View,
}

/// What the window knows that a lens shows: words and places.
pub struct Env<'a> {
    pub tr: &'a dyn Fn(&str) -> String,
    pub kind_name: &'a dyn Fn(&str) -> String,
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
}

fn reads(q: &str) -> bool {
    lens::reads(q)
}

impl Lenses {
    pub fn close(&mut self) {
        self.open = None;
        self.generation += 1;
        self.asked.clear();
    }

    /// Open, follow or close the lens for where the caret is. `asked` is a
    /// press on the term or Ctrl+Space; otherwise a lens opens by itself only
    /// at the end of a filter being typed. Returns what to ask the service.
    pub fn sync(&mut self, q: &str, caret: usize, asked: bool, env: &Env) -> Vec<(u32, Request)> {
        let Some(t) = lens::term_at(q, caret) else {
            self.close();
            return Vec::new();
        };
        let Some(kind) = lens::lens_of(q, &t) else {
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
        }
        let sel = match &self.open {
            Some(o) if kind == Lens::Place && o.kind == kind && self.stepped => o.sel,
            Some(o) if keep && o.kind == kind => o.sel,
            _ => 0,
        };
        if !self.seen.is_empty() && self.open.is_none() {
            self.seen.clear();
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
            facets: None,
            usage: None,
            tree: None,
            disks: None,
            sources: None,
            tiles: Vec::new(),
            place: qplace::View::default(),
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
        match o.kind {
            Lens::Kind | Lens::Ext if !reads(&rest) => {
                let key = format!("facets {:?} {rest}", o.kind);
                match self.seen.get(&key) {
                    Some(Response::Facets(f)) => o.facets = Some(facet_pairs(f)),
                    _ => want.push((
                        0,
                        Request::Facets {
                            query: rest.clone(),
                            by: vec![
                                if o.kind == Lens::Kind {
                                    FacetBy::Kind
                                } else {
                                    FacetBy::Ext { top: 24 }
                                },
                                // Lifts the sampling cap a lone kind count has.
                                FacetBy::Age {
                                    edges: scour_ui::bar_edges(),
                                },
                            ],
                        },
                    )),
                }
            }
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
            (Response::Facets(_), _) => {
                self.seen
                    .insert(format!("facets {:?} {rest}", o.kind), reply);
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
        let mut s = o.sel as i32;
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
        let it = o.items.get(i)?.clone();
        if it.wait {
            return None;
        }
        if let Some(k) = it.tile {
            return Some(Chosen::Tile(k));
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

    /// Term produced by dragging across sparkline bars `[a ..= b]`.
    pub fn spark_range(&self, a: usize, b: usize) -> Option<String> {
        let o = self.open.as_ref()?;
        let (lo, hi) = (a.min(b), a.max(b).min(o.items.len().saturating_sub(1)));
        if lo == hi {
            return o.items.get(lo).map(|i| i.term.clone());
        }
        let neg = if o.term.not { "!" } else { "" };
        match o.kind {
            Lens::Time => {
                let edges = ["1d", "2d", "7d", "30d", "365d"];
                Some(format!(
                    "{neg}dm:{}",
                    edges.get(hi).copied().unwrap_or("365d")
                ))
            }
            Lens::Size => {
                let lo_edge = ["", "100kb", "10mb", "1gb"].get(lo).copied().unwrap_or("");
                let hi_edge = ["100kb", "10mb", "1gb", ""].get(hi).copied().unwrap_or("");
                let val = match (lo_edge.is_empty(), hi_edge.is_empty()) {
                    (false, false) => format!("{lo_edge}..{hi_edge}"),
                    (false, true) => format!(">{lo_edge}"),
                    _ => format!("<{hi_edge}"),
                };
                Some(format!("{neg}size:{val}"))
            }
            _ => None,
        }
    }
}

pub fn home_path(home: &str, p: &str) -> String {
    home_form(home, p)
}

fn facet_pairs(f: &scour_core::FacetResponse) -> Vec<(String, u64)> {
    f.groups
        .first()
        .map(|g| g.facets.iter().map(|x| (x.key.clone(), x.count)).collect())
        .unwrap_or_default()
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
        Lens::Time | Lens::Size => {
            let (title, foot, bands) = if o.kind == Lens::Time {
                (
                    "When it last changed",
                    "Or type one: 24h, 2w, 2026-03-01, >2026-06-01",
                    lens::TIMES,
                )
            } else {
                ("Size", "Or type one: >10mb, <1kb, 1mb..50mb", lens::SIZES)
            };
            o.title = tr(title);
            o.foot = tr(foot);
            lens::bands(t, bands)
                .into_iter()
                .map(|c| Item {
                    count: true,
                    ..it(tr(&c.label), &c.hint, c.term)
                })
                .collect()
        }
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
        Lens::Kind | Lens::Ext => {
            let kind = o.kind == Lens::Kind;
            o.title = tr(if kind { "Type" } else { "Extension" });
            o.foot = tr("Tab adds or takes one away and stays open; Enter writes it and closes.");
            let mut values: Vec<(String, Option<u64>)> = o
                .facets
                .clone()
                .unwrap_or_default()
                .into_iter()
                .map(|(k, n)| (k, Some(n)))
                .collect();
            if kind && values.is_empty() {
                values = scour_ui::lens::KINDS
                    .iter()
                    .map(|k| (k.to_string(), None))
                    .collect();
            }
            let pairs: Vec<(String, String)> = values
                .iter()
                .map(|(k, _)| {
                    let label = if kind {
                        (env.kind_name)(k)
                    } else {
                        format!(".{k}")
                    };
                    (k.clone(), label)
                })
                .collect();
            let typed = t.value.rsplit(';').next().unwrap_or("").to_lowercase();
            let known = pairs.iter().any(|(k, _)| *k == typed);
            lens::list(t, &pairs)
                .into_iter()
                .zip(values)
                .filter(|(_, (k, _))| typed.is_empty() || known || k.starts_with(&typed))
                .take(12)
                .map(|(c, (_, n))| Item {
                    n: n.map(|n| n as i64),
                    stays: true,
                    label: format!("{}{}", if c.hint == "✓" { "✓ " } else { "" }, c.label),
                    ..it(String::new(), "", c.term)
                })
                .collect()
        }
        Lens::Place => qplace::items(o, env),
    }
}
