//! Sort keys: a row's value for the column asked, and how a page is cut by
//! them.

use super::*;
use crate::columns::{BLOCK, Field};
use scour_core::text::{Folder, Unaccented};
use scour_core::{Hit, SortKey};

/// What a row sorts by, without its row being built. Sorting a million rows by
/// name used to allocate a million short `Vec`s, 1,666 ms; `Head` packs the first
/// bytes of the folded name into a number that orders identically.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub enum SortValue {
    Num(i64),
    /// The first sixteen bytes, big-endian. Abbreviated for names and for the
    /// rare extension whose Unicode fold grows past sixteen bytes; equality
    /// means "might be equal" and the boundary group is compared in full.
    Head(u128),
    Text(Vec<u8>),
}

/// The first sixteen bytes, big-endian, so that comparing the numbers is
/// comparing the bytes. Shorter input is padded with zeroes, which sorts before
/// anything — the same as a shorter string.
fn head(bytes: &[u8]) -> u128 {
    let mut v = [0u8; 16];
    let n = bytes.len().min(16);
    v[..n].copy_from_slice(&bytes[..n]);
    u128::from_be_bytes(v)
}

/// What the merge compares a row by in a stored text order, built outside a
/// walk: the same key [`sort_value_into`] makes for the same row.
pub(crate) fn merge_key(seg: &Segment<'_>, row: usize, sort: SortKey) -> SortValue {
    match sort {
        SortKey::Name => SortValue::Head(head(seg.folded.get(row).unwrap_or_default().as_bytes())),
        SortKey::Path => {
            let dir = seg.dirs.get(seg.dir_id(row)).unwrap_or_default();
            let name = seg.names.get(row).unwrap_or_default();
            let mut buf = Vec::with_capacity(dir.len() + 1 + name.len());
            match dir.as_str() {
                "" => {}
                "/" => buf.push(b'/'),
                d => {
                    buf.extend_from_slice(d.as_bytes());
                    buf.push(b'/');
                }
            }
            buf.extend_from_slice(name.as_bytes());
            SortValue::Text(buf)
        }
        _ => SortValue::Num(0),
    }
}

/// Is this key exact, or only the beginning of one? Names are abbreviated, and so
/// are extensions — eligibility is twelve bytes of the spelling, which folding can
/// expand past sixteen. **The path is not on this list**: the merge resolves an
/// equal key by comparing folded names, so a path key would order its tie group by
/// file name, and sixteen bytes of a path is mostly directory.
pub(crate) fn key_is_exact(key: SortKey) -> bool {
    !matches!(key, SortKey::Name | SortKey::Ext)
}

/// The directory paths this walk has already rebuilt, by directory number.
/// [`DirTable::get`] decodes forward from the nearest restart and allocates, the
/// wrong trade for a table read once per row: 2,241,762 rows in 257,167
/// directories is **every directory rebuilt 8.7 times over**.
#[derive(Default)]
pub(crate) struct DirPaths {
    dirs: Vec<Option<Box<str>>>,
    /// Where the last row's spelled name was found. See [`crate::spelling::Reader`].
    names: crate::spelling::Reader,
}

/// A directory's path out of the cache, rebuilt the first time it is asked for.
fn cached_dir<'a>(dirs: &'a mut Vec<Option<Box<str>>>, seg: &Segment<'_>, id: u32) -> &'a str {
    if dirs.is_empty() {
        dirs.resize_with(seg.dirs.len(), || None);
    }
    let at = id as usize;
    if at >= dirs.len() {
        // A number the table does not hold. `get` says the same thing by
        // returning nothing, and a row with no directory is its own name.
        return "";
    }
    if dirs[at].is_none() {
        dirs[at] = Some(seg.dirs.get(id).unwrap_or_default().into_boxed_str());
    }
    dirs[at].as_deref().unwrap_or_default()
}

/// `name` is the row's folded name, as the walk yields it. For a caller holding
/// one row; the walk uses [`sort_value_into`], which reuses the buffer a path key
/// is built in.
pub(super) fn sort_value(
    seg: &Segment<'_>,
    row: usize,
    name: &[u8],
    key: SortKey,
    terms: &[Vec<u8>],
    folders: &[(u32, i64)],
    dirs: &mut DirPaths,
) -> SortValue {
    let mut out = SortValue::Num(0);
    sort_value_into(&mut out, seg, row, name, key, terms, folders, dirs);
    out
}

