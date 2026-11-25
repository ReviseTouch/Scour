//! The place lens drawn: the strip, the tiles, the folder list and the line
//! of what the query searches, made again from the query text and the lens's
//! answers on every draw. What a press in it writes is `placeact`.

use scour_core::Catalog;
use scour_i18n::Catalogue;
use scour_ui::disks::{self, Shown, Tile};
use scour_ui::lens::{self, Scope};
use scour_ui::path::home_form;
use slint::{ModelRc, SharedString, VecModel};

use crate::MainWindow;
use crate::qlens::Open;
use crate::qplace;
use crate::qwire::home;
use crate::ui::{DiskTile, PlaceCrumb, PlaceDir, PlaceHeld, PlaceLeft, PlaceWords, StripSeg};

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(v))
}

/// The lens's words, in the window's language.
pub fn words(w: &MainWindow, cat: &Catalogue) {
    let t = |m: &str| SharedString::from(cat.get(m).as_ref());
    w.set_place_words(PlaceWords {
        title: t("Where it searches"),
        add: t("Add"),
        exclude: t("Exclude"),
        searching_in: t("Searching in:"),
        index_it: t("Index it"),
        not_now: t("Not this time"),
        never: t("Never"),
        new_disk: t("newly plugged in"),
        strip: t("Where the search looks: one slice a disk, as wide as its files in the index"),
        choose_folder: t("Choose folder…"),
        this_folder: t("this folder"),
        direct_only: t("only what is directly inside"),
        places: t("Places"),
    });
}

/// The query's place group holds the whole tile.
pub fn selected(t: &Tile, scope: &Scope) -> bool {
    t.choosable() && !t.roots.is_empty() && t.roots.iter().all(|r| scope.holds(r))
}

/// Drawn full: the query searches somewhere on the disk, or nowhere in
/// particular. A folder above a disk's root searches it too.
pub fn lit(t: &Tile, scope: &Scope) -> bool {
    scope.within.is_empty()
        || t.roots.iter().any(|r| {
            scope
                .within
                .iter()
                .any(|h| on(&h.path, r) || on(r, &h.path))
        })
}

/// `path` is `root` or lies below it.
fn on(path: &str, root: &str) -> bool {
    path == root || root == "/" || path.starts_with(&format!("{root}/"))
}

/// Draw the open place lens from its answers and the query text.
pub fn show(w: &MainWindow, o: &Open, cat: &Catalogue, text: &str) {
    let hm = home();
    let scope = lens::scope(text, &hm);
    let lit = |t: &Tile| lit(t, &scope);
    show_tiles(w, cat, &o.tiles, &scope, &lit, &hm);
    show_held(
        w,
        cat,
        &scope,
        &hm,
        o.tiles.iter().filter(|t| t.choosable()).count(),
    );
    show_folders(w, o, &hm);
}

fn files(cat: &Catalogue, n: u64) -> String {
    cat.get("{n} files").replace("{n}", &crate::grouped(n))
}

fn show_tiles(
    w: &MainWindow,
    cat: &Catalogue,
    tiles: &[Tile],
    scope: &Scope,
    lit: &dyn Fn(&Tile) -> bool,
    hm: &str,
) {
    let rows: Vec<DiskTile> = tiles
        .iter()
        .map(|t| {
            let mut meta = vec![t.fs.clone(), home_form(hm, &t.root)];
            if t.size > 0 {
                meta.push(crate::compact_bytes(t.size));
            }
            let tags: Vec<SharedString> = match t.shown {
                Shown::Partly => t.roots.iter().map(|r| home_form(hm, r).into()).collect(),
                _ => Vec::new(),
            };
            DiskTile {
                id: t.id.as_str().into(),
                name: t.name.as_str().into(),
                kind: t.kind.into(),
                meta: meta.join(" · ").into(),
                tags: model(tags),
                shown: t.shown.as_str().into(),
                state: cat.get(t.shown.label()).as_ref().into(),
                files: t.files.map(|n| files(cat, n)).unwrap_or_default().into(),
                choosable: t.choosable(),
                selected: selected(t, scope),
                lit: lit(t),
                index_it: t.index_it,
            }
        })
        .collect();
    w.set_lens_tiles(model(rows));
    let mut from = 0.0;
    let strip: Vec<StripSeg> = disks::strip(tiles)
        .into_iter()
        .map(|s| {
            let seg = StripSeg {
                from: from as f32,
                share: s.share as f32,
                kind: tiles[s.at].kind.into(),
                lit: lit(&tiles[s.at]),
            };
            from += s.share;
            seg
        })
        .collect();
    w.set_lens_strip(model(strip));
    // A sum is said only once every part of it is counted.
    let sum = |on: Vec<&Tile>| -> Option<u64> { on.iter().map(|t| t.files).sum() };
    let on: Vec<&Tile> = tiles.iter().filter(|t| t.choosable()).collect();
    if on.is_empty() {
        w.set_lens_sum(SharedString::new());
        w.set_lens_total(SharedString::new());
        return;
    }
    let shown: Vec<&Tile> = on.iter().copied().filter(|t| lit(t)).collect();
    let mut said = cat
        .get("{n} disks")
        .replace("{n}", &shown.len().to_string());
    if let Some(n) = sum(shown) {
        said = format!("{said} · {}", files(cat, n));
    }
    w.set_lens_sum(said.into());
    w.set_lens_total(sum(on).map(|n| files(cat, n)).unwrap_or_default().into());
}

