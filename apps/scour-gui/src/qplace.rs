//! The place lens as this window keeps it: what it asks the service, the disk
//! tiles `scour_ui::disks` makes of the answers, the folder it shows, and the
//! lines the keys move over — in the order `placewire` draws them.

use std::collections::HashMap;

use scour_core::SourceInfo;
use scour_proto::{Disk, Request, Response};
use scour_ui::disks::{self, DiskIn, SourceIn, Tile};
use scour_ui::lens;
use scour_ui::path::{full_form, home_form};

use crate::qlens::{Env, Item, Open};

/// What each answer of the place lens is for. A disk's files are `DISK` plus
/// its tile's index; the choices' own counts start at 100.
pub const VOLUMES: u32 = 0;
pub const USAGE: u32 = 1;
pub const TREE: u32 = 2;
pub const SOURCES: u32 = 3;
pub const DISK: u32 = 10;

/// The folders the lens shows beside the tiles.
#[derive(Debug, Clone, Default)]
pub struct View {
    /// The folder the list is in, as a full path.
    pub active: String,
    /// What is typed after the value's last `/`: the list keeps what holds it.
    pub part: String,
    /// The desktop's places, label and path, home first.
    pub places: Vec<(String, String)>,
    /// The folder's own folders, heaviest first: path, name, bytes, files.
    pub dirs: Vec<(String, String, u64, u64)>,
}

/// The tiles of a `volumes` and a `sources` answer, nothing counted yet.
pub fn tiles(disks: &[Disk], sources: &[SourceInfo]) -> Vec<Tile> {
    let ins: Vec<DiskIn> = disks.iter().map(disk_in).collect();
    let srcs: Vec<SourceIn> = sources
        .iter()
        .map(|s| SourceIn {
            name: &s.name,
            roots: &s.roots,
        })
        .collect();
    disks::tiles(&ins, &srcs, |_| None)
}

pub fn disk_in(d: &Disk) -> DiskIn<'_> {
    DiskIn {
        id: &d.volume.id,
        label: &d.volume.label,
        kind: d.volume.kind.as_str(),
        fs: &d.volume.fs,
        roots: &d.volume.roots,
        size: d.volume.size,
        state: d.state.as_str(),
        live: d.seen.live,
        walking: d.seen.walking,
    }
}

/// The folder a click pinned, while the query is still the text it wrote.
pub fn pinned<'a>(browse: &'a Option<(String, String)>, q: &str) -> Option<&'a str> {
    browse
        .as_ref()
        .filter(|(text, _)| text == q)
        .map(|(_, path)| path.as_str())
}

/// The folder the list shows and what is typed past its last `/`.
pub fn browsed(value: &str, home: &str, pinned: Option<&str>) -> (String, String) {
    if let Some(p) = pinned {
        return (p.to_string(), String::new());
    }
    let (base, part) = lens::place_parts(value);
    let active = match (base.is_empty(), home.is_empty()) {
        (true, true) => "/".to_string(),
        (true, false) => home.to_string(),
        _ => full_form(home, &base),
    };
    (active, part)
}

/// What a folder's weight is counted against: the query less the whole run
/// the lens's term is in. Taking the term alone out of `rapor under:/a|under:/b`
/// leaves `rapor |under:/b`, which reads "rapor or under b".
fn scoped(q: &str, t: &lens::Term) -> String {
    let rest: Vec<&str> = lens::runs(q)
        .into_iter()
        .filter(|r| !r.contains(t))
        .map(|r| &q[r[0].start..r[r.len() - 1].end])
        .collect();
    let rest = rest.join(" ");
    if lens::reads(&rest) {
        String::new()
    } else {
        rest
    }
}

/// Take what has been answered, and say what is still to ask. The disks'
/// counts go out only once both lists are in, so their queries are final.
pub fn fill(
    o: &mut Open,
    seen: &HashMap<String, Response>,
    q: &str,
    pinned: Option<&str>,
    env: &Env,
) -> Vec<(u32, Request)> {
    let mut want = Vec::new();
    match seen.get("volumes") {
        Some(Response::Volumes { disks, .. }) => o.disks = Some(disks.clone()),
        _ => want.push((VOLUMES, Request::Volumes {})),
    }
    match seen.get("sources") {
        Some(Response::Sources { sources }) => o.sources = Some(sources.clone()),
        _ => want.push((SOURCES, Request::Sources {})),
    }
    let (active, part) = browsed(&o.term.value, env.home, pinned);
    let query = scoped(q, &o.term);
    match seen.get(&format!("usage {active} {query}")) {
        Some(Response::Usage(u)) => o.usage = Some(u.clone()),
        _ => want.push((
            USAGE,
            Request::Usage {
                path: active.clone(),
                top: 60,
                query,
            },
        )),
    }
    match seen.get(&format!("tree {active}")) {
        Some(Response::Tree { root, .. }) => o.tree = Some(root.nodes.clone()),
        _ => want.push((
            TREE,
            Request::Tree {
                path: active.clone(),
                depth: 1,
                limit: 120,
            },
        )),
    }
    o.tiles = match (&o.disks, &o.sources) {
        (Some(d), Some(s)) => tiles(d, s),
        _ => Vec::new(),
    };
    for (k, t) in o.tiles.iter_mut().enumerate() {
        let Some(q) = &t.count else { continue };
        match seen.get(&format!("disk {q}")) {
            Some(Response::Count { total, .. }) => t.files = Some(*total),
            _ => want.push((
                DISK + k as u32,
                Request::Count {
                    query: q.clone(),
                    cap: u32::MAX,
                },
            )),
        }
    }
    o.place = view(o, env, active, part);
    want
}

