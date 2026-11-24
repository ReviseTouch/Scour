//! The rows of a segment, in folded-extension order.
//!
//! Eligibility comes from the spelling — one to twelve bytes after the last dot,
//! before folding — while the key comes from the folded arena and may be longer.
//! Grouped as [`crate::NameOrder`]; within a group newest first, which is row
//! order, so the rows climb in small steps and are stored as those: 4.4 MB for
//! five million rows where a packed row number each is 15.2.

use crate::packed;

/// Positions a chunk. A read decodes one chunk; a stream keeps it.
const CHUNK: usize = 32;
/// The first byte of a chunk whose rows do not climb: a row number each.
const RAW: u8 = 0xff;

/// The rows of one segment in ascending folded-extension order: a count, where
/// each chunk begins and one more for the end, the chunks, then one bit per
/// position starting an equal-extension group.
#[derive(Debug, Clone, Copy)]
pub struct ExtensionOrder<'a> {
    rows: usize,
    /// The bits of a row number in a chunk stored raw.
    width: u32,
    offsets: &'a [u8],
    chunks: &'a [u8],
    groups: &'a [u8],
}

impl<'a> ExtensionOrder<'a> {
    /// Open the file, or refuse a malformed one: it is exactly as long as its
    /// count and its last offset say.
    pub fn open(bytes: &'a [u8]) -> Option<ExtensionOrder<'a>> {
        let rows = u32::from_le_bytes(bytes.get(0..4)?.try_into().ok()?) as usize;
        let table = if rows == 0 {
            0
        } else {
            (rows.div_ceil(CHUNK) + 1) * 4
        };
        let offsets = bytes.get(4..4 + table)?;
        let body = offsets
            .last_chunk::<4>()
            .map_or(0, |b| u32::from_le_bytes(*b) as usize);
        let groups_at = (4 + table).checked_add(body)?;
        if bytes.len() != groups_at.checked_add(rows.div_ceil(8))? {
            return None;
        }
        let groups = &bytes[groups_at..];
        if rows > 0 && groups.first().is_none_or(|byte| byte & 1 == 0) {
            return None;
        }
        Some(ExtensionOrder {
            rows,
            width: packed::width_for(rows),
            offsets,
            chunks: &bytes[4 + table..groups_at],
            groups,
        })
    }

    pub fn rows(&self) -> usize {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    /// The row whose extension is `i`-th in ascending order. A caller reading
    /// positions in order wants [`Chunk`], which decodes each chunk once.
    pub fn at(&self, i: usize) -> Option<u32> {
        Chunk::default().at(self, i)
    }

    /// The start of the equal-extension group containing position `i`.
    pub(crate) fn group_at_or_before(&self, i: usize) -> Option<usize> {
        crate::name_order::group_at_or_before(self.groups, self.rows, i)
    }

    /// Whether position `i` begins an equal-extension group. One bit, where
    /// [`ExtensionOrder::group_at_or_before`] scans back to the group's start —
    /// asked of every position of a large group, a square of it.
    pub(crate) fn starts_group(&self, i: usize) -> bool {
        i < self.rows
            && self
                .groups
                .get(i / 8)
                .is_some_and(|byte| byte >> (i % 8) & 1 != 0)
    }

    /// The rows of one chunk, or nothing where its bytes do not hold them.
    fn decode(&self, chunk: usize, out: &mut [u32; CHUNK]) -> Option<()> {
        let offset = |c: usize| -> Option<usize> {
            let b = self.offsets.get(c * 4..c * 4 + 4)?;
            Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
        };
        let bytes = self.chunks.get(offset(chunk)?..offset(chunk + 1)?)?;
        let n = CHUNK.min(self.rows.checked_sub(chunk * CHUNK)?);
        let (&kind, rest) = bytes.split_first()?;
        if kind == RAW {
            for (k, row) in out.iter_mut().enumerate().take(n) {
                *row = packed::get(rest, self.width, k)? as u32;
            }
            return Some(());
        }
        let step = u32::from(kind);
        if step > 32 {
            return None;
        }
        let (first, steps) = rest.split_first_chunk::<4>()?;
        out[0] = u32::from_le_bytes(*first);
        for k in 1..n {
            let by = if step == 0 {
                0
            } else {
                packed::get(steps, step, k - 1)? as u32
            };
            out[k] = out[k - 1].wrapping_add(by);
        }
        Some(())
    }
}

/// The chunk a stream of positions is in, decoded once for its thirty-two.
#[derive(Debug, Clone)]
pub(crate) struct Chunk {
    number: usize,
    rows: [u32; CHUNK],
}

impl Default for Chunk {
    fn default() -> Chunk {
        Chunk {
            number: usize::MAX,
            rows: [0; CHUNK],
        }
    }
}

impl Chunk {
    /// [`ExtensionOrder::at`], decoding only when `i` leaves the chunk held. A
    /// row number this segment does not hold answers `None`.
    pub(crate) fn at(&mut self, order: &ExtensionOrder<'_>, i: usize) -> Option<u32> {
        if i >= order.rows {
            return None;
        }
        if self.number != i / CHUNK {
            self.number = usize::MAX;
            order.decode(i / CHUNK, &mut self.rows)?;
            self.number = i / CHUNK;
        }
        let row = self.rows[i % CHUNK];
        ((row as usize) < order.rows).then_some(row)
    }
}

/// Order rows by the complete folded extension of each eligible spelled name.
/// `order` comes from the name-order build, so no second row list is allocated.
pub(crate) fn build(rows: usize, spelled: &[u8], folded: &[u8], order: Vec<u32>) -> Vec<u8> {
    let eligible = eligible_rows(rows, spelled);
    let (order, groups) =
        crate::name_order::order_keyed(rows, folded, order, extension, Some(&eligible));
    encode(&order, &groups)
}

/// The file: each chunk as a first row and packed steps where its rows climb
/// and that is smaller, a packed row number each where not.
fn encode(order: &[u32], groups: &[u8]) -> Vec<u8> {
    let rows = order.len();
    let width = packed::width_for(rows);
    let mut offsets: Vec<u32> = Vec::with_capacity(rows.div_ceil(CHUNK) + 1);
    let mut chunks: Vec<u8> = Vec::new();
    for part in order.chunks(CHUNK) {
        offsets.push(chunks.len() as u32);
        // A row below the one before it is a step of nearly 2^32: never the
        // smaller form, so falling rows are stored raw without being asked.
        let widest = part.windows(2).map(|w| w[1].wrapping_sub(w[0])).max();
        let step = widest.map_or(0, |d| u32::BITS - d.leading_zeros());
        let stepped = 4 + packed::len(part.len() - 1, step).unwrap_or(0);
        if stepped < packed::len(part.len(), width).unwrap_or(0) {
            chunks.push(step as u8);
            chunks.extend_from_slice(&part[0].to_le_bytes());
            if step > 0 {
                packed::pack(
                    part.windows(2).map(|w| u64::from(w[1].wrapping_sub(w[0]))),
                    step,
                    &mut chunks,
                );
            }
        } else {
            chunks.push(RAW);
            packed::pack(part.iter().map(|&row| u64::from(row)), width, &mut chunks);
        }
    }
    let mut out = Vec::with_capacity(8 + offsets.len() * 4 + chunks.len() + groups.len());
    out.extend_from_slice(&(rows as u32).to_le_bytes());
    if rows > 0 {
        offsets.push(chunks.len() as u32);
        for at in &offsets {
            out.extend_from_slice(&at.to_le_bytes());
        }
    }
    out.extend_from_slice(&chunks);
    out.extend_from_slice(groups);
    out
}

/// The row's complete folded extension, with eligibility decided pre-fold.
#[cfg(test)]
pub(crate) fn folded_extension<'a>(spelled: &[u8], folded: &'a [u8]) -> &'a [u8] {
    if extension_is_eligible(spelled) {
        extension(folded)
    } else {
        b""
    }
}

/// The suffix after the last dot, with no folded-length restriction.
pub(crate) fn extension(name: &[u8]) -> &[u8] {
    match name.iter().rposition(|&b| b == b'.') {
        Some(i) if i > 0 && i + 1 < name.len() => &name[i + 1..],
        _ => b"",
    }
}

/// Whether the spelling satisfies the core extension contract.
pub(crate) fn extension_is_eligible(name: &[u8]) -> bool {
    name.iter()
        .rposition(|&b| b == b'.')
        .is_some_and(|i| i > 0 && (1..=12).contains(&(name.len() - i - 1)))
}

/// One eligibility bit per row, derived in a sequential pass over spellings.
fn eligible_rows(rows: usize, names: &[u8]) -> Vec<u8> {
    let mut eligible = vec![0u8; rows.div_ceil(8)];
    let mut cursor = 0usize;
    for row in 0..rows {
        let Some(end) = names
            .get(cursor..)
            .and_then(|rest| memchr::memchr(0, rest))
            .map(|end| cursor + end)
        else {
            break;
        };
        if extension_is_eligible(&names[cursor..end]) {
            eligible[row / 8] |= 1 << (row % 8);
        }
        cursor = end + 1;
    }
    eligible
}

#[cfg(test)]
mod tests;
