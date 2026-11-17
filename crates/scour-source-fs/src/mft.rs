//! The first walk of an NTFS volume, read from its master file table instead
//! of directory by directory. Every file and directory is a record in `$MFT`:
//! a name and its parent's record number, the times, the size. Read end to end
//! in large sequential reads and joined into paths, a volume is seconds where a
//! walk of PC2's C: was minutes, and it is the same rows the walk gives —
//! `$STANDARD_INFORMATION` is where Windows' own directory listing takes its
//! times from.
//!
//! Read in two passes, so only directories are held: the first keeps every
//! directory's name and parent (and the pieces of any record spread over
//! several), the second reads again and hands each file over as it comes.
//!
//! Written against [`ReadAt`] and nothing of Windows, so the tests below build
//! a volume in memory. Opening one needs an administrator — the service.

use std::collections::HashMap;
use std::io;
use std::sync::Arc;

use scour_core::{EntrySink, Meta, SourceId};

use crate::rules::Rules;

/// A volume, read at offsets. Offsets and lengths are sector multiples: a raw
/// volume handle refuses anything else.
pub(crate) trait ReadAt {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()>;
}

/// The record of the volume's root directory.
const ROOT: u64 = 5;
/// Records below this are the filesystem's own (`$MFT`, `$Bitmap`, …); no
/// directory listing shows them.
const RESERVED: u64 = 24;
/// How much is read at a time.
const CHUNK: usize = 1 << 20;
/// `fixup` strides: NTFS protects every 512 bytes, whatever the sector size.
const STRIDE: usize = 512;

const ATTR_STANDARD: u32 = 0x10;
const ATTR_FILE_NAME: u32 = 0x30;
const ATTR_DATA: u32 = 0x80;
const ATTR_REPARSE: u32 = 0xC0;
const ATTR_END: u32 = 0xFFFF_FFFF;

const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const NAMESPACE_DOS: u8 = 2;
const TAG_MOUNT_POINT: u32 = 0xA000_0003;
const TAG_SYMLINK: u32 = 0xA000_000C;

/// What the boot sector says about where things are.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Geometry {
    pub cluster: u64,
    pub record: usize,
    pub mft_at: u64,
}

pub(crate) fn geometry(volume: &impl ReadAt) -> io::Result<Geometry> {
    let mut boot = vec![0u8; 4096];
    volume.read_at(&mut boot, 0)?;
    if &boot[3..11] != b"NTFS    " {
        return Err(io::Error::other("not an NTFS volume"));
    }
    let sector = u16::from_le_bytes([boot[0x0B], boot[0x0C]]) as u64;
    let per_cluster = boot[0x0D] as u64;
    let cluster = sector * per_cluster;
    let mft_lcn = u64::from_le_bytes(boot[0x30..0x38].try_into().unwrap());
    let per_record = boot[0x40] as i8;
    let record = if per_record < 0 {
        1usize << (-per_record as u32)
    } else {
        per_record as usize * cluster as usize
    };
    if cluster == 0 || record < 256 || !record.is_power_of_two() {
        return Err(io::Error::other("an NTFS boot sector that makes no sense"));
    }
    Ok(Geometry {
        cluster,
        record,
        mft_at: mft_lcn * cluster,
    })
}

/// Undo the update sequence: the last two bytes of every 512 are kept in the
/// array at `usa_off` and replaced by a check value. False for a torn record.
pub(crate) fn fixup(rec: &mut [u8]) -> bool {
    if rec.len() < 48 {
        return false;
    }
    let off = u16::from_le_bytes([rec[4], rec[5]]) as usize;
    let count = u16::from_le_bytes([rec[6], rec[7]]) as usize;
    if count == 0 || off + count * 2 > rec.len() || (count - 1) * STRIDE > rec.len() {
        return false;
    }
    let check = [rec[off], rec[off + 1]];
    for i in 1..count {
        let end = i * STRIDE - 2;
        if rec[end..end + 2] != check {
            return false;
        }
        let saved = [rec[off + i * 2], rec[off + i * 2 + 1]];
        rec[end..end + 2].copy_from_slice(&saved);
    }
    true
}