/// The same, written into a value the walk hands back on the next row. **Only the
/// path key cares**: a fresh string per row is 2,234,583 allocations on an
/// unfiltered query, of which two hundred are kept, so the buffer in `out` is
/// refilled and [`out_of_page`] reads it in place.
// One argument over the limit, and it is that buffer: gathering them into a
// struct would put a lifetime on the walk's hottest call.
#[allow(clippy::too_many_arguments)]
pub(super) fn sort_value_into(
    out: &mut SortValue,
    seg: &Segment<'_>,
    row: usize,
    name: &[u8],
    key: SortKey,
    terms: &[Vec<u8>],
    folders: &[(u32, i64)],
    dirs: &mut DirPaths,
) {
    if key == SortKey::Path {
        // Take back whatever buffer the last row was built in. A row keyed by
        // something else, or the first row of a walk, starts with none.
        let mut buf = match std::mem::replace(out, SortValue::Num(0)) {
            SortValue::Text(b) => b,
            _ => Vec::new(),
        };
        buf.clear();
        // The name as it is *spelled*, read forward: from its block's start was
        // 320 ms of 665 on 2,234,580 rows. A capitalised one is rebuilt inside
        // the reader, so the reader and the cache are borrowed apart.
        let raw = dirs.names.at(&seg.names, row).as_bytes();
        // The same join `Segment::path` makes, over a directory this walk may
        // already have rebuilt. Written into the buffer rather than through
        // `format!`, which allocates twice.
        let dir = cached_dir(&mut dirs.dirs, seg, seg.dir_id(row));
        match dir {
            "" => buf.extend_from_slice(raw),
            "/" => {
                buf.push(b'/');
                buf.extend_from_slice(raw);
            }
            d => {
                buf.extend_from_slice(d.as_bytes());
                buf.push(b'/');
                buf.extend_from_slice(raw);
            }
        }
        *out = SortValue::Text(buf);
        return;
    }
    *out = match key {
        // Already folded, which is what the terms are, plus the directory's
        // recorded distance — one byte read.
        SortKey::Relevance => {
            SortValue::Num(relevance(name, terms, seg.dirs.steps(seg.dir_id(row))))
        }
        SortKey::Name => SortValue::Head(head(name)),
        SortKey::Ext => SortValue::Head(head(folded_extension(seg, row, name))),
        // Answered above, where the buffer it is built in can be reused.
        SortKey::Path => unreachable!("the path key is written, not returned"),
        // **A folder sorts by what is under it**, when that is known: its own
        // `Size` is its entry table, four kilobytes, which put every folder behind
        // every file larger than a block. Empty table or a file: the column.
        SortKey::Size => SortValue::Num({
            let own = seg.num(Field::Size, row);
            // **The `IsDir` read first, and it is not a micro-optimisation.**
            // Without it two million file rows each pay a failed binary search
            // over a quarter of a million directories: 183 ms against 138.
            if seg.num(Field::IsDir, row) == 0 || folders.is_empty() {
                own
            } else {
                folders
                    .binary_search_by_key(&(row as u32), |(r, _)| *r)
                    .ok()
                    .map(|i| folders[i].1)
                    .unwrap_or(own)
            }
        }),
        SortKey::Modified => SortValue::Num(seg.num(Field::Mtime, row)),
        SortKey::Created => SortValue::Num(seg.num(Field::Ctime, row)),
        SortKey::Accessed => SortValue::Num(seg.num(Field::Atime, row)),
        SortKey::Kind => SortValue::Num(seg.num(Field::Kind, row)),
        SortKey::Mode => SortValue::Num(seg.num(Field::Mode, row)),
        SortKey::Uid => SortValue::Num(seg.num(Field::Uid, row)),
        SortKey::Gid => SortValue::Num(seg.num(Field::Gid, row)),
        SortKey::Disk => SortValue::Num(seg.num(Field::Disk, row)),
    };
}

/// What a row sorts by in a numeric order, outside a walk: the number
/// [`sort_value_into`] makes for it, folders included. `None` for an order whose
/// key is not a stored number.
pub(crate) fn numeric_key(
    seg: &Segment<'_>,
    row: usize,
    sort: SortKey,
    folders: &[(u32, i64)],
) -> Option<i64> {
    sort_field(sort)?;
    let mut v = SortValue::Num(0);
    sort_value_into(
        &mut v,
        seg,
        row,
        b"",
        sort,
        &[],
        folders,
        &mut DirPaths::default(),
    );
    match v {
        SortValue::Num(n) => Some(n),
        _ => None,
    }
}

/// Whether an order's key is a number a row stores.
pub(crate) fn is_numeric(sort: SortKey) -> bool {
    sort_field(sort).is_some()
}