fn show_held(w: &MainWindow, cat: &Catalogue, scope: &Scope, hm: &str, disks: usize) {
    let entry = |not: bool| {
        move |h: &lens::Held| PlaceHeld {
            label: home_form(hm, &h.path).into(),
            not,
        }
    };
    let mut held: Vec<PlaceHeld> = scope.within.iter().map(entry(false)).collect();
    held.extend(scope.outside.iter().map(entry(true)));
    w.set_lens_holds(model(held));
    w.set_lens_none(
        format!(
            "{} · {}",
            cat.get("everywhere"),
            cat.get("{n} disks").replace("{n}", &disks.to_string())
        )
        .into(),
    );
}

fn show_folders(w: &MainWindow, o: &Open, hm: &str) {
    let v = &o.place;
    let at = qplace::lines(o);
    let left: Vec<PlaceLeft> = v
        .places
        .iter()
        .enumerate()
        .map(|(j, (label, path))| PlaceLeft {
            label: label.as_str().into(),
            sub: match home_form(hm, path) {
                shown if shown == *label => SharedString::new(),
                shown => shown.into(),
            },
            target: path.as_str().into(),
            active: v.active == *path,
            at: (at.places + j) as i32,
        })
        .collect();
    w.set_lens_places(model(left));
    w.set_lens_here_at(at.here as i32);
    w.set_lens_place_cur(v.active.as_str().into());
    w.set_lens_place_shown(home_form(hm, &v.active).into());
    w.set_lens_place_direct(o.term.field == Some("parent"));
    w.set_lens_place_up(match v.active.rfind('/') {
        _ if v.active == "/" => SharedString::new(),
        Some(0) | None => "/".into(),
        Some(i) => v.active[..i].into(),
    });
    w.set_lens_crumbs(model(crumbs(&v.active, hm)));
    let max_b = v.dirs.iter().map(|d| d.2).max().unwrap_or(0).max(1);
    let mark = crate::marks();
    let dirs: Vec<PlaceDir> = v
        .dirs
        .iter()
        .enumerate()
        .map(|(k, (path, name, b, f))| PlaceDir {
            name: format!("{name}/").into(),
            path: path.as_str().into(),
            meta: match (*b, *f) {
                (0, 0) => SharedString::new(),
                (0, f) => scour_ui::format::grouped(f, mark.0).into(),
                (b, _) => scour_ui::format::compact_bytes(b, mark.1).into(),
            },
            bar: if *b > 0 {
                (*b as f32 / max_b as f32).clamp(0.04, 1.0)
            } else {
                0.0
            },
            at: (at.dirs + k) as i32,
        })
        .collect();
    w.set_lens_dirs(model(dirs));
}

/// The steps from `~` or `/` down to the folder, each with its path.
fn crumbs(active: &str, hm: &str) -> Vec<PlaceCrumb> {
    let shown = home_form(hm, active);
    let (mut cur, first, rest) = match shown.strip_prefix('~') {
        Some(rest) if !hm.is_empty() => (hm.to_string(), "~", rest.to_string()),
        _ => (String::new(), "/", active.to_string()),
    };
    let mut out = vec![PlaceCrumb {
        label: first.into(),
        path: if cur.is_empty() {
            "/".into()
        } else {
            cur.as_str().into()
        },
    }];
    for seg in rest.split('/').filter(|s| !s.is_empty()) {
        cur = format!("{cur}/{seg}");
        out.push(PlaceCrumb {
            label: seg.into(),
            path: cur.as_str().into(),
        });
    }
    out
}