/// A non-resident attribute's extents: `(first cluster, clusters)`, `None`
/// for a sparse stretch.
pub(crate) fn runs(mut data: &[u8]) -> Vec<(Option<u64>, u64)> {
    let mut out = Vec::new();
    let mut lcn: i64 = 0;
    while let Some(&head) = data.first() {
        if head == 0 {
            break;
        }
        let (len_n, off_n) = ((head & 0x0F) as usize, (head >> 4) as usize);
        if len_n == 0 || len_n > 8 || off_n > 8 || data.len() < 1 + len_n + off_n {
            break;
        }
        let mut len = 0u64;
        for (i, b) in data[1..1 + len_n].iter().enumerate() {
            len |= (*b as u64) << (8 * i);
        }
        let start = if off_n == 0 {
            None
        } else {
            let raw = &data[1 + len_n..1 + len_n + off_n];
            let mut delta = 0i64;
            for (i, b) in raw.iter().enumerate() {
                delta |= (*b as i64) << (8 * i);
            }
            // Sign-extend from the top byte given.
            let shift = 64 - 8 * off_n as u32;
            delta = (delta << shift) >> shift;
            lcn += delta;
            Some(lcn as u64)
        };
        out.push((start, len));
        data = &data[1 + len_n + off_n..];
    }
    out
}

/// One record, read: what a file's row needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Parsed {
    pub in_use: bool,
    pub dir: bool,
    /// The record this one extends, for a file whose attributes did not fit.
    pub base: u64,
    /// `(parent record, name)`, one per hard link; DOS short names left out.
    pub names: Vec<(u64, String)>,
    pub size: Option<u64>,
    /// `(modified, created, accessed)` as unix seconds.
    pub times: Option<(i64, i64, i64)>,
    pub attrs: u32,
    /// A symbolic link or a junction: a walk shows it and does not go in.
    pub link: bool,
    /// The record holds a `$DATA` beyond its first extent, or none at all yet.
    pub data_seen: bool,
}

impl Parsed {
    fn absorb(&mut self, ext: &Parsed) {
        for n in &ext.names {
            if !self.names.contains(n) {
                self.names.push(n.clone());
            }
        }
        if self.size.is_none() {
            self.size = ext.size;
        }
        if self.times.is_none() {
            self.times = ext.times;
        }
        self.link |= ext.link;
    }
}

fn unix(filetime: i64) -> i64 {
    // 100 ns ticks since 1601 to seconds since 1970.
    (filetime - 116_444_736_000_000_000).div_euclid(10_000_000)
}