/// The column a numeric order reads, when there is one: the keys whose value is a
/// number a row stores, which is what lets the zone map bound a block without
/// decoding it. `Size` is here because [`zone_order`] widens the range to cover
/// the folder rollups.
pub(super) fn sort_field(key: SortKey) -> Option<Field> {
    Some(match key {
        SortKey::Size => Field::Size,
        SortKey::Modified => Field::Mtime,
        SortKey::Created => Field::Ctime,
        SortKey::Accessed => Field::Atime,
        SortKey::Disk => Field::Disk,
        SortKey::Mode => Field::Mode,
        SortKey::Uid => Field::Uid,
        SortKey::Gid => Field::Gid,
        SortKey::Kind => Field::Kind,
        SortKey::Name | SortKey::Ext | SortKey::Path | SortKey::Relevance => return None,
    })
}

/// A sort value as the block order reads it: **smaller is better**, whichever
/// direction was asked for. The complement rather than the negation, because
/// `-i64::MIN` is not an `i64` — so [`out_of_reach`] needs no direction at all.
pub(crate) fn zone_key(value: i64, desc: bool) -> i64 {
    if desc { !value } else { value }
}

/// The candidate blocks in the order a numeric sort wants them, best first — the
/// stored maximum descending, the minimum ascending, decoding not one row. Ties
/// break on the block number ascending, which is load-bearing: stopping is sound
/// only if every unopened block holds rows *after* those already held.
pub(super) fn zone_order(
    seg: &Segment<'_>,
    field: Field,
    desc: bool,
    blocks: &[u32],
    folders: &[(u32, i64)],
) -> Vec<(i64, u32)> {
    let rollups = field == Field::Size && !folders.is_empty();
    let mut at = 0usize;
    let mut out: Vec<(i64, u32)> = Vec::with_capacity(blocks.len());
    for &block in blocks {
        // A block the header cannot describe is one nothing may be concluded
        // about, so it reaches everything and is opened first.
        let (lo, hi) = seg
            .cols
            .block_range(field, block as usize)
            .unwrap_or((i64::MIN, i64::MAX));
        let mut reach = if desc { hi } else { lo };
        if rollups {
            let from = block as usize * BLOCK;
            let to = from + BLOCK;
            while at < folders.len() && (folders[at].0 as usize) < from {
                at += 1;
            }
            let mut j = at;
            while j < folders.len() && (folders[j].0 as usize) < to {
                reach = if desc {
                    reach.max(folders[j].1)
                } else {
                    reach.min(folders[j].1)
                };
                j += 1;
            }
        }
        out.push((zone_key(reach, desc), block));
    }
    // The derived order on the pair is already the one wanted — reach first, block
    // number to break it — which is why the key is complemented rather than
    // compared through a closure that asks the direction per comparison.
    out.sort_unstable();
    out
}

/// Can any row of this block still reach the page? `worst` and `reach` are both
/// in [`zone_key`] space, where smaller is better; [`zone_order`] sorts by reach
/// and then by block, so one block out of reach means every later one is too.
pub(super) fn out_of_reach(worst: Option<(i64, u32)>, reach: i64, block: u32) -> bool {
    match worst {
        // Not a page yet, so nothing to be out of reach of.
        None => false,
        Some((held, row)) => {
            held < reach || (held == reach && (row as usize) < block as usize * BLOCK)
        }
    }
}

/// The order a page is chosen in: the sort value, then the row. The row is not a
/// formality — rows are stored newest-first with the path breaking that, so
/// ordering ties by row is ordering them the way the final sort will, and it makes
/// the comparison total: keeping `kind`'s tie group cost 73 ms against four.
fn page_order(desc: bool) -> impl Fn(&(SortValue, u32), &(SortValue, u32)) -> std::cmp::Ordering {
    move |a, b| if desc { b.0.cmp(&a.0) } else { a.0.cmp(&b.0) }.then(a.1.cmp(&b.1))
}

