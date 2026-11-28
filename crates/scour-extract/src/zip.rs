//! A ZIP's directory and its entries' bytes, read under bounds: from a slice
//! already in memory, or from a file by positioned reads. What an entry is for
//! is the caller's question; this says where it is and what it weighs.

use std::fs::File;
#[cfg(not(unix))]
use std::io::{Read as _, Seek as _, SeekFrom};

use memchr::memrchr_iter;

/// A central directory larger than this is refused rather than read.
pub const MAX_CD: usize = 8 << 20;

/// The end-of-directory record sits within the last 64 KiB and 22 bytes.
const TAIL: usize = 65_557;

const LOCAL: [u8; 4] = [0x50, 0x4B, 0x03, 0x04];
const CENTRAL: [u8; 4] = [0x50, 0x4B, 0x01, 0x02];
const END: [u8; 4] = [0x50, 0x4B, 0x05, 0x06];

/// One entry as a directory or a local header names it. `name` is a range in
/// the buffer the entry was parsed from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Entry {
    pub name: (usize, usize),
    pub method: u16,
    pub flags: u16,
    pub comp_size: u64,
    pub uncomp_size: u64,
    pub local_offset: u64,
    /// When it was last changed, as DOS writes it: the date in the high
    /// half, the time in the low.
    pub modified: u32,
}

impl Entry {
    /// The entry's name, out of the buffer it was parsed from.
    pub fn name_in<'a>(&self, names: &'a [u8]) -> &'a [u8] {
        names.get(self.name.0..self.name.1).unwrap_or_default()
    }

    /// Encrypted: nothing here can read it.
    pub fn encrypted(&self) -> bool {
        self.flags & 1 != 0
    }
}

/// Does this look like a ZIP's first local header?
pub fn is_zip(head: &[u8]) -> bool {
    head.len() >= 4 && head[..4] == LOCAL
}

/// The directory's entry count, offset and size, from the file's tail. A
/// ZIP64 directory (offset or size all ones) is refused.
pub fn find_eocd(tail: &[u8]) -> Option<(usize, u64, usize)> {
    if tail.len() < 22 {
        return None;
    }
    for p in memrchr_iter(b'P', &tail[..=tail.len() - 22]) {
        if tail[p..p + 4] == END {
            let count = u16::from_le_bytes([tail[p + 10], tail[p + 11]]) as usize;
            let cd_size = u32::from_le_bytes(tail[p + 12..p + 16].try_into().ok()?) as usize;
            let cd_off = u32::from_le_bytes(tail[p + 16..p + 20].try_into().ok()?) as u64;
            if cd_off == 0xFFFF_FFFF || cd_size == 0xFFFF_FFFF {
                return None;
            }
            return Some((count, cd_off, cd_size));
        }
    }
    None
}

/// Each record of a central directory, at most `max_count`, its name range
/// shifted by `base` — where `cd` starts in the buffer the names are read from.
#[inline]
pub fn central(cd: &[u8], base: usize, max_count: usize, mut each: impl FnMut(Entry)) {
    let mut at = 0usize;
    let mut seen = 0usize;
    while at + 46 <= cd.len() && seen < max_count {
        if cd[at..at + 4] != CENTRAL {
            break;
        }
        let flags = u16::from_le_bytes([cd[at + 8], cd[at + 9]]);
        let method = u16::from_le_bytes([cd[at + 10], cd[at + 11]]);
        let comp_size =
            u32::from_le_bytes([cd[at + 20], cd[at + 21], cd[at + 22], cd[at + 23]]) as u64;
        let uncomp_size =
            u32::from_le_bytes([cd[at + 24], cd[at + 25], cd[at + 26], cd[at + 27]]) as u64;
        let nlen = u16::from_le_bytes([cd[at + 28], cd[at + 29]]) as usize;
        let elen = u16::from_le_bytes([cd[at + 30], cd[at + 31]]) as usize;
        let clen = u16::from_le_bytes([cd[at + 32], cd[at + 33]]) as usize;
        let local_offset =
            u32::from_le_bytes([cd[at + 42], cd[at + 43], cd[at + 44], cd[at + 45]]) as u64;
        // Time then date, little-endian: the date lands in the high half.
        let modified = u32::from_le_bytes([cd[at + 12], cd[at + 13], cd[at + 14], cd[at + 15]]);
        let Some(name_end) = at.checked_add(46).and_then(|x| x.checked_add(nlen)) else {
            break;
        };
        let Some(next) = name_end.checked_add(elen).and_then(|x| x.checked_add(clen)) else {
            break;
        };
        if next > cd.len() {
            break;
        }
        each(Entry {
            name: (base + at + 46, base + name_end),
            method,
            flags,
            comp_size,
            uncomp_size,
            local_offset,
            modified,
        });
        seen += 1;
        at = next;
    }
}

/// The directory of an archive held whole in memory; names index `archive`.
/// False when there is no directory to read.
pub fn central_in(archive: &[u8], each: impl FnMut(Entry)) -> bool {
    let tail_start = archive.len().saturating_sub(TAIL);
    let Some((count, cd_off, cd_size)) = find_eocd(&archive[tail_start..]) else {
        return false;
    };
    let cd_off = cd_off as usize;
    let Some(cd_end) = cd_off.checked_add(cd_size) else {
        return false;
    };
    if cd_size > MAX_CD || cd_end > archive.len() {
        return false;
    }
    let mut any = false;
    let mut each = each;
    central(&archive[cd_off..cd_end], cd_off, count, |e| {
        any = true;
        each(e)
    });
    any
}