fn u16_at(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// A record already through [`fixup`]. None for one that is not a record.
pub(crate) fn parse(rec: &[u8]) -> Option<Parsed> {
    if rec.get(0..4)? != b"FILE" {
        return None;
    }
    let flags = u16_at(rec, 22)?;
    let mut p = Parsed {
        in_use: flags & 1 != 0,
        dir: flags & 2 != 0,
        base: u64_at(rec, 32)? & 0x0000_FFFF_FFFF_FFFF,
        ..Parsed::default()
    };
    let used = (u32_at(rec, 24)? as usize).min(rec.len());
    let mut at = u16_at(rec, 20)? as usize;
    let mut dos: Vec<(u64, String)> = Vec::new();
    while at + 16 <= used {
        let kind = u32_at(rec, at)?;
        if kind == ATTR_END {
            break;
        }
        let len = u32_at(rec, at + 4)? as usize;
        if len < 16 || at + len > used {
            break;
        }
        let attr = &rec[at..at + len];
        let resident = attr[8] == 0;
        let named = attr[9] != 0;
        let value = if resident {
            let vlen = u32_at(attr, 16)? as usize;
            let voff = u16_at(attr, 20)? as usize;
            attr.get(voff..voff + vlen)
        } else {
            None
        };
        match kind {
            ATTR_STANDARD => {
                if let Some(v) = value.filter(|v| v.len() >= 36) {
                    p.times = Some((
                        unix(u64_at(v, 8)? as i64),
                        unix(u64_at(v, 0)? as i64),
                        unix(u64_at(v, 24)? as i64),
                    ));
                    p.attrs = u32_at(v, 32)?;
                }
            }
            ATTR_FILE_NAME => {
                if let Some(v) = value.filter(|v| v.len() >= 66) {
                    let parent = u64_at(v, 0)? & 0x0000_FFFF_FFFF_FFFF;
                    let n = v[64] as usize;
                    let ns = v[65];
                    if let Some(raw) = v.get(66..66 + n * 2) {
                        let units: Vec<u16> = raw
                            .chunks_exact(2)
                            .map(|c| u16::from_le_bytes([c[0], c[1]]))
                            .collect();
                        let name = String::from_utf16_lossy(&units);
                        if ns == NAMESPACE_DOS {
                            dos.push((parent, name));
                        } else if !p.names.contains(&(parent, name.clone())) {
                            p.names.push((parent, name));
                        }
                    }
                }
            }
            ATTR_DATA if !named => {
                p.data_seen = true;
                if resident {
                    p.size = Some(u32_at(attr, 16)? as u64);
                } else if u64_at(attr, 16)? == 0 {
                    // Only the first extent carries the sizes.
                    p.size = Some(u64_at(attr, 48)?);
                }
            }
            ATTR_REPARSE => {
                let tag = value.and_then(|v| u32_at(v, 0));
                p.link = matches!(tag, Some(TAG_SYMLINK | TAG_MOUNT_POINT));
            }
            _ => {}
        }
        at += len;
    }
    // A name only DOS knows is still a name: never seen, but not worth losing.
    if p.names.is_empty() {
        p.names = dos;
    }
    Some(p)
}

/// The `$MFT`'s own extents and length, from its record — the first one.
fn mft_extents(volume: &impl ReadAt, g: &Geometry) -> io::Result<(Vec<(u64, u64)>, u64)> {
    let mut first = vec![0u8; g.record.max(4096)];
    volume.read_at(&mut first, g.mft_at)?;
    let rec = &mut first[..g.record];
    if !fixup(rec) {
        return Err(io::Error::other("the MFT's own record is torn"));
    }
    let used = (u32_at(rec, 24).unwrap_or(0) as usize).min(rec.len());
    let mut at = u16_at(rec, 20).unwrap_or(0) as usize;
    while at + 16 <= used {
        let kind = u32_at(rec, at).unwrap_or(ATTR_END);
        let len = u32_at(rec, at + 4).unwrap_or(0) as usize;
        if kind == ATTR_END || len < 16 || at + len > used {
            break;
        }
        let attr = &rec[at..at + len];
        if kind == ATTR_DATA && attr[8] != 0 && attr[9] == 0 {
            let size = u64_at(attr, 48).unwrap_or(0);
            let off = u16_at(attr, 32).unwrap_or(0) as usize;
            let extents = runs(attr.get(off..).unwrap_or(&[]))
                .into_iter()
                .filter_map(|(lcn, n)| lcn.map(|l| (l * g.cluster, n * g.cluster)))
                .collect();
            return Ok((extents, size));
        }
        at += len;
    }
    Err(io::Error::other("the MFT has no data"))
}

/// Every record of the volume, in order, handed to `each` as
/// `(record number, parsed)`. Torn and unused records are left out.
fn each_record(
    volume: &impl ReadAt,
    g: &Geometry,
    mut each: impl FnMut(u64, Parsed) -> bool,
) -> io::Result<()> {
    let (extents, size) = mft_extents(volume, g)?;
    let total = size / g.record as u64;
    let mut number = 0u64;
    let mut carry: Vec<u8> = Vec::with_capacity(CHUNK + g.record);
    let mut buf = vec![0u8; CHUNK];
    for (start, len) in extents {
        let mut done = 0u64;
        while done < len && number < total {
            let n = ((len - done) as usize).min(CHUNK);
            volume.read_at(&mut buf[..n], start + done)?;
            done += n as u64;
            carry.extend_from_slice(&buf[..n]);
            let whole = carry.len() / g.record * g.record;
            for rec in carry[..whole].chunks_exact_mut(g.record) {
                if number >= total {
                    break;
                }
                let at = number;
                number += 1;
                if !fixup(rec) {
                    continue;
                }
                if let Some(p) = parse(rec).filter(|p| p.in_use)
                    && !each(at, p)
                {
                    return Ok(());
                }
            }
            carry.drain(..whole);
        }
    }
    Ok(())
}

/// A directory, as the first pass keeps it.
struct Dir {
    parent: u64,
    name: String,
    hidden: bool,
}

/// Where a directory stands once the rules have seen it.
#[derive(Clone)]
enum Place {
    /// Excluded, and nothing below it is wanted.
    Pruned,
    /// Excluded itself, but an allowed path lives below it.
    Through(Arc<str>),
    Shown(Arc<str>),
}

/// What the read found, for the scan's report.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Counts {
    pub entries: u64,
    pub dirs: u64,
    pub excluded: u64,
    pub stopped: bool,
}

