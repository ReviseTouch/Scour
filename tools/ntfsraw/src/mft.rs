//! Every file's extents from the MFT itself, read once from start to end:
//! no file is opened. Names and parents give each record its path; the
//! unnamed `$DATA` attribute gives its runs, or its bytes when it is
//! resident. Anything harder — an attribute list, compressed, encrypted or
//! sparse data, an initialized size short of the real one — is read the
//! normal way.

use std::collections::HashMap;
use std::time::Instant;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Ioctl::NTFS_VOLUME_DATA_BUFFER;

use crate::win::{BLOCK, PIECE, Piece, SPAN, aligned, fixup, hash_range, read_at};

pub struct Gathered {
    pub pieces: Vec<Piece>,
    pub normal_way: Vec<usize>,
    pub sizes: Vec<u64>,
    pub sums: HashMap<usize, u64>,
    pub placed: usize,
    pub in_mft: usize,
    pub empty: usize,
    pub mft_bytes: u64,
    pub mft_seconds: f64,
}

/// What one record says of its file.
#[derive(Default)]
struct Record {
    parent: u32,
    name: String,
    /// The name is the DOS short one: kept only if there is no other.
    short: bool,
    data: Data,
    size: u64,
}

#[derive(Default)]
enum Data {
    #[default]
    None,
    Resident(Vec<u8>),
    Runs(Vec<(u64, u64, u64)>),
    /// Placed where this reader does not follow: read the normal way.
    Hard,
}

const ROOT: u32 = 5;

pub fn gather(volume: HANDLE, data: &NTFS_VOLUME_DATA_BUFFER, paths: &[String]) -> Gathered {
    let began = Instant::now();
    let cluster = data.BytesPerCluster as u64;
    let size = data.BytesPerFileRecordSegment as usize;
    let valid = data.MftValidDataLength as u64;
    let mft_runs = mft_runs(volume, data, size);
    let mut records: Vec<Option<Record>> = Vec::new();
    let buf = aligned(SPAN as usize);
    let mut read = 0u64;
    let mut number = 0u64;
    'runs: for (vcn, lcn, n) in mft_runs {
        let mut done = 0u64;
        while done < n {
            let take = (n - done).min(SPAN / cluster);
            let file_off = (vcn + done) * cluster;
            if file_off >= valid {
                break 'runs;
            }
            let len = (take * cluster) as usize;
            read_at(volume, (lcn + done) * cluster, &mut buf[..len]);
            read += len as u64;
            for chunk in buf[..len].chunks_exact_mut(size) {
                if number * size as u64 >= valid {
                    break 'runs;
                }
                records.push(parse(chunk, cluster));
                number += 1;
            }
            done += take;
        }
    }
    let mft_seconds = began.elapsed().as_secs_f64();

    let letter: String = paths[0].chars().take(2).collect();
    let mut wanted: HashMap<String, usize> = HashMap::with_capacity(paths.len());
    for (i, p) in paths.iter().enumerate() {
        wanted.insert(p.to_lowercase(), i);
    }
    let mut out = Gathered {
        pieces: Vec::new(),
        normal_way: Vec::new(),
        sizes: vec![0; paths.len()],
        sums: HashMap::new(),
        placed: 0,
        in_mft: 0,
        empty: 0,
        mft_bytes: read,
        mft_seconds,
    };
    let mut found = vec![false; paths.len()];
    let mut dirs: HashMap<u32, Option<String>> = HashMap::new();
    for (n, rec) in records.iter().enumerate() {
        let Some(rec) = rec else { continue };
        if matches!(rec.data, Data::None) && rec.size == 0 && rec.name.is_empty() {
            continue;
        }
        let Some(dir) = dir_path(rec.parent, &records, &mut dirs, &letter) else {
            continue;
        };
        let path = format!("{dir}\\{}", rec.name).to_lowercase();
        let Some(&i) = wanted.get(&path) else { continue };
        found[i] = true;
        let _ = n;
        out.sizes[i] = rec.size;
        match &rec.data {
            _ if rec.size == 0 => {
                out.empty += 1;
                out.sums.insert(i, 0);
            }
            Data::Resident(bytes) if bytes.len() as u64 == rec.size => {
                out.in_mft += 1;
                let mut sum = 0;
                hash_range(&mut sum, bytes, 0, rec.size);
                out.sums.insert(i, sum);
            }
            Data::Runs(runs) if runs.iter().map(|r| r.2).sum::<u64>() * cluster >= rec.size => {
                out.placed += 1;
                for &(vcn, lcn, n) in runs {
                    let mut done = 0u64;
                    while done < n {
                        let take = (n - done).min(PIECE / cluster);
                        out.pieces.push(Piece {
                            file: i,
                            lcn: lcn + done,
                            clusters: take,
                            file_offset: (vcn + done) * cluster,
                        });
                        done += take;
                    }
                }
            }
            _ => out.normal_way.push(i),
        }
    }
    for (i, f) in found.iter().enumerate() {
        if !f {
            out.normal_way.push(i);
        }
    }
    let _ = BLOCK;
    out
}

/// The full path of a directory record, cached; None when a link in the
/// chain is missing or not in use.
fn dir_path(
    n: u32,
    records: &[Option<Record>],
    cache: &mut HashMap<u32, Option<String>>,
    letter: &str,
) -> Option<String> {
    if n == ROOT {
        return Some(letter.to_string());
    }
    if let Some(known) = cache.get(&n) {
        return known.clone();
    }
    let rec = records.get(n as usize)?.as_ref()?;
    // A cycle in a damaged MFT would loop for ever: a depth guard.
    if rec.parent == n {
        cache.insert(n, None);
        return None;
    }
    let parent = dir_path(rec.parent, records, cache, letter);
    let path = parent.map(|p| format!("{p}\\{}", rec.name));
    cache.insert(n, path.clone());
    path
}

