//! A deep page in name or path order, reached instead of walked to.
//!
//! Every segment stores its rows in both orders, so the row at a global offset
//! is a selection across sorted lists: bisect on a key taken from the widest
//! remaining range until the offset falls inside that key's tie group. The walk
//! this replaces built a merge key for every row above the page — 654 ms by
//! name and 1,060 ms by path for the forty rows at offset three million.
//!
//! Only the *where* is decided here. The rows around the cut are handed back
//! unordered and the caller's own comparator orders them, so a tie is broken
//! exactly as the walk breaks it.

use std::collections::HashMap;

use scour_core::SortKey;

use crate::name_order::NameOrder;
use crate::order::PathOrder;
use crate::search::Segment;

/// How many rows sharing one key, or scanned past the page to finish a group,
/// this will gather before leaving the page to the walk.
const TIE_ROWS: usize = 100_000;

/// Positions between two entries of a rank table.
const STRIDE: usize = 64;

/// What [`reach`] found.
pub(crate) struct Deep {
    /// `(segment, row)` for every row that may be on the page, in no order.
    pub rows: Vec<(u32, u32)>,
    /// How many of those precede the page once they are ordered.
    pub skip: usize,
    /// Live rows in all segments.
    pub total: usize,
    /// Keys read to get here.
    pub read: u64,
}

#[derive(Clone, Copy)]
enum Order<'a> {
    Name(NameOrder<'a>),
    Path(PathOrder<'a>),
}

impl Order<'_> {
    fn at(self, i: usize) -> Option<u32> {
        match self {
            Order::Name(o) => o.at(i),
            Order::Path(o) => o.at(i),
        }
    }
}

/// Live rows before each position of a stored order. Built per request: one a
/// deletion stale pages from the wrong place.
struct Rank {
    prefix: Vec<u32>,
    total: usize,
    all_live: bool,
}

impl Rank {
    fn build(seg: &Segment<'_>, order: Order<'_>, rows: usize) -> Rank {
        let live: usize = seg.alive.iter().map(|b| b.count_ones() as usize).sum();
        if seg.alive.is_empty() || live == rows {
            return Rank {
                prefix: Vec::new(),
                total: rows,
                all_live: true,
            };
        }
        let mut prefix = Vec::with_capacity(rows.div_ceil(STRIDE));
        let mut running = 0u32;
        for i in 0..rows {
            if i % STRIDE == 0 {
                prefix.push(running);
            }
            if order.at(i).is_some_and(|row| seg.is_alive(row as usize)) {
                running += 1;
            }
        }
        Rank {
            prefix,
            total: running as usize,
            all_live: false,
        }
    }

    /// Live rows among the positions before `at`.
    fn upto(&self, seg: &Segment<'_>, order: Order<'_>, at: usize) -> usize {
        if self.all_live {
            return at;
        }
        let entry = at / STRIDE;
        let Some(&base) = self.prefix.get(entry) else {
            return self.total;
        };
        base as usize
            + (entry * STRIDE..at)
                .filter(|&i| order.at(i).is_some_and(|row| seg.is_alive(row as usize)))
                .count()
    }
}

/// Reads the key a position sorts by, as the order's build compared it.
struct Keys<'a, 's> {
    views: &'s [Segment<'a>],
    orders: Vec<Order<'a>>,
    dirs: HashMap<(usize, u32), String>,
    scratch: Vec<u8>,
    read: u64,
    /// A key that could not be read: the answer is then the walk's to give.
    bad: bool,
}

impl Keys<'_, '_> {
    fn fill(&mut self, seg: usize, at: usize, out: &mut Vec<u8>) {
        out.clear();
        self.read += 1;
        let view = &self.views[seg];
        let Some(row) = self.orders[seg].at(at).map(|row| row as usize) else {
            self.bad = true;
            return;
        };
        match self.orders[seg] {
            Order::Name(_) => match view.folded.get(row) {
                Some(name) => out.extend_from_slice(name.as_bytes()),
                None => self.bad = true,
            },
            Order::Path(_) => {
                let Some(name) = view.names.get(row) else {
                    self.bad = true;
                    return;
                };
                let id = view.dir_id(row);
                let dir = self
                    .dirs
                    .entry((seg, id))
                    .or_insert_with(|| view.dirs.get(id).unwrap_or_default());
                match dir.as_str() {
                    "" => {}
                    "/" => out.push(b'/'),
                    d => {
                        out.extend_from_slice(d.as_bytes());
                        out.push(b'/');
                    }
                }
                out.extend_from_slice(name.as_bytes());
            }
        }
    }

    fn cmp(&mut self, seg: usize, at: usize, with: &[u8]) -> std::cmp::Ordering {
        let mut scratch = std::mem::take(&mut self.scratch);
        self.fill(seg, at, &mut scratch);
        let o = scratch.as_slice().cmp(with);
        self.scratch = scratch;
        o
    }
}

