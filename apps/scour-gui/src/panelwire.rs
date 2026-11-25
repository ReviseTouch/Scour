//! The time, type and size panels drawn from the open lens and the query
//! text, again on every draw. What a press in them writes is `panelact`.

use scour_core::Catalog;
use scour_i18n::Catalogue;
use scour_ui::lens::{self, Lens, MODES, SCALES};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

use crate::qlens::Open;
use crate::qpanel::{self, Counted};
use crate::qwire::QueryBar;
use crate::ui::{CountChip, HistBar, SizeLook, TimeLook, TypeLook, TypeTile};
use crate::{MainWindow, qsize, qtime, qtype};

fn model<T: Clone + 'static>(v: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(v))
}

/// "1,234 items", or nothing before it is counted.
fn items(cat: &Catalogue, c: Option<Counted>) -> String {
    match c {
        Some(_) => cat.get("{n} items").replace("{n}", &qpanel::said(c)),
        None => String::new(),
    }
}

fn exact(n: u64) -> Option<Counted> {
    Some(Counted { n, capped: false })
}

/// A preset as a chip: its count said once known, and how the term holds it.
fn chip(label: String, n: Option<i64>, chosen: bool, not: bool) -> CountChip {
    let counted = n.map(|n| Counted {
        n: n.unsigned_abs(),
        capped: n < 0,
    });
    CountChip {
        label: label.into(),
        n: qpanel::said(counted).into(),
        zero: n == Some(0),
        on: chosen && !not,
        off: chosen && not,
    }
}

/// Draw the open panel, if the lens is one.
pub fn show(w: &MainWindow, qb: &mut QueryBar, cat: &Catalogue, text: &str) {
    let Some(o) = qb.lenses.open.as_ref() else {
        return;
    };
    match o.kind {
        Lens::Time => {
            show_time(w, o, &qb.lenses.look, cat);
            calendar(w, qb, cat);
        }
        Lens::Size => show_size(w, o, cat, text),
        Lens::Kind | Lens::Ext => show_type(w, o, cat, text),
        _ => {}
    }
}

fn show_time(w: &MainWindow, o: &Open, look: &qtime::Look, cat: &Catalogue) {
    let value = o.term.value.as_str();
    let preset = lens::time_pick(value).and_then(|p| p.preset);
    let presets = o
        .items
        .iter()
        .enumerate()
        .map(|(i, it)| chip(it.label.clone(), it.n, preset == Some(i), o.term.not))
        .collect();
    let shown = qtime::shown(value, look);
    let h = qtime::bars(o, look);
    let bars: Vec<HistBar> = h
        .iter()
        .flat_map(|h| {
            let peak = h.peak.max(1) as f32;
            h.bars.iter().enumerate().map(move |(i, b)| HistBar {
                share: b.count as f32 / peak,
                lit: shown.is_some_and(|s| i >= s.first && i <= s.last),
            })
        })
        .collect();
    let words = |m: &str| SharedString::from(cat.get(m).as_ref());
    let kept = qtime::kept(o, look).map_or(o.got.whole, exact);
    let said = match qtime::words(value, cat) {
        _ if value.trim().is_empty() => String::new(),
        Some(s) => [s, items(cat, kept)]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · "),
        None => value.to_string(),
    };
    w.set_lens_time(TimeLook {
        presets: model(presets),
        scales: model(SCALES.iter().map(|s| words(s.label())).collect()),
        scale: SCALES.iter().position(|s| *s == look.scale).unwrap_or(0) as i32,
        modes: model(MODES.iter().map(|m| words(m.label())).collect()),
        mode: MODES.iter().position(|m| *m == look.mode).unwrap_or(0) as i32,
        bars: model(bars),
        from: h
            .as_ref()
            .map(|h| qtime::axis_from(h, cat))
            .unwrap_or_default()
            .into(),
        to: h.as_ref().map(|_| words("today")).unwrap_or_default(),
        said: said.into(),
        total: items(cat, o.got.ages.as_ref().map(|f| f.total).and_then(exact)).into(),
        hint: words("Click or drag across bars to pick a range"),
        armed: look.armed,
    });
}