/// The `$MFT`'s own runs, from its record, number 0.
fn mft_runs(volume: HANDLE, data: &NTFS_VOLUME_DATA_BUFFER, size: usize) -> Vec<(u64, u64, u64)> {
    let cluster = data.BytesPerCluster as u64;
    let lcn = data.MftStartLcn as u64;
    let span = (size as u64).div_ceil(cluster) * cluster;
    let buf = aligned(span as usize);
    read_at(volume, lcn * cluster, &mut buf[..span as usize]);
    match parse(&mut buf[..size], cluster) {
        Some(Record { data: Data::Runs(runs), .. }) => runs,
        _ => panic!("the MFT's own record could not be read"),
    }
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

/// One record: in use, a base record, with its first long name and its data.
fn parse(rec: &mut [u8], cluster: u64) -> Option<Record> {
    if rec.get(0..4)? != b"FILE" {
        return None;
    }
    fixup(rec);
    let flags = u16_at(rec, 0x16)?;
    if flags & 1 == 0 || u64_at(rec, 0x20)? & 0x0000_FFFF_FFFF_FFFF != 0 {
        return None;
    }
    let directory = flags & 2 != 0;
    let mut out = Record::default();
    let mut at = u16_at(rec, 0x14)? as usize;
    while at + 16 <= rec.len() {
        let kind = u32_at(rec, at)?;
        if kind == 0xFFFF_FFFF {
            break;
        }
        let len = u32_at(rec, at + 4)? as usize;
        if len == 0 || at + len > rec.len() {
            break;
        }
        let nonresident = rec[at + 8] != 0;
        let name_len = rec[at + 9];
        let aflags = u16_at(rec, at + 0x0C)?;
        match kind {
            // An attribute list: the data may be in another record.
            0x20 => out.data = Data::Hard,
            0x30 if !nonresident => {
                let voff = u16_at(rec, at + 0x14)? as usize;
                let v = rec.get(at + voff..at + len)?;
                let parent = (u64_at(v, 0)? & 0x0000_FFFF_FFFF_FFFF) as u32;
                let nlen = *v.get(0x40)? as usize;
                let space = *v.get(0x41)?;
                let units: Vec<u16> = v
                    .get(0x42..0x42 + nlen * 2)?
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                let short = space == 2;
                if out.name.is_empty() || (out.short && !short) {
                    out.parent = parent;
                    out.name = String::from_utf16_lossy(&units);
                    out.short = short;
                }
            }
            0x80 if name_len == 0 && !directory => {
                // Compressed, encrypted, sparse: not read raw.
                if aflags & (0x0001 | 0x4000 | 0x8000) != 0 {
                    out.data = Data::Hard;
                } else if !nonresident {
                    let vlen = u32_at(rec, at + 0x10)? as usize;
                    let voff = u16_at(rec, at + 0x14)? as usize;
                    out.size = vlen as u64;
                    if !matches!(out.data, Data::Hard) {
                        out.data = Data::Resident(rec.get(at + voff..at + voff + vlen)?.to_vec());
                    }
                } else {
                    let start_vcn = u64_at(rec, at + 0x10)?;
                    let roff = u16_at(rec, at + 0x20)? as usize;
                    let real = u64_at(rec, at + 0x30)?;
                    let init = u64_at(rec, at + 0x38)?;
                    out.size = real;
                    if start_vcn != 0 || init < real {
                        out.data = Data::Hard;
                    } else if !matches!(out.data, Data::Hard) {
                        out.data = runs(rec.get(at + roff..at + len)?, cluster)
                            .map_or(Data::Hard, Data::Runs);
                    }
                }
            }
            _ => {}
        }
        at += len;
    }
    Some(out)
}

/// A run list: (VCN, LCN, clusters) of every placed run. None if it holds
/// a sparse run or does not parse.
fn runs(mut b: &[u8], _cluster: u64) -> Option<Vec<(u64, u64, u64)>> {
    let (mut out, mut vcn, mut lcn) = (Vec::new(), 0u64, 0i64);
    loop {
        let head = *b.first()?;
        if head == 0 {
            return Some(out);
        }
        let (ls, os) = ((head & 0x0F) as usize, (head >> 4) as usize);
        if ls == 0 || ls > 8 || os > 8 || b.len() < 1 + ls + os {
            return None;
        }
        let mut len = 0u64;
        for k in 0..ls {
            len |= (b[1 + k] as u64) << (8 * k);
        }
        if os == 0 {
            return None;
        }
        let mut off = 0i64;
        for k in 0..os {
            off |= (b[1 + ls + k] as i64) << (8 * k);
        }
        // Sign-extend from the field's width.
        let shift = 64 - 8 * os as u32;
        off = (off << shift) >> shift;
        lcn += off;
        if lcn < 0 {
            return None;
        }
        out.push((vcn, lcn as u64, len));
        vcn += len;
        b = &b[1 + ls + os..];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_lists_decode_with_signed_offsets() {
        // 0x21: one byte of length, two of offset — 0x18 clusters at 0x5634;
        // then 0x11: 0x10 clusters at -0x10 from there.
        let b = [0x21, 0x18, 0x34, 0x56, 0x11, 0x10, 0xF0, 0x00];
        let r = runs(&b, 4096).unwrap();
        assert_eq!(r, vec![(0, 0x5634, 0x18), (0x18, 0x5624, 0x10)]);
        // A sparse run: no offset field.
        assert!(runs(&[0x01, 0x05, 0x00], 4096).is_none());
    }
}
