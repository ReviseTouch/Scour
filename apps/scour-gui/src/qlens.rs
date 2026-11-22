//! The lens as this window keeps it: which term it is open on, its choices,
//! and the answers it asked the service for. The choices themselves come from
//! `scour_ui::lens`, so the page and the window offer the same ones.

use std::collections::HashMap;

use scour_core::FacetBy;
use scour_proto::{Disk, Request, Response};
use scour_ui::lens::{self, Lens};

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
    /// A disk not indexed: choosing it asks to index it.
    pub disk: Option<String>,
}

pub struct Open {
    pub kind: Lens,
    pub a: usize,
    pub b: usize,
    term: lens::Term,
    raw: String,
    pub sel: usize,
    pub items: Vec<Item>,
    pub title: String,
    pub foot: String,
    facets: Option<Vec<(String, u64)>>,
    usage: Option<scour_core::UsageResponse>,
    disks: Option<Vec<Disk>>,
}

/// What the window knows that a lens shows: words and places.
pub struct Env<'a> {
    pub tr: &'a dyn Fn(&str) -> String,
    pub kind_name: &'a dyn Fn(&str) -> String,
    pub home: &'a str,
    pub places: &'a [(String, String)],
    pub grouped: &'a dyn Fn(u64) -> String,
    pub bytes: &'a dyn Fn(u64) -> String,
}

/// What choosing did.
pub enum Chosen {
    /// The query is now this, the caret here; the lens stays open if `keep`.
    Edit {
        query: String,
        caret: usize,
        keep: bool,
    },
    /// Index this disk.
    Disk(String),
}

#[derive(Default)]
pub struct Lenses {
    pub open: Option<Open>,
    pub generation: u64,
    seen: HashMap<String, Response>,
}

fn reads(q: &str) -> bool {
    lens::terms(q).iter().any(|t| t.field == Some("content"))
}

fn home_form(home: &str, p: &str) -> String {
    match p.strip_prefix(home) {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("~{rest}")
        }
        _ => p.to_string(),
    }
}

fn full_form(home: &str, p: &str) -> String {
    match p.strip_prefix('~') {
        Some(rest) if !home.is_empty() && (rest.is_empty() || rest.starts_with('/')) => {
            format!("{home}{rest}")
        }
        _ => p.to_string(),
    }
}

