//! Counting what a query names by kind, extension, folder and age.

use super::*;

use crate::search::Segment;
use scour_core::{FacetBy, Kind};
use std::collections::HashMap;

/// How many rows a facet count will look at before it answers approximately.
/// A facet is a sidebar, not a result. Nobody waits for it.
const FACET_SCAN_CAP: usize = 200_000;

/// …except a distribution, which has to see everything. Rows are stored newest
/// first, so a cap takes a prefix rather than a sample, and a capped histogram
/// of ages says "today" whatever was asked.
const AGE_SCAN_CAP: usize = usize::MAX;

/// One facet question's counts while the walk runs: a kind and an age band by
/// number, anything else by its text.
enum Tally {
    Kinds(Box<[u64; 256]>),
    Bands(Vec<u64>),
    Keyed(HashMap<String, u64>),
}

impl Tally {
    /// The counts by key, as the answer names them: a kind by its token — not
    /// its label, which a rail turns into a `kind:` term and which translates —
    /// and a band by its edge, or `older`. Only what was seen.
    fn into_keys(self, by: &FacetBy) -> HashMap<String, u64> {
        match (self, by) {
            (Tally::Keyed(map), _) => map,
            (Tally::Kinds(n), _) => {
                let mut map = HashMap::new();
                for (value, &count) in n.iter().enumerate() {
                    if count > 0 {
                        let kind = Kind::from_u8(value as u8).unwrap_or(Kind::File);
                        *map.entry(kind.token().to_owned()).or_default() += count;
                    }
                }
                map
            }
            (Tally::Bands(n), FacetBy::Age { edges }) => n
                .iter()
                .enumerate()
                .filter(|&(_, &count)| count > 0)
                .map(|(band, &count)| {
                    let key = edges
                        .get(band)
                        .map_or_else(|| "older".to_owned(), |e| e.to_string());
                    (key, count)
                })
                .collect(),
            (Tally::Bands(_), _) => HashMap::new(),
        }
    }
}

/// How many live rows a segment has, and how many of them under each
/// extension, read off its extension order: the rows of an extension are one
/// group there, so a group is counted on the live bits and named by the folded
/// name of its first live row — 6,789 names for 7.3 million rows. The key is
/// what [`Spelled::ext`] says of that row; folding is by character and the dot
/// is its own, so the extension of the fold is the fold of the extension.
fn extensions_of(seg: &Segment<'_>) -> (usize, Vec<(String, u64)>) {
    let Some(order) = seg.eorder else {
        return (0, Vec::new());
    };
    let mut chunk = crate::extension_order::Chunk::default();
    let mut out: Vec<(String, u64)> = Vec::new();
    let mut live = 0usize;
    // The group being counted: its first live row, and how many.
    let mut group: Option<(u32, u64)> = None;
    let name = |row: u32| -> String {
        let folded = seg.folded.get(row as usize).unwrap_or_default();
        String::from_utf8_lossy(crate::search::folded_extension(
            seg,
            row as usize,
            folded.as_bytes(),
        ))
        .into_owned()
    };
    let close = |group: Option<(u32, u64)>, out: &mut Vec<(String, u64)>| {
        if let Some((first, n)) = group {
            let ext = name(first);
            if !ext.is_empty() {
                out.push((ext, n));
            }
        }
    };
    for i in 0..order.rows() {
        if order.starts_group(i) {
            close(group.take(), &mut out);
        }
        let Some(row) = chunk.at(&order, i) else {
            continue;
        };
        if !seg.is_alive(row as usize) {
            continue;
        }
        live += 1;
        match &mut group {
            Some((_, n)) => *n += 1,
            None => group = Some((row, 1)),
        }
    }
    close(group, &mut out);
    (live, out)
}

/// The spellings of one segment's rows, for a pass that asks in row order.
#[derive(Default)]
pub(super) struct Spelled(crate::spelling::Reader);