/// The entries by their local headers, front to back: for an archive whose
/// directory is missing or cut off. An entry that does not say its size —
/// streamed, flag 8 — is taken to run to the end, and is the last read.
pub fn local_headers(archive: &[u8], mut each: impl FnMut(Entry)) {
    let mut at = 0usize;
    while at + 30 <= archive.len() {
        if archive[at..at + 4] != LOCAL {
            break;
        }
        let flags = u16::from_le_bytes([archive[at + 6], archive[at + 7]]);
        let method = u16::from_le_bytes([archive[at + 8], archive[at + 9]]);
        let comp_size = u32::from_le_bytes([
            archive[at + 18],
            archive[at + 19],
            archive[at + 20],
            archive[at + 21],
        ]) as u64;
        let uncomp_size = u32::from_le_bytes([
            archive[at + 22],
            archive[at + 23],
            archive[at + 24],
            archive[at + 25],
        ]) as u64;
        let nlen = u16::from_le_bytes([archive[at + 26], archive[at + 27]]) as usize;
        let elen = u16::from_le_bytes([archive[at + 28], archive[at + 29]]) as usize;
        let Some(data_start) = at
            .checked_add(30)
            .and_then(|x| x.checked_add(nlen))
            .and_then(|x| x.checked_add(elen))
        else {
            break;
        };
        if data_start > archive.len() {
            break;
        }
        let avail = (archive.len() - data_start) as u64;
        let eff_comp = if comp_size == 0 && flags & 8 != 0 {
            avail
        } else {
            comp_size
        };
        each(Entry {
            name: (at + 30, at + 30 + nlen),
            method,
            flags,
            comp_size: eff_comp,
            uncomp_size,
            local_offset: at as u64,
            // A local header's time is where the central one's is, less 2.
            modified: u32::from_le_bytes([
                archive[at + 10],
                archive[at + 11],
                archive[at + 12],
                archive[at + 13],
            ]),
        });
        if comp_size == 0 {
            break;
        }
        let Some(next) = data_start.checked_add(comp_size as usize) else {
            break;
        };
        at = next;
    }
}

/// An entry's compressed bytes, when `buf` holds them: `buf` starts where
/// the archive does.
#[inline]
pub fn data_in<'a>(buf: &'a [u8], e: &Entry) -> Option<&'a [u8]> {
    let loc = usize::try_from(e.local_offset).ok()?;
    let hdr = buf.get(loc..loc.checked_add(30)?)?;
    if hdr[..4] != LOCAL {
        return None;
    }
    let nlen = u16::from_le_bytes([hdr[26], hdr[27]]) as usize;
    let elen = u16::from_le_bytes([hdr[28], hdr[29]]) as usize;
    let start = loc.checked_add(30)?.checked_add(nlen)?.checked_add(elen)?;
    let end = start.checked_add(usize::try_from(e.comp_size).ok()?)?;
    buf.get(start..end)
}

/// `dst.len()` bytes at `offset`, without moving the file's cursor on Unix.
pub fn read_at(file: &mut File, offset: u64, dst: &mut [u8]) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt as _;
        file.read_exact_at(dst, offset).is_ok()
    }
    #[cfg(not(unix))]
    {
        file.seek(SeekFrom::Start(offset)).is_ok() && file.read_exact(dst).is_ok()
    }
}

/// A file's central directory into `cd_buf`, each entry's name indexing it.
/// Two reads at most: the tail, and the directory when the tail missed it.
/// `read_bytes` grows by what was read. False when nothing was found.
pub fn load_central(
    file: &mut File,
    file_len: u64,
    cd_buf: &mut Vec<u8>,
    read_bytes: &mut u64,
    each: impl FnMut(Entry),
) -> bool {
    let tail_len = file_len.min(TAIL as u64) as usize;
    if tail_len < 22 {
        return false;
    }
    cd_buf.resize(tail_len, 0);
    let tail_off = file_len - tail_len as u64;
    if !read_at(file, tail_off, cd_buf) {
        return false;
    }
    *read_bytes += tail_len as u64;
    let Some((count, cd_off, cd_size)) = find_eocd(cd_buf) else {
        return false;
    };
    if cd_size > MAX_CD || cd_off + cd_size as u64 > file_len {
        return false;
    }
    let mut any = false;
    let mut each = each;
    let mut counted = |e| {
        any = true;
        each(e)
    };
    if cd_off >= tail_off {
        let rel = (cd_off - tail_off) as usize;
        let Some(end) = rel.checked_add(cd_size) else {
            return false;
        };
        if end <= cd_buf.len() {
            central(&cd_buf[rel..end], rel, count, &mut counted);
            return any;
        }
    }
    cd_buf.resize(cd_size, 0);
    if !read_at(file, cd_off, cd_buf) {
        return false;
    }
    *read_bytes += cd_size as u64;
    central(cd_buf, 0, count, &mut counted);
    any
}

/// An entry's compressed bytes, read from the file into `raw`.
pub fn read_raw(
    file: &mut File,
    file_len: u64,
    e: &Entry,
    raw: &mut Vec<u8>,
    read_bytes: &mut u64,
) -> bool {
    if e.local_offset + 30 > file_len {
        return false;
    }
    let mut lhdr = [0u8; 30];
    if !read_at(file, e.local_offset, &mut lhdr) || lhdr[..4] != LOCAL {
        return false;
    }
    *read_bytes += 30;
    let nlen = u16::from_le_bytes([lhdr[26], lhdr[27]]) as u64;
    let elen = u16::from_le_bytes([lhdr[28], lhdr[29]]) as u64;
    let data_off = e.local_offset + 30 + nlen + elen;
    if data_off.saturating_add(e.comp_size) > file_len {
        return false;
    }
    let len = e.comp_size as usize;
    raw.resize(len, 0);
    if !read_at(file, data_off, raw) {
        return false;
    }
    *read_bytes += e.comp_size;
    true
}