/// Read the whole volume into `sink`, as a walk of `root` — the volume's root,
/// `C:/` — would. `hidden` false leaves out what Windows hides.
pub(crate) fn read(
    volume: &impl ReadAt,
    source: SourceId,
    root: &str,
    rules: &Rules,
    hidden: bool,
    sink: &mut dyn EntrySink,
) -> io::Result<Counts> {
    let g = geometry(volume)?;

    // Pass one: directories, and the pieces of records spread over several.
    let mut dirs: HashMap<u64, Dir> = HashMap::new();
    let mut root_times = None;
    let mut ext: HashMap<u64, Parsed> = HashMap::new();
    let mut spread: Vec<(u64, Parsed)> = Vec::new();
    let mut stopped = false;
    let listening = &*sink;
    each_record(volume, &g, |n, p| {
        // Nothing reaches the sink in this pass, so it is asked outright.
        if n % 65_536 == 0 && listening.stopping() {
            stopped = true;
            return false;
        }
        if p.base != 0 {
            ext.entry(p.base).or_default().absorb(&p);
        } else if n == ROOT {
            root_times = p.times;
        } else if p.dir && !p.link && n >= RESERVED {
            spread.push((n, p));
        }
        true
    })?;
    for (n, mut p) in spread {
        if let Some(e) = ext.get(&n) {
            p.absorb(e);
        }
        if let Some((parent, name)) = p.names.first() {
            dirs.insert(
                n,
                Dir {
                    parent: *parent,
                    name: name.clone(),
                    hidden: p.attrs & FILE_ATTRIBUTE_HIDDEN != 0,
                },
            );
        }
    }

    let mut counts = Counts::default();
    if stopped {
        counts.stopped = true;
        return Ok(counts);
    }
    let mut places: HashMap<u64, Place> = HashMap::new();
    let root_path: Arc<str> = Arc::from(root);
    places.insert(ROOT, Place::Shown(Arc::clone(&root_path)));
    let skip = |name: &str, attrs_hidden: bool| !hidden && (attrs_hidden || name.starts_with('.'));

    let place_of =
        |want: u64, places: &mut HashMap<u64, Place>, counts: &mut Counts| -> Option<Place> {
            // Up to the first directory already placed, then back down.
            let mut chain = Vec::new();
            let mut at = want;
            while !places.contains_key(&at) {
                if chain.len() > 4096 || chain.contains(&at) {
                    return None;
                }
                dirs.get(&at)?;
                chain.push(at);
                at = dirs[&at].parent;
            }
            for n in chain.into_iter().rev() {
                let d = &dirs[&n];
                let place = match places[&d.parent].clone() {
                    Place::Pruned => Place::Pruned,
                    Place::Through(parent) | Place::Shown(parent) => {
                        let path: Arc<str> = Arc::from(join(&parent, &d.name));
                        if skip(&d.name, d.hidden)
                            || (!rules.is_empty() && rules.excludes(&path, &d.name, true))
                        {
                            counts.excluded += 1;
                            if rules.may_contain_allowed(&path) {
                                Place::Through(path)
                            } else {
                                Place::Pruned
                            }
                        } else {
                            Place::Shown(path)
                        }
                    }
                };
                places.insert(n, place);
            }
            places.get(&want).cloned()
        };

    // The root's own row, as a walk's first.
    let push = |sink: &mut dyn EntrySink,
                counts: &mut Counts,
                path: &str,
                is_dir: bool,
                size: u64,
                times: Option<(i64, i64, i64)>|
     -> bool {
        let (mtime, ctime, atime) = times.unwrap_or((0, 0, 0));
        let meta = Meta {
            size: if is_dir { 0 } else { size as i64 },
            mtime,
            ctime,
            atime,
            mode: 0,
            uid: 0,
            gid: 0,
            disk: if is_dir { 0 } else { size as i64 },
            items: -1,
            links: 1,
        };
        counts.entries += 1;
        if is_dir {
            counts.dirs += 1;
        }
        let entry = scour_core::Entry {
            id: scour_core::EntryId::path_hash(source, path),
            path: path.to_owned(),
            is_dir,
            meta,
        };
        if sink.push(entry).is_stop() {
            counts.stopped = true;
            return false;
        }
        true
    };
    if !push(&mut *sink, &mut counts, root, true, 0, root_times) {
        return Ok(counts);
    }

    // Pass two: every row, as it comes.
    each_record(volume, &g, |n, mut p| {
        if p.base != 0 || n < RESERVED {
            return true;
        }
        if let Some(e) = ext.get(&n) {
            p.absorb(e);
        }
        let is_dir = p.dir && !p.link;
        for (parent, name) in &p.names {
            let (Some(Place::Shown(at)) | Some(Place::Through(at))) =
                place_of(*parent, &mut places, &mut counts)
            else {
                continue;
            };
            if is_dir {
                // A directory's own row comes from where it was placed, once.
                if let Some(Place::Shown(path)) = place_of(n, &mut places, &mut counts)
                    && !push(&mut *sink, &mut counts, &path, true, 0, p.times)
                {
                    return false;
                }
                break;
            }
            let path = join(&at, name);
            if skip(name, p.attrs & FILE_ATTRIBUTE_HIDDEN != 0)
                || (!rules.is_empty() && rules.excludes(&path, name, false))
            {
                counts.excluded += 1;
                continue;
            }
            if !push(
                &mut *sink,
                &mut counts,
                &path,
                false,
                p.size.unwrap_or(0),
                p.times,
            ) {
                return false;
            }
        }
        true
    })?;
    Ok(counts)
}