/// The first position in `lo..hi` for which `before` is false.
fn bound(mut lo: usize, mut hi: usize, mut before: impl FnMut(usize) -> bool) -> usize {
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if before(mid) {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

/// The rows around `offset` in `sort` order over every live row, or `None` for
/// a shape this cannot answer: another order, a segment written without the
/// stored order, or a tie group too large to gather.
pub(crate) fn reach(
    views: &[Segment<'_>],
    sort: SortKey,
    descending: bool,
    offset: usize,
    limit: usize,
) -> Option<Deep> {
    let orders: Vec<Order<'_>> = views
        .iter()
        .map(|seg| {
            let rows = seg.rows();
            match sort {
                SortKey::Name => seg.norder.filter(|o| o.rows() == rows).map(Order::Name),
                SortKey::Path => seg.porder.filter(|o| o.rows() == rows).map(Order::Path),
                _ => None,
            }
        })
        .collect::<Option<_>>()?;
    let n = views.len();
    let ranks: Vec<Rank> = (0..n)
        .map(|s| Rank::build(&views[s], orders[s], views[s].rows()))
        .collect();
    let total: usize = ranks.iter().map(|r| r.total).sum();
    if offset >= total {
        return Some(Deep {
            rows: Vec::new(),
            skip: 0,
            total,
            read: 0,
        });
    }
    let mut keys = Keys {
        views,
        orders: orders.clone(),
        dirs: HashMap::new(),
        scratch: Vec::new(),
        read: 0,
        bad: false,
    };
    let upto = |s: usize, at: usize| ranks[s].upto(&views[s], orders[s], at);

    // The same row counted from the ascending end.
    let target = if descending {
        total - 1 - offset
    } else {
        offset
    };
    // What is left of each segment: positions below `lo` sort before the row
    // looked for, positions from `hi` on sort after it.
    let mut lo = vec![0usize; n];
    let mut hi: Vec<usize> = views.iter().map(Segment::rows).collect();
    let mut pivot = Vec::new();
    let mut first = vec![0usize; n];
    let mut past = vec![0usize; n];
    let (below, upto_group) = loop {
        let widest = (0..n).max_by_key(|&s| hi[s] - lo[s])?;
        if hi[widest] == lo[widest] {
            return None;
        }
        let mid = lo[widest] + (hi[widest] - lo[widest]) / 2;
        keys.fill(widest, mid, &mut pivot);
        let (mut below, mut upto_group) = (0usize, 0usize);
        for s in 0..n {
            first[s] = bound(lo[s], hi[s], |i| keys.cmp(s, i, &pivot).is_lt());
            past[s] = bound(first[s], hi[s], |i| keys.cmp(s, i, &pivot).is_le());
            below += upto(s, first[s]);
            upto_group += upto(s, past[s]);
        }
        if keys.bad {
            return None;
        }
        if target < below {
            hi.copy_from_slice(&first);
        } else if target >= upto_group {
            lo.copy_from_slice(&past);
        } else {
            break (below, upto_group);
        }
    };

    if (0..n).map(|s| past[s] - first[s]).sum::<usize>() > TIE_ROWS {
        return None;
    }
    let mut rows: Vec<(u32, u32)> = Vec::new();
    let take = |rows: &mut Vec<(u32, u32)>, s: usize, at: usize| -> bool {
        match orders[s].at(at) {
            Some(row) if views[s].is_alive(row as usize) => {
                rows.push((s as u32, row));
                true
            }
            _ => false,
        }
    };
    for s in 0..n {
        for at in first[s]..past[s] {
            take(&mut rows, s, at);
        }
    }
    let skip = if descending {
        offset - (total - upto_group)
    } else {
        offset - below
    };
    // The page may run past the group: each segment's next `limit` live rows
    // hold everything it can reach.
    let mut edge = Vec::new();
    for s in 0..n {
        let mut taken = 0usize;
        if !descending {
            let mut at = past[s];
            while taken < limit && at < views[s].rows() {
                taken += usize::from(take(&mut rows, s, at));
                at += 1;
            }
            continue;
        }
        // Backwards, a group cut short would hand over its tail while the
        // order inside a group still runs forwards: finish the group.
        let mut at = first[s];
        let mut steps = 0usize;
        while at > 0 {
            if taken >= limit {
                if edge.is_empty() {
                    keys.fill(s, at, &mut edge);
                }
                if keys.cmp(s, at - 1, &edge).is_ne() {
                    break;
                }
            }
            at -= 1;
            taken += usize::from(take(&mut rows, s, at));
            steps += 1;
            if steps > TIE_ROWS + limit {
                return None;
            }
        }
        edge.clear();
    }
    if keys.bad {
        return None;
    }
    Some(Deep {
        rows,
        skip,
        total,
        read: keys.read,
    })
}