/// The calendar lights the days a date term covers, set again only when the
/// term changes so a first day being picked is not lost to a redraw. A term
/// that is dates opens it, on the month of its last day.
fn calendar(w: &MainWindow, qb: &mut QueryBar, cat: &Catalogue) {
    let lenses = &mut qb.lenses;
    let Some(o) = lenses.open.as_ref() else {
        return;
    };
    let value = o.term.value.clone();
    if lenses.cal_for.as_deref() == Some(value.as_str()) {
        return;
    }
    let fresh = lenses.cal_for.is_none();
    lenses.cal_for = Some(value.clone());
    let Some((a, b)) = qtime::days(&value) else {
        w.set_cal_from(-1);
        w.set_cal_to(-1);
        return;
    };
    w.set_cal_from(a as i32);
    w.set_cal_to(b as i32);
    if fresh && !w.get_cal_open() {
        let today = qtime::now().div_euclid(86_400);
        let months = |d: i64| {
            let (y, m, _) = lens::Date {
                day: d,
                form: lens::DateForm::Day,
            }
            .ymd();
            y * 12 + i64::from(m)
        };
        qb.cal_back = (months(today) - months(b)).max(0);
        w.set_cal_open(true);
        crate::panelact::show_calendar(w, qb.cal_back, cat);
    }
}

fn show_size(w: &MainWindow, o: &Open, cat: &Catalogue, text: &str) {
    let chosen = qsize::chosen(o, text);
    let presets = o
        .items
        .iter()
        .enumerate()
        .map(|(i, it)| chip(it.label.clone(), it.n, chosen == Some(i), o.term.not))
        .collect();
    // A preset counts itself; what any other term keeps is the query's count.
    let words = match lens::size_said(text, &o.term) {
        Some(m) => cat.get(m).into_owned(),
        None => text[o.a..o.b.min(text.len())]
            .trim_start_matches('!')
            .to_string(),
    };
    let kept = match chosen {
        Some(i) if !o.term.not => o.got.sizes.get(i).copied().flatten(),
        _ => o.got.whole,
    };
    let not = if o.term.not { "!" } else { "" };
    let said = match chosen {
        None if o.term.value.trim().is_empty() => String::new(),
        _ => [format!("{not}{words}"), items(cat, kept)]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" · "),
    };
    w.set_lens_size(SizeLook {
        presets: model(presets),
        said: said.into(),
        total: SharedString::new(),
    });
}

/// How wide a chip is drawn, near enough to share two lines between them.
fn chip_w(c: &CountChip) -> f32 {
    c.label.chars().count() as f32 * 7.0 + c.n.chars().count() as f32 * 6.3 + 22.0
}

/// "Extension", or the extensions in the kinds chosen, named.
fn ext_title(cat: &Catalogue, text: &str) -> String {
    let on = lens::picked(text, "kind").on;
    if on.is_empty() {
        return cat.get("Extension").into_owned();
    }
    let names: Vec<String> = on
        .iter()
        .map(|k| lens::kind_label(k).map_or_else(|| k.clone(), |m| cat.get(m).into_owned()))
        .collect();
    cat.get("Extensions in {kinds}")
        .replace("{kinds}", &names.join(", "))
}

fn show_type(w: &MainWindow, o: &Open, cat: &Catalogue, text: &str) {
    let tiles: Vec<TypeTile> = qtype::tiles(o, text)
        .into_iter()
        .map(|t| TypeTile {
            token: t.token.into(),
            label: cat.get(t.label).as_ref().into(),
            n: t.n.map_or_else(|| "…".into(), crate::grouped).into(),
            share: t.share,
            zero: t.n == Some(0),
            on: t.on,
            off: t.off,
        })
        .collect();
    let chips: Vec<CountChip> = qtype::chips(o, text)
        .into_iter()
        .map(|c| CountChip {
            label: format!(".{}", c.value).into(),
            n: c.n.map(crate::grouped).unwrap_or_default().into(),
            zero: c.n == Some(0),
            on: c.on,
            off: c.off,
        })
        .collect();
    let size = w.window().size();
    let wide = size.width as f32 / w.window().scale_factor();
    let room = (wide - 20.0).min(580.0) - 24.0;
    let (mut one, mut two, mut at) = (Vec::new(), Vec::new(), 0.0);
    for c in chips {
        at += chip_w(&c) + 6.0;
        if at <= room + 6.0 {
            one.push(c)
        } else {
            two.push(c)
        }
    }
    w.set_lens_type(TypeLook {
        tiles: model(tiles),
        exts: model(one),
        exts_2: model(two),
        ext_title: ext_title(cat, text).into(),
        total: items(cat, o.got.kinds.as_ref().map(|f| f.total).and_then(exact)).into(),
    });
}