fn join(parent: &str, name: &str) -> String {
    if parent.ends_with('/') {
        format!("{parent}{name}")
    } else {
        format!("{parent}/{name}")
    }
}

/// The volume behind a root that is a whole drive, `C:/`, opened for reading.
/// None for anything else, or when this process may not open it.
#[cfg(windows)]
pub(crate) fn open(root: &str) -> Option<std::fs::File> {
    use std::os::windows::fs::OpenOptionsExt;
    let b = root.as_bytes();
    if !(root.len() == 3 && b[0].is_ascii_alphabetic() && &b[1..] == b":/") {
        return None;
    }
    // FILE_SHARE_READ | FILE_SHARE_WRITE: the volume is in use.
    std::fs::OpenOptions::new()
        .read(true)
        .share_mode(0x1 | 0x2)
        .open(format!(r"\\.\{}:", &root[..1]))
        .ok()
}

#[cfg(windows)]
impl ReadAt for std::fs::File {
    fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
        use std::os::windows::fs::FileExt;
        let mut done = 0;
        while done < buf.len() {
            let n = self.seek_read(&mut buf[done..], offset + done as u64)?;
            if n == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            done += n;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use scour_core::{Entry, Flow};

    const RECORD: usize = 1024;
    const CLUSTER: u64 = 4096;

    impl ReadAt for Vec<u8> {
        fn read_at(&self, buf: &mut [u8], offset: u64) -> io::Result<()> {
            let at = offset as usize;
            let src = self
                .get(at..at + buf.len())
                .ok_or(io::ErrorKind::UnexpectedEof)?;
            buf.copy_from_slice(src);
            Ok(())
        }
    }

    fn ft(unix: i64) -> u64 {
        (unix * 10_000_000 + 116_444_736_000_000_000) as u64
    }

    /// An attribute, resident unless `runs` is given.
    fn attr(kind: u32, name: &str, value: &[u8]) -> Vec<u8> {
        let name16: Vec<u8> = name.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let voff = (24 + name16.len()).div_ceil(8) * 8;
        let len = (voff + value.len()).div_ceil(8) * 8;
        let mut a = vec![0u8; len];
        a[0..4].copy_from_slice(&kind.to_le_bytes());
        a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        a[9] = (name16.len() / 2) as u8;
        a[10..12].copy_from_slice(&24u16.to_le_bytes());
        a[16..20].copy_from_slice(&(value.len() as u32).to_le_bytes());
        a[20..22].copy_from_slice(&(voff as u16).to_le_bytes());
        a[24..24 + name16.len()].copy_from_slice(&name16);
        a[voff..voff + value.len()].copy_from_slice(value);
        a
    }

    fn nonresident_data(size: u64, runs: &[u8]) -> Vec<u8> {
        let len = (64 + runs.len()).div_ceil(8) * 8;
        let mut a = vec![0u8; len];
        a[0..4].copy_from_slice(&ATTR_DATA.to_le_bytes());
        a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        a[8] = 1;
        a[32..34].copy_from_slice(&64u16.to_le_bytes());
        a[40..48].copy_from_slice(&size.to_le_bytes());
        a[48..56].copy_from_slice(&size.to_le_bytes());
        a[64..64 + runs.len()].copy_from_slice(runs);
        a
    }

    fn standard(mtime: i64, attrs: u32) -> Vec<u8> {
        let mut v = vec![0u8; 48];
        v[0..8].copy_from_slice(&ft(mtime - 100).to_le_bytes());
        v[8..16].copy_from_slice(&ft(mtime).to_le_bytes());
        v[24..32].copy_from_slice(&ft(mtime + 5).to_le_bytes());
        v[32..36].copy_from_slice(&attrs.to_le_bytes());
        attr(ATTR_STANDARD, "", &v)
    }

    fn file_name(parent: u64, name: &str, ns: u8) -> Vec<u8> {
        let units: Vec<u8> = name.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let mut v = vec![0u8; 66 + units.len()];
        v[0..8].copy_from_slice(&(parent | (1 << 48)).to_le_bytes());
        v[64] = (units.len() / 2) as u8;
        v[65] = ns;
        v[66..].copy_from_slice(&units);
        attr(ATTR_FILE_NAME, "", &v)
    }

    /// A record with its update sequence applied, as on disk.
    fn record(flags: u16, base: u64, attrs: &[Vec<u8>]) -> Vec<u8> {
        let mut r = vec![0u8; RECORD];
        r[0..4].copy_from_slice(b"FILE");
        r[4..6].copy_from_slice(&48u16.to_le_bytes());
        r[6..8].copy_from_slice(&3u16.to_le_bytes()); // check value + 2 strides
        r[20..22].copy_from_slice(&56u16.to_le_bytes());
        r[22..24].copy_from_slice(&flags.to_le_bytes());
        r[32..40].copy_from_slice(&base.to_le_bytes());
        let mut at = 56;
        for a in attrs {
            r[at..at + a.len()].copy_from_slice(a);
            at += a.len();
        }
        r[at..at + 4].copy_from_slice(&ATTR_END.to_le_bytes());
        at += 8;
        r[24..28].copy_from_slice(&(at as u32).to_le_bytes());
        // The check value goes over the last two bytes of each stride.
        let check = [0x2A, 0x00];
        r[48..50].copy_from_slice(&check);
        for i in 1..3 {
            let end = i * STRIDE - 2;
            let kept = [r[end], r[end + 1]];
            r[48 + i * 2..50 + i * 2].copy_from_slice(&kept);
            r[end..end + 2].copy_from_slice(&check);
        }
        r
    }

    const IN_USE: u16 = 1;
    const DIR: u16 = 3;

    /// A volume: a boot sector, and an MFT in two extents so a run list and a
    /// split are both read. Records by number; missing ones are empty.
    fn volume(records: &[(u64, Vec<u8>)]) -> Vec<u8> {
        let count = 64u64;
        // Records 0..32 at cluster 16, 32..64 at cluster 40.
        let per_cluster = CLUSTER / RECORD as u64;
        let half = 32 / per_cluster;
        let mut runs = vec![0x11, half as u8, 16];
        runs.extend([0x11, half as u8, 24]); // +24 from 16 → 40
        runs.push(0);
        let mut mft = vec![0u8; (count as usize) * RECORD];
        let mut zero = record(
            IN_USE,
            0,
            &[
                standard(0, 0),
                file_name(ROOT, "$MFT", 3),
                nonresident_data(count * RECORD as u64, &runs),
            ],
        );
        mft[..RECORD].swap_with_slice(&mut zero);
        for (n, r) in records {
            let at = *n as usize * RECORD;
            mft[at..at + RECORD].copy_from_slice(r);
        }
        let mut img = vec![0u8; 60 * CLUSTER as usize];
        img[3..11].copy_from_slice(b"NTFS    ");
        img[0x0B..0x0D].copy_from_slice(&512u16.to_le_bytes());
        img[0x0D] = 8;
        img[0x30..0x38].copy_from_slice(&16u64.to_le_bytes());
        img[0x40] = (-10i8) as u8; // 2^10 = 1024
        let split = 32 * RECORD;
        img[16 * CLUSTER as usize..16 * CLUSTER as usize + split].copy_from_slice(&mft[..split]);
        img[40 * CLUSTER as usize..40 * CLUSTER as usize + split].copy_from_slice(&mft[split..]);
        img
    }

    #[derive(Default)]
    struct Rows(Vec<Entry>);
    impl EntrySink for Rows {
        fn push(&mut self, e: Entry) -> Flow {
            self.0.push(e);
            Flow::Continue
        }
    }

    fn sample() -> Vec<u8> {
        let t = 1_700_000_000;
        volume(&[
            (
                ROOT,
                record(DIR, 0, &[standard(t, 0), file_name(ROOT, ".", 3)]),
            ),
            (
                24,
                record(
                    DIR,
                    0,
                    &[
                        standard(t, 0),
                        file_name(ROOT, "Users", 1),
                        file_name(ROOT, "USERS~1", 2),
                    ],
                ),
            ),
            (
                25,
                record(DIR, 0, &[standard(t, 0), file_name(24, "hasan", 3)]),
            ),
            (
                26,
                record(
                    IN_USE,
                    0,
                    &[
                        standard(t + 1, 0),
                        file_name(25, "rapor.pdf", 3),
                        attr(ATTR_DATA, "", &[7u8; 300]),
                    ],
                ),
            ),
            // A large file, its data elsewhere; and a second name for it.
            (
                27,
                record(
                    IN_USE,
                    0,
                    &[
                        standard(t + 2, 0),
                        file_name(25, "büyük.iso", 1),
                        file_name(ROOT, "link.iso", 1),
                        nonresident_data(5_000_000_000, &[0x11, 1, 50, 0]),
                    ],
                ),
            ),
            // Deleted: not in use.
            (
                28,
                record(0, 0, &[standard(t, 0), file_name(25, "silinmiş.txt", 3)]),
            ),
            // A junction: shown, not entered.
            (
                29,
                record(
                    DIR,
                    0,
                    &[
                        standard(t, 0),
                        file_name(ROOT, "Documents and Settings", 3),
                        attr(ATTR_REPARSE, "", &TAG_MOUNT_POINT.to_le_bytes()),
                    ],
                ),
            ),
            // Excluded by the rules below, with something inside.
            (
                30,
                record(DIR, 0, &[standard(t, 0), file_name(ROOT, "Windows", 3)]),
            ),
            (
                31,
                record(
                    IN_USE,
                    0,
                    &[standard(t, 0), file_name(30, "explorer.exe", 3)],
                ),
            ),
            // Hidden.
            (
                32,
                record(
                    IN_USE,
                    0,
                    &[
                        standard(t, FILE_ATTRIBUTE_HIDDEN),
                        file_name(ROOT, "pagefile.sys", 3),
                    ],
                ),
            ),
            // A record whose name lives in an extension record.
            (
                33,
                record(IN_USE, 0, &[standard(t, 0), attr(ATTR_DATA, "", b"abc")]),
            ),
            (40, record(IN_USE, 33, &[file_name(25, "parçalı.txt", 3)])),
            // A reserved record nobody lists.
            (
                11,
                record(DIR, 0, &[standard(t, 0), file_name(ROOT, "$Extend", 3)]),
            ),
        ])
    }

    fn rows(img: &Vec<u8>, rules: &Rules, hidden: bool) -> (Vec<(String, bool, i64)>, Counts) {
        let mut sink = Rows::default();
        let counts = read(img, SourceId(1), "C:/", rules, hidden, &mut sink).expect("a read");
        let mut out: Vec<_> = sink
            .0
            .into_iter()
            .map(|e| (e.path, e.is_dir, e.meta.size))
            .collect();
        out.sort();
        (out, counts)
    }

    #[test]
    fn a_volume_reads_as_a_walk_would_list_it() {
        let img = sample();
        let (got, counts) = rows(&img, &Rules::from_options(&Default::default()), true);
        let want: Vec<(String, bool, i64)> = vec![
            ("C:/".into(), true, 0),
            ("C:/Documents and Settings".into(), false, 0),
            ("C:/Users".into(), true, 0),
            ("C:/Users/hasan".into(), true, 0),
            ("C:/Users/hasan/büyük.iso".into(), false, 5_000_000_000),
            ("C:/Users/hasan/parçalı.txt".into(), false, 3),
            ("C:/Users/hasan/rapor.pdf".into(), false, 300),
            ("C:/Windows".into(), true, 0),
            ("C:/Windows/explorer.exe".into(), false, 0),
            ("C:/link.iso".into(), false, 5_000_000_000),
            ("C:/pagefile.sys".into(), false, 0),
        ];
        assert_eq!(got, want);
        assert_eq!(counts.entries, want.len() as u64);
        assert_eq!(counts.dirs, 4);
    }

    #[test]
    fn rules_and_hidden_files_are_kept_out_as_in_a_walk() {
        let img = sample();
        let opts = scour_core::ScanOptions {
            exclude_paths: vec!["C:/Windows".into()],
            ..Default::default()
        };
        let (got, counts) = rows(&img, &Rules::from_options(&opts), false);
        let paths: Vec<&str> = got.iter().map(|r| r.0.as_str()).collect();
        assert!(
            !paths.iter().any(|p| p.starts_with("C:/Windows")),
            "{paths:?}"
        );
        assert!(
            !paths.contains(&"C:/pagefile.sys"),
            "a hidden file was listed"
        );
        assert!(paths.contains(&"C:/Users/hasan/rapor.pdf"));
        assert!(counts.excluded >= 2);
    }

    #[test]
    fn times_come_from_the_standard_information() {
        let img = sample();
        let mut sink = Rows::default();
        read(
            &img,
            SourceId(1),
            "C:/",
            &Rules::from_options(&Default::default()),
            true,
            &mut sink,
        )
        .expect("a read");
        let pdf = sink
            .0
            .iter()
            .find(|e| e.path.ends_with("rapor.pdf"))
            .expect("the pdf");
        assert_eq!(pdf.meta.mtime, 1_700_000_001);
        assert_eq!(pdf.meta.ctime, 1_700_000_001 - 100);
        assert_eq!(pdf.meta.atime, 1_700_000_001 + 5);
    }

    /// A walk told to stop does not read the volume through first.
    #[test]
    fn a_stop_is_heard_before_any_row() {
        struct Stopping(u64);
        impl EntrySink for Stopping {
            fn push(&mut self, _e: Entry) -> Flow {
                self.0 += 1;
                Flow::Continue
            }
            fn stopping(&self) -> bool {
                true
            }
        }
        let img = sample();
        let mut sink = Stopping(0);
        let counts = read(
            &img,
            SourceId(1),
            "C:/",
            &Rules::from_options(&Default::default()),
            true,
            &mut sink,
        )
        .expect("a read");
        assert!(counts.stopped);
        assert_eq!(sink.0, 0);
    }

    #[test]
    fn a_torn_record_is_left_out() {
        let mut r = record(IN_USE, 0, &[standard(0, 0), file_name(ROOT, "x", 3)]);
        assert!(fixup(&mut r.clone()));
        r[STRIDE - 2] ^= 0xFF;
        assert!(!fixup(&mut r));
    }

    #[test]
    fn run_lists_decode_with_signed_offsets_and_sparse_stretches() {
        // 4 clusters at 100, 2 sparse, 3 at 100 - 20 = 80.
        let data = [0x21, 4, 100, 0, 0x01, 2, 0x11, 3, 0xEC, 0];
        assert_eq!(runs(&data), vec![(Some(100), 4), (None, 2), (Some(80), 3)]);
    }

    #[test]
    fn not_ntfs_is_refused() {
        let img = vec![0u8; 8192];
        assert!(geometry(&img).is_err());
    }
}
