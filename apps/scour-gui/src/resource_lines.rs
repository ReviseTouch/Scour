//! The resource panel's rows, from an answer and the panel's state: one per
//! thing kept on disk, the index's unsorted rows, Scour's programs, and the
//! foot. Words and lines are `scour_ui::resources`'; numbers are the window's.

use scour_core::Catalog;
use scour_i18n::Catalogue;
use scour_proto::{Held, HeldKind, Resources};
use scour_ui::resources as says;
use slint::SharedString;

use crate::resources::{Panel, Press, token};
use crate::{HeldLine, RunLine, SortLine, compact_bytes, grouped, marks, t};

/// A number with the language's decimal mark.
pub(crate) fn decimal(v: f64, places: usize) -> String {
    let said = format!("{v:.places$}");
    match marks().1 {
        '.' => said,
        mark => said.replace('.', &mark.to_string()),
    }
}

/// How long, as a person says it: the two largest units.
pub(crate) fn uptime_said(cat: &Catalogue, ms: u64) -> String {
    let (id, a, b) = says::uptime(ms);
    t(cat, id)
        .replace("{a}", &a.to_string())
        .replace("{b}", &b.to_string())
}

/// A row's note with its numbers in. A part whose number is nought says
/// nothing (`up to 0 B` is no limit), so it is left out.
fn note(cat: &Catalogue, id: &str, h: &Held) -> String {
    let said = cat.get(id);
    said.split(" · ")
        .filter(|part| !(part.contains("{limit}") && h.limit == 0))
        .filter(|part| !(part.contains("{parts}") && h.parts == 0))
        .collect::<Vec<_>>()
        .join(" · ")
        .replace("{n}", &grouped(h.items))
        .replace("{parts}", &h.parts.to_string())
        .replace("{limit}", &compact_bytes(h.limit))
}

/// A msgid through the catalogue, or nothing for no msgid.
fn maybe(cat: &Catalogue, id: &str) -> SharedString {
    if id.is_empty() {
        SharedString::new()
    } else {
        t(cat, id)
    }
}

/// One line per thing kept on disk, in the order the service sends.
pub(crate) fn held_lines(p: &Panel, r: &Resources, cat: &Catalogue) -> Vec<HeldLine> {
    let sizes: Vec<u64> = r.files.iter().map(|h| h.bytes).collect();
    r.files
        .iter()
        .zip(says::bars(&sizes))
        .filter_map(|(h, share)| {
            let thing = says::thing(token(h.kind))?;
            let press = Press::Clear(h.kind);
            let (said, said_no) = p.said_by(cat, press);
            let refused = says::refused(&h.busy);
            Some(HeldLine {
                token: thing.token.into(),
                label: t(cat, thing.label),
                note: note(cat, thing.note, h).into(),
                size: compact_bytes(h.bytes).into(),
                path: crate::tray::home_tilde(&h.path).into(),
                full: h.path.as_str().into(),
                share,
                // Settings are a few kilobytes and never emptied.
                bar: h.kind != HeldKind::State,
                button: t(cat, thing.act.button),
                danger: thing.act.danger,
                two_step: !thing.act.ask.is_empty(),
                ask: maybe(cat, thing.act.ask),
                go: maybe(cat, thing.act.go),
                refused: refused.map(|id| t(cat, id)).unwrap_or_default(),
                // Opened before the browser was, it closes: there is nothing to do.
                asking: p.asking == Some(press) && refused.is_none(),
                said,
                said_no,
            })
        })
        .collect()
}

/// The index's unsorted rows, under its line; no count, no line.
pub(crate) fn sort_line(p: &Panel, r: &Resources, cat: &Catalogue) -> SortLine {
    let (said, said_no) = p.said_by(cat, Press::Sort);
    let n = grouped(r.unsorted);
    SortLine {
        label: t(cat, says::UNSORTED),
        // Kept while it says what the press did, though the rows are in order.
        count: if r.unsorted > 0 || !said.is_empty() {
            n.as_str().into()
        } else {
            SharedString::new()
        },
        button: t(cat, says::SORT.button),
        ask: t(cat, says::SORT.ask).replace("{n}", &n).into(),
        go: t(cat, says::SORT.go),
        asking: p.asking == Some(Press::Sort),
        said,
        said_no,
    }
}

/// Scour's programs by kind, with how many; MCP says why there are so many.
pub(crate) fn run_lines(r: &Resources, cat: &Catalogue) -> Vec<RunLine> {
    says::RUNNING
        .iter()
        .filter_map(|(kind, label)| {
            let x = r.running.iter().find(|x| x.kind == *kind && x.count > 0)?;
            Some(RunLine {
                kind: (*kind).into(),
                label: t(cat, label),
                count: grouped(u64::from(x.count)).into(),
                note: if *kind == "mcp" {
                    format!(
                        "{} {}",
                        t(cat, says::MCP_NOTE),
                        t(cat, says::OLDEST).replace("{u}", &uptime_said(cat, x.oldest_s * 1000))
                    )
                    .into()
                } else {
                    SharedString::new()
                },
            })
        })
        .collect()
}

/// The foot: the version and how long the service has run, or why a
/// restart was refused; and the restart button's word.
pub(crate) struct Foot {
    pub said: SharedString,
    pub no: bool,
    pub restart: SharedString,
}

pub(crate) fn foot(p: &Panel, r: &Resources, cat: &Catalogue) -> Foot {
    let (no, refused) = p.said_by(cat, Press::Restart);
    Foot {
        said: if refused {
            no
        } else {
            t(cat, says::FOOT)
                .replace("{v}", &r.version)
                .replace("{u}", &uptime_said(cat, r.uptime_ms))
                .into()
        },
        no: refused,
        restart: t(
            cat,
            if p.restarting.is_some() {
                says::RESTART.done
            } else {
                says::RESTART.button
            },
        ),
    }
}
