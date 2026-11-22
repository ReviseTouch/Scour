//! A deep page in name, path or extension order, reached instead of walked to.
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

use crate::columns::Field;
use crate::extension_order::{Chunk, ExtensionOrder};
use crate::name_order::NameOrder;
use crate::order::PathOrder;
use crate::search::Segment;

/// How many rows sharing one key *and* one second this will gather before
/// leaving the page to the walk. Rows stepped over to fill the page are not
/// counted: a deleted folder is a run of dead positions in path order, cheap
/// to step over and a second's walk to give up on.
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
    Ext(ExtensionOrder<'a>),
}

impl Order<'_> {
    fn at(self, i: usize) -> Option<u32> {
        match self {
            Order::Name(o) => o.at(i),
            Order::Path(o) => o.at(i),
            Order::Ext(o) => o.at(i),
        }
    }

    /// The row at every position of `from..to`, in order. The extension order
    /// decodes a chunk of thirty-two to read one position, so a run of them
    /// keeps the chunk.
    fn each(self, from: usize, to: usize, mut f: impl FnMut(usize, Option<u32>)) {
        match self {
            Order::Ext(o) => {
                let mut chunk = Chunk::default();
                for i in from..to {
                    f(i, chunk.at(&o, i));
                }
            }
            _ => {
                for i in from..to {
                    f(i, self.at(i));
                }
            }
        }
    }
}

/// Whether a row is shown: alive, and not under a removal that is waiting for
/// the next commit. `hide` is asked only of segments `hides` names.
struct Shown<'h> {
    hide: &'h dyn Fn(usize, usize) -> bool,
    hides: &'h [bool],
}

