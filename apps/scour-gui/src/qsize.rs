//! The size panel as this window keeps it: five presets, each counted with
//! its own `count` over the query less its size terms (there is no size
//! facet), and which one the term is.

use std::collections::HashMap;

use scour_proto::Response;
use scour_ui::lens::{self, SIZE_PRESETS};

use crate::qlens::{Env, Item, Open};
use crate::qpanel::{self, Want};

/// The first preset's slot; the others follow it.
pub const SIZES: u32 = 100;

pub fn fill(o: &mut Open, seen: &HashMap<String, Response>, q: &str) -> Vec<Want> {
    let mut out = Vec::new();
    if lens::reads(q) {
        o.got.sizes.clear();
        return out;
    }
    o.got.sizes = lens::size_counts(q)
        .iter()
        .enumerate()
        .map(|(i, query)| qpanel::count(seen, SIZES + i as u32, query, &mut out))
        .collect();
    out
}

/// The presets, with their counts as they come in.
pub fn items(o: &mut Open, env: &Env) -> Vec<Item> {
    let tr = env.tr;
    o.title = tr("Size");
    o.foot = tr("Or type one: >10mb, <1kb, 1mb..50mb");
    SIZE_PRESETS
        .iter()
        .enumerate()
        .map(|(i, p)| Item {
            label: tr(p.label),
            term: lens::size_preset_term(&o.term, i),
            n: o.got
                .sizes
                .get(i)
                .copied()
                .flatten()
                .map(|c| if c.capped { -(c.n as i64) } else { c.n as i64 }),
            ..Default::default()
        })
        .collect()
}

/// Which preset the term is, if it is one.
pub fn chosen(o: &Open, q: &str) -> Option<usize> {
    lens::size_preset_of(q, &o.term)
}