/// The rows that can still reach the page, given only their sort values.
///
/// The whole tie group at the boundary comes too: the final order breaks ties on
/// the path, so a row tied with the last may still displace it, and cutting at
/// exactly `need` returns a page that is deterministic, plausible and wrong.
///
/// Also the trim, so it has to be idempotent — and is, because **the page's worst
/// row only ever improves**. The group is kept by *value*, not by the row that
/// breaks it: a row the boundary ties with today may be the boundary tomorrow.
pub(super) fn narrow(keyed: &mut Vec<(SortValue, u32)>, need: usize, desc: bool, exact: bool) {
    if need == 0 {
        keyed.clear();
        return;
    }
    // Fewer than a page: nothing to choose between, and no boundary to be had.
    if keyed.len() < need {
        return;
    }
    // Selection, not a sort: which forty win out of two hundred thousand does not
    // require ordering the rest. Run even at exactly `need`, where it leaves the
    // page's worst row at `need - 1` for `worst` and `bar` to read.
    let k = need - 1;
    keyed.select_nth_unstable_by(k, page_order(desc));
    if exact {
        keyed.truncate(need);
        return;
    }
    // An abbreviated key only says the first sixteen bytes agree, and there are
    // few such rows. Gathered by swapping rather than into a second vector,
    // because this runs inside the walk.
    let boundary = keyed[k].0.clone();
    let mut end = need;
    for i in need..keyed.len() {
        if keyed[i].0 == boundary {
            keyed.swap(end, i);
            end += 1;
        }
    }
    keyed.truncate(end);
}

/// Can this row still reach the page, given the worst row the page holds?
///
/// The text key's answer to [`out_of_reach`]: nothing here skips a block, since no
/// stored number bounds a name. What it skips is the **keeping** — 32 bytes a
/// candidate and, ordering by path, the string it owns, 2,234,583 times over.
/// `bar` may be thousands of rows stale, which is safe in the one direction that
/// matters: the page's worst only improves, so an old boundary rejects less.
pub(super) fn out_of_page(
    bar: &Option<(SortValue, u32)>,
    key: &SortValue,
    row: u32,
    desc: bool,
    exact: bool,
) -> bool {
    let Some((held, at)) = bar else {
        // No page yet, so nothing to be out of.
        return false;
    };
    match if desc { held.cmp(key) } else { key.cmp(held) } {
        std::cmp::Ordering::Less => false,
        std::cmp::Ordering::Greater => true,
        // Equal on the key: the row number is what `page_order` breaks it
        // with, and it may only be trusted when the key is the whole answer.
        std::cmp::Ordering::Equal => exact && row > *at,
    }
}

pub(crate) fn sort_hits(hits: &mut [Hit], key: SortKey, desc: bool, terms: &[&str]) {
    // Text keys are computed once a row, not once a comparison: folding inside
    // the comparator costs O(n log n) folds for a page of forty, and measured
    // 260 ms.
    let mut keyed: Vec<(Option<String>, i64, Hit)> = std::mem::take(&mut hits.to_vec())
        .into_iter()
        .map(|h| {
            let k = match key {
                SortKey::Name => Some(Unaccented.fold(h.name())),
                SortKey::Ext => Some(scour_core::ext_of(h.name())),
                _ => None,
            };
            // Scored again here rather than carried: this merges rows from
            // several segments, each scored against the same terms.
            let r = if key == SortKey::Relevance {
                let folded: Vec<Vec<u8>> = terms
                    .iter()
                    .map(|t| Unaccented.fold(t).into_bytes())
                    .collect();
                // The distance is recomputed from the path because a `Hit` carries
                // no directory number. Same function, same answer.
                let steps = crate::dirs::steps_of(crate::dirs::dir_part(&h.path));
                relevance(Unaccented.fold(h.name()).as_bytes(), &folded, steps)
            } else {
                0
            };
            (k, r, h)
        })
        .collect();

    keyed.sort_unstable_by(|(ka, ra, a), (kb, rb, b)| {
        let o = match key {
            SortKey::Relevance => ra.cmp(rb),
            SortKey::Name | SortKey::Ext => ka.cmp(kb),
            SortKey::Path => a.path.cmp(&b.path),
            SortKey::Size => a.meta.size.cmp(&b.meta.size),
            SortKey::Modified => a.meta.mtime.cmp(&b.meta.mtime),
            SortKey::Created => a.meta.ctime.cmp(&b.meta.ctime),
            SortKey::Accessed => a.meta.atime.cmp(&b.meta.atime),
            SortKey::Kind => a.kind.cmp(&b.kind),
            SortKey::Mode => a.meta.mode.cmp(&b.meta.mode),
            SortKey::Uid => a.meta.uid.cmp(&b.meta.uid),
            SortKey::Gid => a.meta.gid.cmp(&b.meta.gid),
            SortKey::Disk => a.meta.disk.cmp(&b.meta.disk),
        };
        let o = if desc { o.reverse() } else { o };
        // The stored order breaks the tie: newest first, then path. Not the path
        // alone — on a low-cardinality key like `kind` that makes the whole result
        // set one tie group.
        o.then_with(|| b.meta.mtime.cmp(&a.meta.mtime))
            .then_with(|| a.path.cmp(&b.path))
    });
    for (slot, (_, _, h)) in hits.iter_mut().zip(keyed) {
        *slot = h;
    }
}