/// The key an answer is kept under: the one `fill` looks it up by, never what
/// the reply spells, or a key that differs would be asked for forever.
pub fn key(
    o: &Open,
    slot: u32,
    reply: &Response,
    q: &str,
    pinned: Option<&str>,
    env: &Env,
) -> Option<String> {
    let (active, _) = browsed(&o.term.value, env.home, pinned);
    Some(match (reply, slot) {
        (Response::Volumes { .. }, _) => "volumes".into(),
        (Response::Sources { .. }, _) => "sources".into(),
        (Response::Usage(_), _) => format!("usage {active} {}", scoped(q, &o.term)),
        (Response::Tree { .. }, _) => format!("tree {active}"),
        (Response::Count { .. }, n) if n >= DISK => {
            let t = o.tiles.get((n - DISK) as usize)?;
            format!("disk {}", t.count.as_ref()?)
        }
        _ => return None,
    })
}

fn view(o: &Open, env: &Env, active: String, part: String) -> View {
    let mut places = Vec::new();
    if !env.home.is_empty() {
        places.push(("~".to_string(), env.home.to_string()));
    }
    places.extend(env.places.iter().cloned());
    let mut by_path: std::collections::BTreeMap<String, (String, u64, u64)> = Default::default();
    // `usage` of `/` answers with `/` itself as its one child.
    for c in o
        .usage
        .iter()
        .flat_map(|u| &u.children)
        .filter(|c| c.path != active)
    {
        let name = c.path.rsplit('/').next().unwrap_or(&c.path).to_owned();
        by_path.insert(c.path.clone(), (name, c.bytes, c.files));
    }
    for n in o.tree.iter().flatten().filter(|n| n.is_dir) {
        by_path
            .entry(n.path.clone())
            .or_insert_with(|| (n.name.clone(), 0, n.children));
    }
    let want = part.to_lowercase();
    let mut dirs: Vec<_> = by_path
        .into_iter()
        .filter(|(_, (name, ..))| want.is_empty() || name.to_lowercase().contains(&want))
        .map(|(p, (name, b, f))| (p, name, b, f))
        .collect();
    dirs.sort_by(|a, b| b.2.cmp(&a.2).then(b.3.cmp(&a.3)).then(a.1.cmp(&b.1)));
    dirs.truncate(60);
    View {
        active,
        part,
        places,
        dirs,
    }
}

/// Where the line of each part of the lens starts in its items.
pub struct Lines {
    pub places: usize,
    pub here: usize,
    pub dirs: usize,
}

pub fn lines(o: &Open) -> Lines {
    let places = o.tiles.len();
    let here = places + o.place.places.len();
    Lines {
        places,
        here,
        dirs: here + 1,
    }
}

/// The line the place lens rests on: the first folder holding what is being
/// typed, else this folder — what Enter has always chosen.
pub fn resting(o: &Open) -> usize {
    let at = lines(o);
    if !o.place.part.is_empty() && !o.place.dirs.is_empty() {
        at.dirs
    } else {
        at.here
    }
}

fn item(label: String, term: String) -> Item {
    Item {
        label,
        term,
        ..Default::default()
    }
}

/// What the keys move over: the tiles, the places, this folder and its own
/// folders — what is drawn, in the order it is drawn. A tile nothing on is
/// indexed is passed over.
pub fn items(o: &mut Open, env: &Env) -> Vec<Item> {
    let tr = env.tr;
    o.title = tr("Where it searches");
    o.foot = format!(
        "{}  ·  {}",
        tr("click a disk: search only there"),
        tr("Ctrl+click: several")
    );
    let t = &o.term;
    let folder = |path: &str| lens::place(t, &home_form(env.home, path));
    let mut out: Vec<Item> = o
        .tiles
        .iter()
        .enumerate()
        .map(|(k, tile)| Item {
            tile: Some(k),
            wait: !tile.choosable(),
            ..item(tile.name.clone(), String::new())
        })
        .collect();
    let v = &o.place;
    for (label, path) in &v.places {
        out.push(Item {
            dir: Some(path.clone()),
            stays: true,
            ..item(label.clone(), folder(path))
        });
    }
    out.push(item(tr("this folder"), folder(&v.active)));
    for (path, name, ..) in &v.dirs {
        out.push(Item {
            dir: Some(path.clone()),
            stays: true,
            ..item(format!("{name}/"), folder(path))
        });
    }
    out
}

#[cfg(test)]
#[path = "qplace_tests.rs"]
mod tests;