impl Spelled {
    /// The row's name as it is spelled, or nothing for a row to skip.
    pub(super) fn of<'r>(&'r mut self, seg: &Segment<'r>, row: usize) -> Option<&'r str> {
        self.0.read(&seg.names, row)
    }

    /// What [`scour_core::ext_of`] says of the row's spelling. Read off the
    /// fold where the spelling is as long: the rule is about bytes and a dot.
    fn ext<'a>(&mut self, seg: &Segment<'a>, row: usize) -> std::borrow::Cow<'a, str> {
        if let Some((folded, as_long)) = self.0.folded(&seg.names, row) {
            let ext = std::str::from_utf8(folded)
                .map(scour_core::ext_str)
                .unwrap_or_default();
            if as_long {
                return ext.into();
            }
        }
        scour_core::ext_of(self.of(seg, row).unwrap_or_default()).into()
    }
}

impl NativeIndex {
    /// The body of [`Index::facets`].
    pub(super) fn count_facets(&self, req: &FacetRequest) -> Result<FacetResponse> {
        let started = Instant::now();
        let inner = self.inner.read();
        let mut seen = 0usize;
        // `now` once, not per row: a walk of two hundred thousand rows that
        // asks the clock each time is asking it two hundred thousand times.
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        // The most demanding question decides, because they share the walk: a
        // distribution cannot be sampled, so it makes a top-ten exact too.
        let cap = if req.by.iter().any(|b| matches!(b, FacetBy::Age { .. })) {
            AGE_SCAN_CAP
        } else {
            FACET_SCAN_CAP
        };
        let parents: Vec<String> = req
            .by
            .iter()
            .map(|b| match b {
                FacetBy::Dir { path, .. } => path.trim_end_matches('/').to_owned(),
                _ => String::new(),
            })
            .collect();
        // Read once per row however many questions want it, and not at all when
        // none does — the common case, since kind and age are both columns.
        let wants_name = req.by.iter().any(|b| matches!(b, FacetBy::Dir { .. }));
        let wants_ext = req.by.iter().any(|b| matches!(b, FacetBy::Ext { .. }));

        // **Counted where they can be, not filed by string.** A kind and an age
        // band are a handful of values, and each row of the empty query made a
        // `String` of both to look them up: 14 million allocations and 320 ms
        // for the rail's two questions over 6.9 million rows.
        let mut tallies: Vec<Tally> = req
            .by
            .iter()
            .map(|b| match b {
                FacetBy::Kind => Tally::Kinds(Box::new([0; 256])),
                FacetBy::Age { edges } => Tally::Bands(vec![0; edges.len() + 1]),
                _ => Tally::Keyed(HashMap::new()),
            })
            .collect();
        let rows_wanted = req.by.iter().any(|b| !matches!(b, FacetBy::Ext { .. }));

        for live in &inner.segments {
            let seg = live.view()?;
            let plan = Plan::compile(&req.query, &seg)?;
            let doomed = Doomed::new(&seg, &inner.hidden_prefixes);
            // **Every extension of a segment, off its extension order.** Asked
            // with an age, the count is exact and every row was looked at, its
            // name read for the extension: the whole name arena, 200 MB of the
            // index brought in and 680 ms, behind the narrowing line of every
            // window opened on the empty query. The order holds the rows
            // grouped by extension already, and one name a group says which.
            let by_order = wants_ext
                && cap == AGE_SCAN_CAP
                && plan.is_empty()
                && doomed.is_empty()
                && seg.eorder.is_some_and(|o| o.rows() == seg.rows());
            if by_order {
                let (live_rows, extensions) = extensions_of(&seg);
                for (i, by) in req.by.iter().enumerate() {
                    if let (FacetBy::Ext { .. }, Tally::Keyed(map)) = (by, &mut tallies[i]) {
                        for (ext, n) in &extensions {
                            *map.entry(ext.clone()).or_default() += n;
                        }
                    }
                }
                if !rows_wanted {
                    seen += live_rows;
                    continue;
                }
            }
            // A segment's matches come in row order, so its names are read on
            // from the last one rather than from their block's start.
            let mut names = Spelled::default();
            let ext_by_row = wants_ext && !by_order;
            let go = crate::search::walk_matches(&seg, &plan, |row| {
                if !doomed.is_empty() && doomed.takes(&seg, row, seg.dir_id(row)) {
                    return true;
                }
                let ext = ext_by_row.then(|| names.ext(&seg, row));
                let name = wants_name.then(|| names.of(&seg, row).unwrap_or_default());
                for (i, by) in req.by.iter().enumerate() {
                    match (by, &mut tallies[i]) {
                        (FacetBy::Kind, Tally::Kinds(n)) => {
                            n[seg.num_of(Field::Kind, row) as u8 as usize] += 1;
                        }
                        (FacetBy::Age { edges }, Tally::Bands(n)) => {
                            let days = (now - seg.num_of(Field::Mtime, row)).max(0) / 86_400;
                            // Ascending, so the first band it fits is its own.
                            // Linear because there are a couple of dozen.
                            let band = edges
                                .iter()
                                .position(|&e| days <= e as i64)
                                .unwrap_or(edges.len());
                            n[band] += 1;
                        }
                        (FacetBy::Ext { .. }, Tally::Keyed(counts)) => {
                            // Looked up before it is owned: nearly every row's
                            // extension has been seen, and needs no allocation.
                            match ext.as_deref().filter(|e| !e.is_empty()) {
                                None => {}
                                Some(ext) => match counts.get_mut(ext) {
                                    Some(n) => *n += 1,
                                    None => {
                                        counts.insert(ext.to_owned(), 1);
                                    }
                                },
                            }
                        }
                        (FacetBy::Dir { .. }, Tally::Keyed(counts)) => {
                            let path = seg.path(row, name.unwrap_or_default());
                            if let Some(rest) = path
                                .strip_prefix(parents[i].as_str())
                                .and_then(|r| r.strip_prefix('/'))
                            {
                                let child = rest.split('/').next().unwrap_or(rest);
                                *counts.entry(child.to_owned()).or_default() += 1;
                            }
                        }
                        _ => {}
                    }
                }
                seen += 1;
                seen < cap
            });
            if !go {
                break;
            }
        }
        let counts: Vec<HashMap<String, u64>> = req
            .by
            .iter()
            .zip(tallies)
            .map(|(by, tally)| tally.into_keys(by))
            .collect();

        let groups: Vec<scour_core::FacetGroup> = req
            .by
            .iter()
            .zip(counts)
            .map(|(by, map)| {
                let top = match by {
                    FacetBy::Kind => 16,
                    FacetBy::Ext { top } | FacetBy::Dir { top, .. } => (*top).max(1) as usize,
                    // Every band, or the chart has holes in it.
                    FacetBy::Age { edges } => edges.len() + 1,
                };
                let mut facets: Vec<Facet> = map
                    .into_iter()
                    .map(|(key, count)| Facet { key, count })
                    .collect();
                facets.sort_unstable_by(|a, b| b.count.cmp(&a.count).then(a.key.cmp(&b.key)));
                facets.truncate(top);
                scour_core::FacetGroup {
                    by: by.clone(),
                    facets,
                }
            })
            .collect();

        Ok(FacetResponse {
            // The first group, flat, so a caller that asked one question does
            // not have to unwrap a list to read its answer.
            facets: groups.first().map(|g| g.facets.clone()).unwrap_or_default(),
            by: groups.first().map(|g| g.by.clone()).unwrap_or_default(),
            total: seen as u64,
            groups,
            capped: seen >= cap,
            took_us: started.elapsed().as_micros() as u64,
            // The index is handed a parsed query and never sees the text.
            // Stamped by the engine on the way out.
            misread: Vec::new(),
        })
    }
}