impl Shown<'_> {
    fn row(&self, s: usize, seg: &Segment<'_>, row: usize) -> bool {
        seg.is_alive(row) && !(self.hides[s] && (self.hide)(s, row))
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
    fn build(
        seg: &Segment<'_>,
        s: usize,
        shown: &Shown<'_>,
        order: Order<'_>,
        rows: usize,
    ) -> Rank {
        let live: usize = seg.alive.iter().map(|b| b.count_ones() as usize).sum();
        if !shown.hides[s] && (seg.alive.is_empty() || live == rows) {
            return Rank {
                prefix: Vec::new(),
                total: rows,
                all_live: true,
            };
        }
        let mut prefix = Vec::with_capacity(rows.div_ceil(STRIDE));
        let mut running = 0u32;
        order.each(0, rows, |i, row| {
            if i % STRIDE == 0 {
                prefix.push(running);
            }
            if row.is_some_and(|row| shown.row(s, seg, row as usize)) {
                running += 1;
            }
        });
        Rank {
            prefix,
            total: running as usize,
            all_live: false,
        }
    }

    /// Live rows among the positions before `at`.
    fn upto(
        &self,
        seg: &Segment<'_>,
        s: usize,
        shown: &Shown<'_>,
        order: Order<'_>,
        at: usize,
    ) -> usize {
        if self.all_live {
            return at;
        }
        let entry = at / STRIDE;
        let Some(&base) = self.prefix.get(entry) else {
            return self.total;
        };
        let mut live = 0usize;
        order.each(entry * STRIDE, at, |_, row| {
            live += usize::from(row.is_some_and(|row| shown.row(s, seg, row as usize)));
        });
        base as usize + live
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
            // The key the walk compares last, `compare_extensions`: complete,
            // where its sort value is only the head.
            Order::Ext(_) => match view.folded.get(row) {
                Some(name) => out.extend_from_slice(crate::search::folded_extension(
                    view,
                    row,
                    name.as_bytes(),
                )),
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

/// The rows around `offset` in `sort` order over every shown row, or `None`
/// for a shape this cannot answer: another order, a segment written without the
/// stored order, or more rows sharing one key and one second than to gather.
/// `hide(segment, row)` is a removal not yet committed, asked only of the
/// segments `hides` marks: without it a page asked in the second between a
/// deletion and its commit was walked to, 1.1 s and 2 GB at three million rows
/// by path.
pub(crate) fn reach(
    views: &[Segment<'_>],
    sort: SortKey,
    descending: bool,
    offset: usize,
    limit: usize,
    hide: &dyn Fn(usize, usize) -> bool,
    hides: &[bool],
) -> Option<Deep> {
    let shown = Shown { hide, hides };
    let orders: Vec<Order<'_>> = views
        .iter()
        .map(|seg| {
            let rows = seg.rows();
            match sort {
                SortKey::Name => seg.norder.filter(|o| o.rows() == rows).map(Order::Name),
                SortKey::Path => seg.porder.filter(|o| o.rows() == rows).map(Order::Path),
                SortKey::Ext => seg.eorder.filter(|o| o.rows() == rows).map(Order::Ext),
                _ => None,
            }
        })
        .collect::<Option<_>>()?;
    let n = views.len();
    let ranks: Vec<Rank> = (0..n)
        .map(|s| Rank::build(&views[s], s, &shown, orders[s], views[s].rows()))
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
    let upto = |s: usize, at: usize| ranks[s].upto(&views[s], s, &shown, orders[s], at);

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

    // Rows of the group that precede the page. The order inside a group is the
    // same both ways — newest first, then path — so only where the group sits
    // depends on the direction.
    let skip = if descending {
        upto_group - 1 - target
    } else {
        target - below
    };

    // **Inside the group, by date.** In every segment a group's positions run
    // in row order, and rows are stored newest first, so the dates fall along
    // it: the second the page begins in is a bisection, and only that second's
    // rows are gathered. Gathering the whole group instead gave up past a
    // hundred thousand rows — every common extension, and the commonest names —
    // and the walk then took 0.8 to 1.2 s.
    let misread = std::cell::Cell::new(false);
    let mtime = |s: usize, at: usize| -> i64 {
        match orders[s].at(at) {
            Some(row) => views[s].num_of(Field::Mtime, row as usize),
            None => {
                misread.set(true);
                i64::MIN
            }
        }
    };
    // The first position of the group with a date not after `t`.
    let cut = |s: usize, t: i64| bound(first[s], past[s], |i| mtime(s, i) > t);
    let newer = |t: i64| -> usize { (0..n).map(|s| upto(s, cut(s, t)) - upto(s, first[s])).sum() };
    let (mut low, mut high) = (i64::MAX, i64::MIN);
    for s in 0..n {
        if past[s] > first[s] {
            high = high.max(mtime(s, first[s]));
            low = low.min(mtime(s, past[s] - 1));
        }
    }
    if low > high {
        return None;
    }
    while low < high {
        let mid = low + ((i128::from(high) - i128::from(low)) / 2) as i64;
        if newer(mid) <= skip {
            high = mid;
        } else {
            low = mid + 1;
        }
    }
    let when = high;
    let skip = skip.checked_sub(newer(when))?;
    let from: Vec<usize> = (0..n).map(|s| cut(s, when)).collect();
    let to: Vec<usize> = (0..n)
        .map(|s| bound(from[s], past[s], |i| mtime(s, i) >= when))
        .collect();
    if misread.get() || (0..n).map(|s| to[s] - from[s]).sum::<usize>() > TIE_ROWS {
        return None;
    }

    let mut rows: Vec<(u32, u32)> = Vec::new();
    let take = |rows: &mut Vec<(u32, u32)>, s: usize, at: usize| -> bool {
        match orders[s].at(at) {
            Some(row) if shown.row(s, &views[s], row as usize) => {
                rows.push((s as u32, row));
                true
            }
            _ => false,
        }
    };
    for s in 0..n {
        for at in from[s]..to[s] {
            take(&mut rows, s, at);
        }
    }
    // The page may run on past that second. Each segment's next `limit` shown
    // rows in the order asked hold everything it can reach: a row beyond them
    // has `limit` rows of its own segment before it.
    let mut key = Vec::new();
    for s in 0..n {
        let mut taken = 0usize;
        // Ascending, the order asked is the stored one from here on.
        let end = if descending { past[s] } else { views[s].rows() };
        let mut at = to[s];
        while taken < limit && at < end {
            taken += usize::from(take(&mut rows, s, at));
            at += 1;
        }
        if !descending {
            continue;
        }
        // Descending, what comes next is stored *before* this group, a group at
        // a time, each read from its start. Stepping back takes whole groups;
        // the one the step stops inside is read from its start instead of
        // finished, which for a group of a million was the walk's price. Rows
        // taken twice over would be on the page twice, so the two halves meet.
        let mut at = first[s];
        while taken < limit && at > 0 {
            at -= 1;
            taken += usize::from(take(&mut rows, s, at));
        }
        if at == 0 || at == first[s] {
            continue;
        }
        // Back from `at` by doubling steps while the key holds, then bisected.
        keys.fill(s, at, &mut key);
        let (mut lo, mut hi, mut step) = (0usize, at, 1usize);
        while let Some(p) = hi.checked_sub(step) {
            if keys.cmp(s, p, &key).is_ne() {
                lo = p + 1;
                break;
            }
            hi = p;
            step *= 2;
        }
        let start = bound(lo, hi, |i| keys.cmp(s, i, &key).is_lt());
        let mut taken = 0usize;
        for i in start..at {
            if taken >= limit {
                break;
            }
            taken += usize::from(take(&mut rows, s, i));
        }
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

/// Mantissa bits a numeric bin keeps: a bin is one value below 4,096 and a
/// 4,096th of its magnitude above.
const MANTISSA: u32 = 12;
/// Bins on each side of zero. `mag` tops out at `(52 << 12) | 4095`.
const HALF: u32 = 1 << 18;
/// The most rows the pass that gathers a page's neighbourhood will hold, 9.6 MB
/// of them. Past it the bin is split again by date, or the page is left to the
/// walk. `SCOUR_GATHER_ROWS` overrides, for the tests.
fn gather_rows() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| {
        std::env::var("SCOUR_GATHER_ROWS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(400_000)
    })
}

fn mag(x: u64) -> u32 {
    if x < 1 << MANTISSA {
        return x as u32;
    }
    let e = 63 - x.leading_zeros();
    ((e - MANTISSA + 1) << MANTISSA) | ((x >> (e - MANTISSA)) as u32 & ((1 << MANTISSA) - 1))
}

/// A bin a number falls in, in the same order as the numbers: smaller first.
fn bin(z: i64) -> u32 {
    if z >= 0 {
        HALF + mag(z as u64)
    } else {
        HALF - 1 - mag(!z as u64)
    }
}

/// The bins holding positions `first..=last` of a histogram, and how many rows
/// come before the first of them.
fn bins_for(hist: &[u32], first: usize, last: usize) -> (u32, u32, usize, usize) {
    let (mut b1, mut b2, mut ahead, mut through) = (0, 0, 0, 0);
    let mut cum = 0usize;
    let mut found_first = false;
    for (b, &n) in hist.iter().enumerate() {
        if n == 0 {
            continue;
        }
        let next = cum + n as usize;
        if !found_first && first < next {
            b1 = b as u32;
            ahead = cum;
            found_first = true;
        }
        if last < next {
            b2 = b as u32;
            through = next;
            break;
        }
        cum = next;
    }
    (b1, b2, ahead, through - ahead)
}

/// What [`reach_numeric`] found: every row whose key lies between the page's
/// first and last, so ties at either end are whole.
pub(crate) struct Numeric {
    /// `(segment, row, value, mtime)`, in no order.
    pub rows: Vec<(u32, u32, i64, i64)>,
    /// How many of those precede the page once they are ordered.
    pub skip: usize,
    /// Every match.
    pub total: usize,
}

/// A deep page in a numeric order **without holding what is above it**. The
/// walk kept every row ahead of the page to know where the page starts: 300 to
/// 400 MB for a page two million rows down, and the window asks that of three
/// connections at once while it scrolls. Instead a first pass counts the keys
/// into bins, which says which bin the page starts in and how many rows come
/// before it; a second pass gathers only the rows of that bin. A bin too full to
/// gather — one value shared by a million rows, as a sort by kind has — is split
/// by date in one more pass, the order's own tie-break.
///
/// `each` runs its argument for every match: `(segment, row, value, mtime)`.
/// `None` when the page cannot be gathered in [`gather_rows`]; the walk answers.
pub(crate) fn reach_numeric(
    descending: bool,
    offset: usize,
    limit: usize,
    mut each: impl FnMut(&mut dyn FnMut(u32, u32, i64, i64)),
) -> Option<Numeric> {
    use crate::search::zone_key;
    // In key space, smaller is earlier: the value by the requested direction,
    // then the newer date, which is how the merge breaks a tie.
    let z = |v: i64| zone_key(v, descending);
    let zm = |m: i64| zone_key(m, true);

    let mut hist = vec![0u32; 2 * HALF as usize];
    let mut total = 0usize;
    each(&mut |_, _, v, _| {
        hist[bin(z(v)) as usize] += 1;
        total += 1;
    });
    if offset >= total || limit == 0 {
        return Some(Numeric {
            rows: Vec::new(),
            skip: 0,
            total,
        });
    }
    let last = (offset + limit).min(total) - 1;
    let (b1, b2, mut ahead, mut inside) = bins_for(&hist, offset, last);

    // One value too common to gather: split it by date.
    let mut only: Option<(i64, u32, u32)> = None;
    if inside > gather_rows() {
        if b1 != b2 {
            return None;
        }
        hist.iter_mut().for_each(|n| *n = 0);
        let (mut lo, mut hi) = (i64::MAX, i64::MIN);
        each(&mut |_, _, v, m| {
            let k = z(v);
            if bin(k) == b1 {
                lo = lo.min(k);
                hi = hi.max(k);
                hist[bin(zm(m)) as usize] += 1;
            }
        });
        if lo != hi {
            return None;
        }
        let (m1, m2, before, within) = bins_for(&hist, offset - ahead, last - ahead);
        if within > gather_rows() {
            return None;
        }
        ahead += before;
        inside = within;
        only = Some((lo, m1, m2));
    }
    drop(hist);

    let mut rows: Vec<(i64, i64, u32, u32)> = Vec::with_capacity(inside);
    each(&mut |s, r, v, m| {
        let (k, d) = (z(v), zm(m));
        let keep = match only {
            None => (b1..=b2).contains(&bin(k)),
            Some((value, m1, m2)) => k == value && (m1..=m2).contains(&bin(d)),
        };
        if keep {
            rows.push((k, d, s, r));
        }
    });
    if rows.len() != inside {
        // The index changed between passes, which a read lock forbids; or the
        // passes disagree. Either way this is not an answer.
        return None;
    }
    // The page's first and last keys, then every row between them: a tie at
    // either end comes whole, and the caller's comparator orders inside it.
    let (first, last) = (offset - ahead, last - ahead);
    let key = |r: &(i64, i64, u32, u32)| (r.0, r.1);
    rows.select_nth_unstable_by_key(first, key);
    let lo = key(&rows[first]);
    let hi = if last > first {
        let tail = &mut rows[first + 1..];
        tail.select_nth_unstable_by_key(last - first - 1, key);
        key(&tail[last - first - 1])
    } else {
        lo
    };
    let before = rows.iter().filter(|r| key(r) < lo).count();
    let unz = |k: i64| if descending { !k } else { k };
    let rows = rows
        .into_iter()
        .filter(|r| (lo..=hi).contains(&key(r)))
        .map(|(k, d, s, r)| (s, r, unz(k), !d))
        .collect();
    Some(Numeric {
        rows,
        skip: first - before,
        total,
    })
}