impl Lenses {
    pub fn close(&mut self) {
        self.open = None;
        self.generation += 1;
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
        let sel = match &self.open {
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
            disks: None,
        });
        self.generation += 1;
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
                let (base, _) = lens::place_parts(&o.term.value);
                if base.is_empty() {
                    match self.seen.get("volumes") {
                        Some(Response::Volumes { disks, .. }) => o.disks = Some(disks.clone()),
                        _ => want.push((0, Request::Volumes {})),
                    }
                } else {
                    let scoped = if reads(&rest) {
                        String::new()
                    } else {
                        rest.clone()
                    };
                    let path = full_form(env.home, &base);
                    let key = format!("usage {path} {scoped}");
                    match self.seen.get(&key) {
                        Some(Response::Usage(u)) => o.usage = Some(u.clone()),
                        _ => want.push((
                            0,
                            Request::Usage {
                                path,
                                top: 60,
                                query: scoped,
                            },
                        )),
                    }
                }
            }
            _ => {}
        }
        o.items = items(o, env);
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
            (Response::Volumes { .. }, _) => {
                self.seen.insert("volumes".into(), reply);
            }
            (Response::Usage(_), _) => {
                // Keyed as `fill` looks it up, never by the path the reply
                // spells: a key that differs would ask again forever.
                let (base, _) = lens::place_parts(&o.term.value);
                let scoped = if reads(&rest) {
                    String::new()
                } else {
                    rest.clone()
                };
                let path = full_form(env.home, &base);
                self.seen.insert(format!("usage {path} {scoped}"), reply);
            }
            _ => return Vec::new(),
        }
        self.fill(q, env)
    }

    pub fn step(&mut self, by: i32) {
        let Some(o) = self.open.as_mut() else { return };
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
        if let Some(id) = it.disk {
            return Some(Chosen::Disk(id));
        }
        let mut text = it.term.clone();
        let mut caret_in = None;
        if it.launch && text == "content:" {
            text = "content:\"\"".into();
            caret_in = Some(o.a + text.len() - 1);
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
        let caret =
            caret_in.unwrap_or(o.a + text.len() - usize::from(deeper && quoted) + tail.len());
        if it.launch || deeper {
            o.sel = 0;
        }
        if !keep {
            self.close();
        }
        Some(Chosen::Edit { query, caret, keep })
    }

    /// Ask again next time: what was answered no longer holds.
    pub fn forget(&mut self, key: &str) {
        self.seen.remove(key);
    }
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

fn items(o: &mut Open, env: &Env) -> Vec<Item> {
    let tr = env.tr;
    let t = &o.term;
    match o.kind {
        Lens::Launch => {
            o.title = tr("Add a filter");
            o.foot = tr("Type a few letters to narrow it. Enter starts the filter.");
            lens::launch(&o.raw, |m| tr(m))
                .into_iter()
                .map(|c| Item {
                    launch: true,
                    ..it(tr(&c.label), &tr(&c.hint), c.term)
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
        Lens::Place => place_items(o, env),
    }
}

fn place_items(o: &mut Open, env: &Env) -> Vec<Item> {
    let tr = env.tr;
    let t = &o.term;
    let (base, part) = lens::place_parts(&t.value);
    let want = part.to_lowercase();
    o.foot = tr("Tab goes into a folder; Enter chooses it. Backspace climbs back out.");
    let mut out = Vec::new();
    if base.is_empty() {
        o.title = tr("Where");
        for d in o.disks.iter().flatten() {
            let root = d.volume.roots.first().cloned().unwrap_or_default();
            let name = if d.volume.label.is_empty() {
                root.clone()
            } else {
                d.volume.label.clone()
            };
            if !want.is_empty()
                && !name.to_lowercase().starts_with(&want)
                && !root.to_lowercase().contains(&want)
            {
                continue;
            }
            let group = if out.is_empty() {
                tr("Disks")
            } else {
                String::new()
            };
            let on = matches!(
                d.state,
                scour_proto::DiskState::Auto
                    | scour_proto::DiskState::Chosen
                    | scour_proto::DiskState::Configured
            );
            out.push(if on {
                Item {
                    dir: Some(root.clone()),
                    stays: true,
                    group,
                    ..it(
                        name,
                        &home_form(env.home, &root),
                        lens::place(t, &home_form(env.home, &root)),
                    )
                }
            } else {
                Item {
                    disk: Some(d.volume.id.clone()),
                    group,
                    ..it(name, &tr("not indexed — choose to index it"), String::new())
                }
            });
        }
        let mut first = true;
        for (label, path) in env.places {
            let shown = home_form(env.home, path);
            if !want.is_empty()
                && !label.to_lowercase().starts_with(&want)
                && !shown.to_lowercase().contains(&want)
            {
                continue;
            }
            out.push(Item {
                dir: Some(path.clone()),
                stays: true,
                group: if first { tr("Places") } else { String::new() },
                ..it(label.clone(), &shown, lens::place(t, &shown))
            });
            first = false;
        }
        return out;
    }
    let u = o.usage.as_ref();
    o.title = home_form(env.home, &full_form(env.home, &base));
    if let Some(u) = u.filter(|u| u.child_count as usize > u.children.len()) {
        o.title.push_str(" · ");
        o.title.push_str(
            &tr("the {n} heaviest of {all}")
                .replace("{n}", &(env.grouped)(u.children.len() as u64))
                .replace("{all}", &(env.grouped)(u64::from(u.child_count))),
        );
    }
    if part.is_empty() {
        out.push(Item {
            n: u.map(|u| u.root.files as i64),
            ..it(
                tr("this folder"),
                &u.filter(|u| u.root.bytes > 0)
                    .map(|u| (env.bytes)(u.root.bytes))
                    .unwrap_or_default(),
                lens::place(t, &base),
            )
        });
    }
    let mut kids: Vec<_> = u.map(|u| u.children.clone()).unwrap_or_default();
    kids.sort_by(|x, y| {
        y.files
            .cmp(&x.files)
            .then(y.bytes.cmp(&x.bytes))
            .then(x.path.cmp(&y.path))
    });
    for c in kids {
        let name = c.path.rsplit('/').next().unwrap_or(&c.path).to_string();
        if !want.is_empty() && !name.to_lowercase().contains(&want) {
            continue;
        }
        out.push(Item {
            n: Some(c.files as i64),
            dir: Some(c.path.clone()),
            stays: true,
            ..it(
                format!("{name}/"),
                &if c.bytes > 0 {
                    (env.bytes)(c.bytes)
                } else {
                    String::new()
                },
                lens::place(t, &home_form(env.home, &c.path)),
            )
        });
    }
    if u.is_some() && part.is_empty() {
        let mut flip = lens::place_flipped(t);
        flip.term = flip.term.replace(&t.value, &base);
        out.push(Item {
            group: " ".into(),
            ..it(tr(&flip.label), &flip.hint, flip.term)
        });
    }
    if u.is_none() {
        out.push(Item {
            wait: true,
            ..it(tr("reading the folder…"), "", String::new())
        });
    } else if out.is_empty() {
        out.push(Item {
            wait: true,
            ..it(tr("no folder here by that name"), "", String::new())
        });
    }
    out
}
