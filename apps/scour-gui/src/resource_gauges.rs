//! The resource panel's memory and processor: the rows with their bars, the
//! machine's memory as one bar, the service's share of a core and the line of
//! the last minute of it. A figure the system does not give is left out.

use std::fmt::Write as _;

use scour_i18n::Catalogue;
use scour_proto::Resources;
use scour_ui::resources as says;
use slint::SharedString;

use crate::resource_lines::decimal;
use crate::resources::{Panel, SPARK};
use crate::{MemLine, compact_bytes, t};

/// The sparkline's top, in percent of one core, until the line goes higher.
const SPARK_FLOOR: f32 = 10.0;

/// What the service and Scour's other programs hold together.
pub(crate) fn scour_bytes(r: &Resources) -> u64 {
    says::service_held(r.memory.service, r.memory.own)
        + r.running.iter().map(|x| x.bytes).sum::<u64>()
}

/// The memory rows: the service's as `service_lines` lists them, then each
/// kind of Scour's programs. Each bar is a share of the largest figure.
pub(crate) fn mem_lines(r: &Resources, cat: &Catalogue) -> Vec<MemLine> {
    let m = r.memory;
    let service = says::service_lines(m.service, m.peak, m.own, m.mapped);
    let scale = r
        .running
        .iter()
        .map(|x| x.bytes)
        .chain(service.iter().flat_map(|l| [l.bytes, l.peak]))
        .max()
        .unwrap_or(0)
        .max(1) as f32;
    let mut out: Vec<MemLine> = service
        .iter()
        .map(|l| MemLine {
            label: t(cat, l.label),
            note: if l.note.is_empty() {
                SharedString::new()
            } else {
                t(cat, l.note).replace("{n}", &compact_bytes(l.peak)).into()
            },
            value: compact_bytes(l.bytes).into(),
            share: l.bytes as f32 / scale,
            ghost: l.peak as f32 / scale,
        })
        .collect();
    for (kind, label) in says::RUNNING {
        if let Some(x) = r.running.iter().find(|x| x.kind == kind && x.count > 0) {
            out.push(MemLine {
                label: t(cat, label),
                note: SharedString::new(),
                value: compact_bytes(x.bytes).into(),
                share: x.bytes as f32 / scale,
                ghost: 0.0,
            });
        }
    }
    out
}

/// The machine's memory as one bar: Scour's share and everyone else's, of
/// all of it, and the words under it.
#[derive(Debug, PartialEq)]
pub(crate) struct Machine {
    pub us: f32,
    pub others: f32,
    pub us_said: String,
    pub others_said: String,
    pub said: String,
}

pub(crate) fn machine(r: &Resources, cat: &Catalogue) -> Option<Machine> {
    let m = r.memory;
    if m.total == 0 {
        return None;
    }
    let scour = scour_bytes(r);
    let used = m.total.saturating_sub(m.available);
    let others = used.saturating_sub(scour);
    Some(Machine {
        us: scour as f32 / m.total as f32,
        others: others as f32 / m.total as f32,
        // The program's name, which no language translates.
        us_said: format!("Scour {}", compact_bytes(scour)),
        others_said: format!("{} {}", t(cat, says::OTHERS), compact_bytes(others)),
        said: t(cat, says::MACHINE_MEMORY)
            .replace("{used}", &compact_bytes(used))
            .replace("{total}", &compact_bytes(m.total)),
    })
}

/// A share in percent, said short (`0.7%`, `%0,7`) and long (`of one core`).
pub(crate) fn percent(cat: &Catalogue, v: f32) -> (String, String) {
    let n = decimal(f64::from(v), 1);
    (
        t(cat, "{n}%").replace("{n}", &n),
        t(cat, says::CORE).replace("{n}", &n),
    )
}

/// The processor's words: the service's latest share, short and long, and
/// the machine's line. Empty where there is nothing to say yet.
pub(crate) fn cpu_said(p: &Panel, r: &Resources, cat: &Catalogue) -> (String, String, String) {
    let c = r.cpu;
    let (short, long) = p.spark.back().map(|v| percent(cat, *v)).unwrap_or_default();
    let machine = match p.machine {
        Some(busy) if c.total > 0 => t(cat, says::MACHINE_CPU)
            .replace("{n}", &decimal(f64::from(busy), 1))
            .replace("{t}", &c.threads.to_string())
            .replace("{l}", &decimal(f64::from(c.load_x100) / 100.0, 2)),
        _ => String::new(),
    };
    (short, long, machine)
}

/// Whether the system says nothing about the processor at all.
pub(crate) fn cpu_unknown(r: &Resources) -> bool {
    r.cpu.total == 0 && r.cpu.service_ms == 0
}

/// The sparkline in a 300 × 64 box, newest at the right edge and each answer
/// a thirtieth of the way: the line, the area under it, and where it ends.
/// A point level with both neighbours is left out: the software renderer's
/// stroker did not come back from a join between two segments in line.
pub(crate) fn spark(values: &[f32]) -> Option<(String, String, f32)> {
    const W: f32 = 300.0;
    const TOP: f32 = 8.0;
    const BOTTOM: f32 = 56.0;
    if values.len() < 2 {
        return None;
    }
    let values = &values[values.len().saturating_sub(SPARK)..];
    let top = values.iter().copied().fold(SPARK_FLOOR, f32::max);
    let step = W / (SPARK - 1) as f32;
    let first = SPARK - values.len();
    // Rounded as they are written, so "level" is what is drawn.
    let ys: Vec<String> = values
        .iter()
        .map(|v| format!("{:.1}", BOTTOM - v.clamp(0.0, top) / top * (BOTTOM - TOP)))
        .collect();
    let mut line = String::new();
    for (k, y) in ys.iter().enumerate() {
        let level = k > 0 && k + 1 < ys.len() && ys[k - 1] == *y && ys[k + 1] == *y;
        if level {
            continue;
        }
        let x = (first + k) as f32 * step;
        let _ = write!(line, "{}{x:.1} {y}", if k == 0 { "M " } else { " L " });
    }
    let area = format!(
        "{line} L {W:.1} {BOTTOM:.1} L {:.1} {BOTTOM:.1} Z",
        first as f32 * step
    );
    let end = ys.last().and_then(|y| y.parse().ok()).unwrap_or(BOTTOM);
    Some((line, area, end))
}
