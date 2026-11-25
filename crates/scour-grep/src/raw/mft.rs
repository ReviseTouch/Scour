//! An MFT record, read from the bytes of the MFT itself: what a file's name,
//! parent, times, attributes and data say, without the file being opened.
//! Anything this reader does not follow — an attribute list, compressed,
//! encrypted or sparse data, an initialized size short of the real one, a
//! torn record — leaves the file to be read the normal way.

/// The root directory's record.
pub(crate) const ROOT: u64 = 5;
/// `$Secure`, whose `$SDS` stream holds every security descriptor.
pub(crate) const SECURE: u64 = 9;

/// A record of a file or a directory in use, as far as it matters here.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Record {
    pub lsn: u64,
    pub seq: u16,
    pub directory: bool,
    /// The parent's record number and sequence number.
    pub parent: u64,
    pub parent_seq: u16,
    pub name: String,
    /// The name is the DOS short one: kept only if there is no other.
    short: bool,
    /// `$STANDARD_INFORMATION`: written and changed, as FILETIMEs.
    pub modified: i64,
    pub changed: i64,
    pub attributes: u32,
    pub security_id: Option<u32>,
    pub size: u64,
    pub data: Data,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) enum Data {
    /// No unnamed `$DATA` seen: a directory, or nothing to read.
    #[default]
    None,
    Resident(Vec<u8>),
    /// (VCN, LCN, clusters) of every run.
    Runs(Vec<(u64, u64, u64)>),
    /// Placed where this reader does not follow: read the normal way.
    Hard,
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

const REF: u64 = 0x0000_FFFF_FFFF_FFFF;

/// The update sequence put back: each sector's last two bytes stand in the
/// array at the record's start. False when a sector does not end in the
/// sequence number — a record torn by a write — which is then not used.
pub(crate) fn fixup(rec: &mut [u8]) -> bool {
    let (Some(off), Some(count)) = (u16_at(rec, 4), u16_at(rec, 6)) else {
        return false;
    };
    let (off, count) = (off as usize, count as usize);
    if count < 2 || off + count * 2 > rec.len() || (count - 1) * 512 > rec.len() {
        return false;
    }
    let usn = [rec[off], rec[off + 1]];
    for s in 1..count {
        let end = s * 512;
        if rec[end - 2..end] != usn {
            return false;
        }
        rec[end - 2] = rec[off + s * 2];
        rec[end - 1] = rec[off + s * 2 + 1];
    }
    true
}

/// One record: in use and a base record, or None.
pub(crate) fn parse(rec: &mut [u8]) -> Option<Record> {
    if rec.get(0..4)? != b"FILE" || !fixup(rec) {
        return None;
    }
    let flags = u16_at(rec, 0x16)?;
    if flags & 1 == 0 || u64_at(rec, 0x20)? & REF != 0 {
        return None;
    }
    let used = (u32_at(rec, 0x18)? as usize).min(rec.len());
    let mut out = Record {
        lsn: u64_at(rec, 0x08)?,
        seq: u16_at(rec, 0x10)?,
        directory: flags & 2 != 0,
        ..Record::default()
    };
    let mut at = u16_at(rec, 0x14)? as usize;
    while at + 16 <= used {
        let kind = u32_at(rec, at)?;
        if kind == 0xFFFF_FFFF {
            break;
        }
        let len = u32_at(rec, at + 4)? as usize;
        if len < 16 || at + len > used {
            return None;
        }
        let attr = &rec[at..at + len];
        attribute(&mut out, kind, attr);
        at += len;
    }
    Some(out)
}

fn attribute(out: &mut Record, kind: u32, a: &[u8]) -> Option<()> {
    let nonresident = a[8] != 0;
    let name_len = a[9] as usize;
    let aflags = u16_at(a, 0x0C)?;
    let value = || -> Option<&[u8]> {
        let vlen = u32_at(a, 0x10)? as usize;
        let voff = u16_at(a, 0x14)? as usize;
        a.get(voff..voff + vlen)
    };
    match kind {
        0x10 if !nonresident => {
            let v = value()?;
            out.modified = u64_at(v, 0x08)? as i64;
            out.changed = u64_at(v, 0x10)? as i64;
            out.attributes = u32_at(v, 0x20)?;
            out.security_id = u32_at(v, 0x34);
        }
        // An attribute list: the data may stand in another record.
        0x20 => out.data = Data::Hard,
        0x30 if !nonresident => {
            let v = value()?;
            let parent = u64_at(v, 0)?;
            let nlen = *v.get(0x40)? as usize;
            let space = *v.get(0x41)?;
            let units: Vec<u16> = v
                .get(0x42..0x42 + nlen * 2)?
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect();
            // 2 is the DOS namespace: a short name beside a long one.
            let short = space == 2;
            if out.name.is_empty() || (out.short && !short) {
                out.parent = parent & REF;
                out.parent_seq = (parent >> 48) as u16;
                out.name = String::from_utf16_lossy(&units);
                out.short = short;
            }
        }
        0x80 if name_len == 0 && !out.directory => {
            // Compressed (the low byte), encrypted, sparse.
            if aflags & (0x00FF | 0x4000 | 0x8000) != 0 || matches!(out.data, Data::Hard) {
                out.data = Data::Hard;
                return Some(());
            }
            if !nonresident {
                let v = value()?;
                out.size = v.len() as u64;
                out.data = Data::Resident(v.to_vec());
            } else {
                let start_vcn = u64_at(a, 0x10)?;
                let roff = u16_at(a, 0x20)? as usize;
                let real = u64_at(a, 0x30)?;
                let init = u64_at(a, 0x38)?;
                out.size = real;
                out.data = if start_vcn != 0 || init < real {
                    Data::Hard
                } else {
                    a.get(roff..).and_then(runs).map_or(Data::Hard, Data::Runs)
                };
            }
        }
        _ => {}
    }
    Some(())
}

/// The runs of a named `$DATA` stream in this record — `$SDS` in `$Secure`'s —
/// with its real size; None when it is resident, absent or not followed.
pub(crate) fn named_runs(rec: &mut [u8], stream: &str) -> Option<(Vec<(u64, u64, u64)>, u64)> {
    if rec.get(0..4)? != b"FILE" || !fixup(rec) {
        return None;
    }
    let used = (u32_at(rec, 0x18)? as usize).min(rec.len());
    let want: Vec<u16> = stream.encode_utf16().collect();
    let mut at = u16_at(rec, 0x14)? as usize;
    while at + 16 <= used {
        let kind = u32_at(rec, at)?;
        let len = u32_at(rec, at + 4)? as usize;
        if kind == 0xFFFF_FFFF || len < 16 || at + len > used {
            return None;
        }
        let a = &rec[at..at + len];
        if kind == 0x20 {
            return None;
        }
        let name_len = a[9] as usize;
        let name_off = u16_at(a, 0x0A)? as usize;
        if kind == 0x80 && a[8] != 0 && name_len == want.len() {
            let name: Vec<u16> = a
                .get(name_off..name_off + name_len * 2)?
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect();
            if name == want && u64_at(a, 0x10)? == 0 {
                let roff = u16_at(a, 0x20)? as usize;
                let real = u64_at(a, 0x30)?;
                return Some((runs(a.get(roff..)?)?, real));
            }
        }
        at += len;
    }
    None
}

/// Where the MFT itself lies, from its own record — number 0 — and, when
/// it is in so many pieces that the record holds an attribute list, from the
/// records that list names, read through `record` once their place is known
/// (a large system disk's MFT is like that). None when they cannot be had,
/// or do not join up.
pub(crate) fn mft_runs(
    zero: &mut [u8],
    record: &mut dyn FnMut(u64, &[(u64, u64, u64)]) -> Option<Vec<u8>>,
) -> Option<Vec<(u64, u64, u64)>> {
    if zero.get(0..4)? != b"FILE" || !fixup(zero) {
        return None;
    }
    let mut pieces = data_pieces(zero)?;
    let mut others: Vec<u64> = Vec::new();
    for a in attributes(zero)? {
        if u32_at(a, 0)? == 0x20 {
            // A list held elsewhere: not followed.
            if a[8] != 0 {
                return None;
            }
            let voff = u16_at(a, 0x14)? as usize;
            let vlen = u32_at(a, 0x10)? as usize;
            let mut list = a.get(voff..voff + vlen)?;
            while list.len() >= 0x1A {
                let len = u16_at(list, 4)? as usize;
                if len < 0x1A || len > list.len() {
                    break;
                }
                let n = u64_at(list, 0x10)? & REF;
                if u32_at(list, 0)? == 0x80 && list[6] == 0 && n != 0 && !others.contains(&n) {
                    others.push(n);
                }
                list = &list[len..];
            }
        }
    }
    for n in others {
        let mut rec = record(n, &prefix(&pieces))?;
        if rec.get(0..4)? != b"FILE" || !fixup(&mut rec) {
            return None;
        }
        pieces.extend(data_pieces(&rec)?);
    }
    // Every piece joined to the one before it, or the MFT is not known.
    let all = prefix(&pieces);
    let mut distinct = pieces;
    distinct.sort_unstable();
    distinct.dedup();
    (!all.is_empty() && all.len() == distinct.len()).then_some(all)
}

/// The pieces of the unnamed `$DATA` a fixed-up record holds: their runs.
fn data_pieces(rec: &[u8]) -> Option<Vec<(u64, u64, u64)>> {
    let mut out = Vec::new();
    for a in attributes(rec)? {
        if u32_at(a, 0)? == 0x80 && a[8] != 0 && a[9] == 0 {
            let start = u64_at(a, 0x10)?;
            let roff = u16_at(a, 0x20)? as usize;
            out.extend(runs_from(a.get(roff..)?, start)?);
        }
    }
    Some(out)
}

/// The runs in VCN order from the first, as far as they join up: where
/// what is known so far lies.
fn prefix(pieces: &[(u64, u64, u64)]) -> Vec<(u64, u64, u64)> {
    let mut all = pieces.to_vec();
    all.sort_unstable();
    all.dedup();
    let mut next = 0;
    all.into_iter()
        .take_while(|&(vcn, _, n)| {
            let joins = vcn == next;
            next = vcn + n;
            joins
        })
        .collect()
}

/// Every attribute of a fixed-up record, whole.
fn attributes(rec: &[u8]) -> Option<Vec<&[u8]>> {
    let used = (u32_at(rec, 0x18)? as usize).min(rec.len());
    let mut at = u16_at(rec, 0x14)? as usize;
    let mut out = Vec::new();
    while at + 16 <= used {
        if u32_at(rec, at)? == 0xFFFF_FFFF {
            break;
        }
        let len = u32_at(rec, at + 4)? as usize;
        if len < 16 || at + len > used {
            return None;
        }
        out.push(&rec[at..at + len]);
        at += len;
    }
    Some(out)
}

/// A run list: (VCN, LCN, clusters) of every run. None if it holds a sparse
/// run or does not parse.
pub(crate) fn runs(b: &[u8]) -> Option<Vec<(u64, u64, u64)>> {
    runs_from(b, 0)
}

/// The same for a list whose first run is at VCN `vcn`: a piece of an
/// attribute held across records.
fn runs_from(mut b: &[u8], vcn: u64) -> Option<Vec<(u64, u64, u64)>> {
    let (mut out, mut vcn, mut lcn) = (Vec::new(), vcn, 0i64);
    loop {
        let head = *b.first()?;
        if head == 0 {
            return Some(out);
        }
        let (ls, os) = ((head & 0x0F) as usize, (head >> 4) as usize);
        // No offset field is a sparse run.
        if ls == 0 || ls > 8 || os == 0 || os > 8 || b.len() < 1 + ls + os {
            return None;
        }
        let mut len = 0u64;
        for k in 0..ls {
            len |= (b[1 + k] as u64) << (8 * k);
        }
        let mut off = 0i64;
        for k in 0..os {
            off |= (b[1 + ls + k] as i64) << (8 * k);
        }
        // Sign-extended from the field's width.
        let shift = 64 - 8 * os as u32;
        off = (off << shift) >> shift;
        lcn = lcn.checked_add(off)?;
        if lcn < 0 || len == 0 {
            return None;
        }
        out.push((vcn, lcn as u64, len));
        vcn += len;
        b = &b[1 + ls + os..];
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn run_lists_decode_with_signed_offsets() {
        // 0x21: one byte of length, two of offset — 0x18 clusters at 0x5634;
        // then 0x11: 0x10 clusters at -0x10 from there.
        let b = [0x21, 0x18, 0x34, 0x56, 0x11, 0x10, 0xF0, 0x00];
        assert_eq!(
            runs(&b).unwrap(),
            vec![(0, 0x5634, 0x18), (0x18, 0x5624, 0x10)]
        );
        // A sparse run has no offset field.
        assert!(runs(&[0x01, 0x05, 0x00]).is_none());
    }

    /// A 1 KiB record with these attributes, its update sequence applied.
    pub(crate) fn record(flags: u16, attrs: &[Vec<u8>]) -> Vec<u8> {
        let mut r = vec![0u8; 1024];
        r[0..4].copy_from_slice(b"FILE");
        r[4..6].copy_from_slice(&0x30u16.to_le_bytes());
        r[6..8].copy_from_slice(&3u16.to_le_bytes());
        r[8..16].copy_from_slice(&77u64.to_le_bytes());
        r[0x10..0x12].copy_from_slice(&4u16.to_le_bytes());
        r[0x14..0x16].copy_from_slice(&0x38u16.to_le_bytes());
        r[0x16..0x18].copy_from_slice(&flags.to_le_bytes());
        let mut at = 0x38;
        for a in attrs {
            r[at..at + a.len()].copy_from_slice(a);
            at += a.len();
        }
        r[at..at + 4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
        r[0x18..0x1C].copy_from_slice(&((at + 8) as u32).to_le_bytes());
        // The update sequence: 0x0101 at each sector's end, the real bytes kept.
        r[0x30..0x32].copy_from_slice(&[1, 1]);
        for s in 1..3 {
            let end = s * 512;
            r[0x30 + s * 2] = r[end - 2];
            r[0x31 + s * 2] = r[end - 1];
            r[end - 2] = 1;
            r[end - 1] = 1;
        }
        r
    }

    pub(crate) fn resident(kind: u32, name: &str, value: &[u8]) -> Vec<u8> {
        let units: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let voff = (0x18 + units.len()).next_multiple_of(8);
        let len = (voff + value.len()).next_multiple_of(8);
        let mut a = vec![0u8; len];
        a[0..4].copy_from_slice(&kind.to_le_bytes());
        a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        a[9] = (units.len() / 2) as u8;
        a[0x0A..0x0C].copy_from_slice(&0x18u16.to_le_bytes());
        a[0x10..0x14].copy_from_slice(&(value.len() as u32).to_le_bytes());
        a[0x14..0x16].copy_from_slice(&(voff as u16).to_le_bytes());
        a[0x18..0x18 + units.len()].copy_from_slice(&units);
        a[voff..voff + value.len()].copy_from_slice(value);
        a
    }

    pub(crate) fn file_name(parent: u64, parent_seq: u16, name: &str) -> Vec<u8> {
        let mut v = vec![0u8; 0x42];
        v[0..8].copy_from_slice(&(parent | (parent_seq as u64) << 48).to_le_bytes());
        v[0x40] = name.encode_utf16().count() as u8;
        v[0x41] = 1;
        v.extend(name.encode_utf16().flat_map(u16::to_le_bytes));
        resident(0x30, "", &v)
    }

    pub(crate) fn standard(modified: i64, attributes: u32, security_id: u32) -> Vec<u8> {
        let mut v = vec![0u8; 0x48];
        v[0x08..0x10].copy_from_slice(&modified.to_le_bytes());
        v[0x10..0x18].copy_from_slice(&modified.to_le_bytes());
        v[0x20..0x24].copy_from_slice(&attributes.to_le_bytes());
        v[0x34..0x38].copy_from_slice(&security_id.to_le_bytes());
        resident(0x10, "", &v)
    }

    pub(crate) fn nonresident(
        name: &str,
        flags: u16,
        real: u64,
        init: u64,
        runs: &[u8],
    ) -> Vec<u8> {
        let units: Vec<u8> = name.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let roff = (0x40 + units.len()).next_multiple_of(8);
        let len = (roff + runs.len()).next_multiple_of(8);
        let mut a = vec![0u8; len];
        a[0..4].copy_from_slice(&0x80u32.to_le_bytes());
        a[4..8].copy_from_slice(&(len as u32).to_le_bytes());
        a[8] = 1;
        a[9] = (units.len() / 2) as u8;
        a[0x0A..0x0C].copy_from_slice(&0x40u16.to_le_bytes());
        a[0x0C..0x0E].copy_from_slice(&flags.to_le_bytes());
        a[0x20..0x22].copy_from_slice(&(roff as u16).to_le_bytes());
        a[0x30..0x38].copy_from_slice(&real.to_le_bytes());
        a[0x38..0x40].copy_from_slice(&init.to_le_bytes());
        a[0x40..0x40 + units.len()].copy_from_slice(&units);
        a[roff..roff + runs.len()].copy_from_slice(runs);
        a
    }

    #[test]
    fn a_file_record_says_its_name_times_and_runs() {
        let mut r = record(
            1,
            &[
                standard(1234, 0x20, 0x101),
                file_name(40, 3, "notes.txt"),
                nonresident("", 0, 9000, 9000, &[0x21, 0x03, 0x00, 0x10, 0x00]),
            ],
        );
        let got = parse(&mut r).unwrap();
        assert_eq!((got.lsn, got.seq, got.directory), (77, 4, false));
        assert_eq!(
            (got.parent, got.parent_seq, got.name.as_str()),
            (40, 3, "notes.txt")
        );
        assert_eq!(
            (got.modified, got.attributes, got.security_id),
            (1234, 0x20, Some(0x101))
        );
        assert_eq!(got.size, 9000);
        assert_eq!(got.data, Data::Runs(vec![(0, 0x1000, 3)]));
    }

    #[test]
    fn small_files_are_in_the_record() {
        let mut r = record(
            1,
            &[
                standard(1, 0x20, 1),
                file_name(5, 5, "a"),
                resident(0x80, "", b"hello"),
            ],
        );
        let got = parse(&mut r).unwrap();
        assert_eq!(got.data, Data::Resident(b"hello".to_vec()));
        assert_eq!(got.size, 5);
    }

    #[test]
    fn what_is_not_followed_is_left_to_the_normal_way() {
        let runs = [0x21, 0x03, 0x00, 0x10, 0x00];
        for (flags, real, init) in [
            (0x0001, 9, 9),
            (0x4000, 9, 9),
            (0x8000, 9, 9),
            (0, 9000, 100),
        ] {
            let mut r = record(
                1,
                &[
                    file_name(5, 5, "x"),
                    nonresident("", flags, real, init, &runs),
                ],
            );
            assert_eq!(parse(&mut r).unwrap().data, Data::Hard, "flags {flags:#x}");
        }
        let list = resident(0x20, "", &[0; 32]);
        let mut r = record(1, &[list, file_name(5, 5, "x"), resident(0x80, "", b"x")]);
        assert_eq!(parse(&mut r).unwrap().data, Data::Hard);
    }

    #[test]
    fn a_torn_record_or_one_not_in_use_says_nothing() {
        let mut r = record(1, &[file_name(5, 5, "x")]);
        r[1022] ^= 0xFF;
        assert!(parse(&mut r).is_none());
        let mut r = record(0, &[file_name(5, 5, "x")]);
        assert!(parse(&mut r).is_none());
    }

    #[test]
    fn an_mft_in_many_pieces_is_followed_through_its_attribute_list() {
        // Record 0: the first eight clusters at 0x1000, and a list naming
        // record 3 for the rest of `$DATA`.
        let mut entry = vec![0u8; 0x20];
        entry[0..4].copy_from_slice(&0x80u32.to_le_bytes());
        entry[4..6].copy_from_slice(&0x20u16.to_le_bytes());
        entry[0x08..0x10].copy_from_slice(&8u64.to_le_bytes());
        entry[0x10..0x18].copy_from_slice(&(3u64 | 1 << 48).to_le_bytes());
        let mut zero = record(
            1,
            &[
                resident(0x20, "", &entry),
                nonresident("", 0, 0x10000, 0x10000, &[0x21, 0x08, 0x00, 0x10, 0x00]),
            ],
        );
        // Record 3: eight more clusters, at 0x5000, from VCN 8.
        let mut more = nonresident("", 0, 0x10000, 0x10000, &[0x21, 0x08, 0x00, 0x50, 0x00]);
        more[0x10..0x18].copy_from_slice(&8u64.to_le_bytes());
        let three = record(1, &[more]);
        let mut asked = Vec::new();
        let got = mft_runs(&mut zero, &mut |n, known| {
            asked.push((n, known.to_vec()));
            Some(three.clone())
        });
        assert_eq!(got, Some(vec![(0, 0x1000, 8), (8, 0x5000, 8)]));
        // Record 3 was looked for where the first piece says it is.
        assert_eq!(asked, vec![(3, vec![(0, 0x1000, 8)])]);
        // A piece that does not join up: the MFT is not known.
        let mut zero = record(
            1,
            &[
                resident(0x20, "", &entry),
                nonresident("", 0, 0x10000, 0x10000, &[0x21, 0x08, 0x00, 0x10, 0x00]),
            ],
        );
        let mut far = nonresident("", 0, 0x10000, 0x10000, &[0x21, 0x08, 0x00, 0x50, 0x00]);
        far[0x10..0x18].copy_from_slice(&20u64.to_le_bytes());
        let far = record(1, &[far]);
        assert_eq!(mft_runs(&mut zero, &mut |_, _| Some(far.clone())), None);
    }

    #[test]
    fn the_security_stream_is_found_by_name() {
        let runs = [0x21, 0x08, 0x00, 0x20, 0x00];
        let mut r = record(
            1,
            &[
                file_name(5, 5, "$Secure"),
                nonresident("$SDH", 0, 1, 1, &runs),
                nonresident("$SDS", 0, 0x48000, 0x48000, &runs),
            ],
        );
        assert_eq!(
            named_runs(&mut r, "$SDS"),
            Some((vec![(0, 0x2000, 8)], 0x48000))
        );
    }
}
